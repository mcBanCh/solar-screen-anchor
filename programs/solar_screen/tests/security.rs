//! LiteSVM tests against the compiled SBF program (target/deploy/solar_screen.so).
//! Run: cargo build-sbf && cargo test -p solar_screen --test security
use anchor_lang::{InstructionData, ToAccountMetas};
use litesvm::LiteSVM;
use solana_sdk::{
    instruction::Instruction, native_token::LAMPORTS_PER_SOL, pubkey::Pubkey,
    signature::Keypair, signer::Signer, system_program, transaction::Transaction,
};
use solar_screen::{accounts as acc, instruction as ix, ID};

const JOB_AMOUNT: u64 = 20_000_000;

struct Env { svm: LiteSVM, authority: Keypair, treasury: Pubkey, config: Pubkey }

fn pda(seeds: &[&[u8]]) -> Pubkey { Pubkey::find_program_address(seeds, &ID).0 }

fn send(svm: &mut LiteSVM, ixn: Instruction, signers: &[&Keypair]) -> Result<(), String> {
    let tx = Transaction::new_signed_with_payer(&[ixn], Some(&signers[0].pubkey()), signers, svm.latest_blockhash());
    let r = svm.send_transaction(tx).map(|_| ()).map_err(|e| format!("{:?}", e.meta.logs));
    svm.expire_blockhash();
    r
}

fn setup() -> Env {
    let mut svm = LiteSVM::new();
    let so = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/deploy/solar_screen.so");
    svm.add_program_from_file(ID, so).expect("build with cargo build-sbf first");
    let authority = Keypair::new();
    svm.airdrop(&authority.pubkey(), 10 * LAMPORTS_PER_SOL).unwrap();
    let treasury = Keypair::new().pubkey();
    svm.airdrop(&treasury, LAMPORTS_PER_SOL).unwrap();
    let config = pda(&[b"config"]);
    let i = Instruction { program_id: ID,
        accounts: acc::Initialize { authority: authority.pubkey(), config, system_program: system_program::ID }.to_account_metas(None),
        data: ix::Initialize { treasury }.data() };
    send(&mut svm, i, &[&authority]).unwrap();
    Env { svm, authority, treasury, config }
}

fn new_node(e: &mut Env) -> (Keypair, Pubkey) {
    let op = Keypair::new();
    e.svm.airdrop(&op.pubkey(), LAMPORTS_PER_SOL).unwrap();
    let node = pda(&[b"node", op.pubkey().as_ref()]);
    let i = Instruction { program_id: ID,
        accounts: acc::RegisterNode { operator: op.pubkey(), node, system_program: system_program::ID }.to_account_metas(None),
        data: ix::RegisterNode { endpoint: "https://node.example".into() }.data() };
    send(&mut e.svm, i, &[&op]).unwrap();
    (op, node)
}

/// Creates a job, claims it with (op,node) and submits a result. Returns job PDA.
fn submitted_job(e: &mut Env, op: &Keypair, node: Pubkey, id: u64) -> (Keypair, Pubkey) {
    let payer = Keypair::new();
    e.svm.airdrop(&payer.pubkey(), LAMPORTS_PER_SOL).unwrap();
    let job = pda(&[b"job", payer.pubkey().as_ref(), &id.to_le_bytes()]);
    let i = Instruction { program_id: ID,
        accounts: acc::CreateJob { payer: payer.pubkey(), config: e.config, job, system_program: system_program::ID }.to_account_metas(None),
        data: ix::CreateJob { job_id: id, amount: JOB_AMOUNT, brief: "screen caller".into() }.data() };
    send(&mut e.svm, i, &[&payer]).unwrap();
    let i = Instruction { program_id: ID,
        accounts: acc::ClaimJob { operator: op.pubkey(), node, job }.to_account_metas(None),
        data: ix::ClaimJob {}.data() };
    send(&mut e.svm, i, &[op]).unwrap();
    let i = Instruction { program_id: ID,
        accounts: acc::SubmitResult { operator: op.pubkey(), node, job }.to_account_metas(None),
        data: ix::SubmitResult { result_hash: [7u8; 32], result_uri: "reports/1.json".into() }.data() };
    send(&mut e.svm, i, &[op]).unwrap();
    (payer, job)
}

fn settle_ix(e: &Env, settler: Pubkey, job: Pubkey, node: Pubkey, operator: Pubkey) -> Instruction {
    Instruction { program_id: ID,
        accounts: acc::Settle { settler, config: e.config, job, node, operator, treasury: e.treasury }.to_account_metas(None),
        data: ix::Settle {}.data() }
}

#[test]
fn happy_path_settles_95_5() {
    let mut e = setup();
    let (op, node) = new_node(&mut e);
    let (_p, job) = submitted_job(&mut e, &op, node, 1);
    let op_before = e.svm.get_balance(&op.pubkey()).unwrap();
    let tr_before = e.svm.get_balance(&e.treasury).unwrap();
    let i = settle_ix(&e, op.pubkey(), job, node, op.pubkey());
    send(&mut e.svm, i, &[&op]).unwrap();
    let fee = JOB_AMOUNT * 500 / 10_000;
    // operator also paid the tx fee (5000 lamports) as fee payer
    assert_eq!(e.svm.get_balance(&op.pubkey()).unwrap() + 5000 - op_before, JOB_AMOUNT - fee);
    assert_eq!(e.svm.get_balance(&e.treasury).unwrap() - tr_before, fee);
}

#[test]
fn authority_can_settle() {
    let mut e = setup();
    let (op, node) = new_node(&mut e);
    let (_p, job) = submitted_job(&mut e, &op, node, 2);
    let auth = e.authority.insecure_clone();
    let i = settle_ix(&e, auth.pubkey(), job, node, op.pubkey());
    send(&mut e.svm, i, &[&auth]).unwrap();
}

#[test]
fn random_signer_cannot_settle() {
    let mut e = setup();
    let (op, node) = new_node(&mut e);
    let (_p, job) = submitted_job(&mut e, &op, node, 3);
    let rando = Keypair::new();
    e.svm.airdrop(&rando.pubkey(), LAMPORTS_PER_SOL).unwrap();
    let i = settle_ix(&e, rando.pubkey(), job, node, op.pubkey());
    let err = send(&mut e.svm, i, &[&rando]).unwrap_err();
    assert!(err.contains("Unauthorized"), "{err}");
}

#[test]
fn cannot_redirect_payout_to_other_node() {
    let mut e = setup();
    let (op, node) = new_node(&mut e);
    let (_p, job) = submitted_job(&mut e, &op, node, 4);
    let (attacker, attacker_node) = new_node(&mut e);
    let i = settle_ix(&e, attacker.pubkey(), job, attacker_node, attacker.pubkey());
    let err = send(&mut e.svm, i, &[&attacker]).unwrap_err();
    assert!(err.contains("WrongNode"), "{err}");
}

#[test]
fn other_node_cannot_submit_result() {
    let mut e = setup();
    let (op, node) = new_node(&mut e);
    let payer = Keypair::new();
    e.svm.airdrop(&payer.pubkey(), LAMPORTS_PER_SOL).unwrap();
    let job = pda(&[b"job", payer.pubkey().as_ref(), &5u64.to_le_bytes()]);
    let i = Instruction { program_id: ID,
        accounts: acc::CreateJob { payer: payer.pubkey(), config: e.config, job, system_program: system_program::ID }.to_account_metas(None),
        data: ix::CreateJob { job_id: 5, amount: JOB_AMOUNT, brief: "x".into() }.data() };
    send(&mut e.svm, i, &[&payer]).unwrap();
    let i = Instruction { program_id: ID, accounts: acc::ClaimJob { operator: op.pubkey(), node, job }.to_account_metas(None), data: ix::ClaimJob {}.data() };
    send(&mut e.svm, i, &[&op]).unwrap();
    let (attacker, attacker_node) = new_node(&mut e);
    let i = Instruction { program_id: ID,
        accounts: acc::SubmitResult { operator: attacker.pubkey(), node: attacker_node, job }.to_account_metas(None),
        data: ix::SubmitResult { result_hash: [1u8; 32], result_uri: "fake".into() }.data() };
    let err = send(&mut e.svm, i, &[&attacker]).unwrap_err();
    assert!(err.contains("WrongNode"), "{err}");
}

#[test]
fn only_payer_can_cancel() {
    let mut e = setup();
    let payer = Keypair::new();
    e.svm.airdrop(&payer.pubkey(), LAMPORTS_PER_SOL).unwrap();
    let job = pda(&[b"job", payer.pubkey().as_ref(), &6u64.to_le_bytes()]);
    let i = Instruction { program_id: ID,
        accounts: acc::CreateJob { payer: payer.pubkey(), config: e.config, job, system_program: system_program::ID }.to_account_metas(None),
        data: ix::CreateJob { job_id: 6, amount: JOB_AMOUNT, brief: "x".into() }.data() };
    send(&mut e.svm, i, &[&payer]).unwrap();
    let thief = Keypair::new();
    e.svm.airdrop(&thief.pubkey(), LAMPORTS_PER_SOL).unwrap();
    let i = Instruction { program_id: ID, accounts: acc::CancelJob { payer: thief.pubkey(), job }.to_account_metas(None), data: ix::CancelJob {}.data() };
    assert!(send(&mut e.svm, i, &[&thief]).is_err());
    let i = Instruction { program_id: ID, accounts: acc::CancelJob { payer: payer.pubkey(), job }.to_account_metas(None), data: ix::CancelJob {}.data() };
    send(&mut e.svm, i, &[&payer]).unwrap();
}
