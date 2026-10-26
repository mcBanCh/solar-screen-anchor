import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair, SystemProgram, LAMPORTS_PER_SOL } from "@solana/web3.js";
import { createHash } from "crypto";
import { assert } from "chai";

const MIN_STAKE = 50_000_000;
const MIN_JOB = 10_000_000;
const JOB_AMOUNT = 20_000_000; // 0.02 SOL — demo price

function statusName(status: unknown): string {
  if (status && typeof status === "object") {
    return Object.keys(status as object)[0] ?? "";
  }
  return String(status);
}

describe("solar_screen", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.SolarScreen as Program;
  const connection = provider.connection;
  const authority = provider.wallet as anchor.Wallet;

  const treasury = Keypair.generate();
  const operator = Keypair.generate();
  const payer = Keypair.generate();

  const jobId = new anchor.BN(1);

  const [configPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("config")],
    program.programId
  );
  const [nodePda] = PublicKey.findProgramAddressSync(
    [Buffer.from("node"), operator.publicKey.toBuffer()],
    program.programId
  );
  const [jobPda] = PublicKey.findProgramAddressSync(
    [Buffer.from("job"), payer.publicKey.toBuffer(), jobId.toArrayLike(Buffer, "le", 8)],
    program.programId
  );

  async function fund(pubkey: PublicKey, sol = 2) {
    const sig = await connection.requestAirdrop(pubkey, sol * LAMPORTS_PER_SOL);
    await connection.confirmTransaction(sig, "confirmed");
  }

  it("initialize config", async () => {
    await fund(authority.publicKey, 2);
    await program.methods
      .initialize(treasury.publicKey)
      .accounts({
        authority: authority.publicKey,
        config: configPda,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    const config = await program.account.config.fetch(configPda);
    assert.ok(config.authority.equals(authority.publicKey));
    assert.ok(config.treasury.equals(treasury.publicKey));
    assert.equal(config.feeBps.toNumber(), 500);
    assert.equal(config.jobCount.toNumber(), 0);
  });

  it("register_node stakes 0.05 SOL on the node PDA", async () => {
    await fund(operator.publicKey, 2);
    const before = await connection.getBalance(operator.publicKey);

    await program.methods
      .registerNode("gpu://solar-node-01")
      .accounts({
        operator: operator.publicKey,
        node: nodePda,
        systemProgram: SystemProgram.programId,
      })
      .signers([operator])
      .rpc();

    const node = await program.account.node.fetch(nodePda);
    assert.ok(node.operator.equals(operator.publicKey));
    assert.equal(node.endpoint, "gpu://solar-node-01");
    assert.equal(node.stakeLamports.toNumber(), MIN_STAKE);
    assert.equal(node.active, true);

    const after = await connection.getBalance(operator.publicKey);
    assert.isBelow(after, before - MIN_STAKE + 20_000);
  });

  it("create_job escrows 0.02 SOL then claim → submit → settle 95/5", async () => {
    await fund(payer.publicKey, 2);
    await fund(treasury.publicKey, 1);

    const treasuryBefore = await connection.getBalance(treasury.publicKey);
    const operatorBefore = await connection.getBalance(operator.publicKey);

    await program.methods
      .createJob(jobId, new anchor.BN(JOB_AMOUNT), "screen unknown caller")
      .accounts({
        payer: payer.publicKey,
        config: configPda,
        job: jobPda,
        systemProgram: SystemProgram.programId,
      })
      .signers([payer])
      .rpc();

    let job = await program.account.job.fetch(jobPda);
    assert.equal(job.id.toNumber(), 1);
    assert.ok(job.payer.equals(payer.publicKey));
    assert.equal(job.amount.toNumber(), JOB_AMOUNT);
    assert.equal(statusName(job.status), "open");

    const config = await program.account.config.fetch(configPda);
    assert.equal(config.jobCount.toNumber(), 1);

    await program.methods
      .claimJob()
      .accounts({
        operator: operator.publicKey,
        node: nodePda,
        job: jobPda,
      })
      .signers([operator])
      .rpc();

    job = await program.account.job.fetch(jobPda);
    assert.equal(statusName(job.status), "claimed");
    assert.ok(job.node.equals(nodePda));

    const report = JSON.stringify({
      caller_name: "Andrii Koval",
      intent: "delivery window",
      action: "callback",
    });
    const hash = Array.from(createHash("sha256").update(report).digest());

    await program.methods
      .submitResult(hash, "reports/demo.json")
      .accounts({
        operator: operator.publicKey,
        node: nodePda,
        job: jobPda,
      })
      .signers([operator])
      .rpc();

    job = await program.account.job.fetch(jobPda);
    assert.equal(statusName(job.status), "submitted");

    await program.methods
      .settle()
      .accounts({
        settler: authority.publicKey,
        config: configPda,
        job: jobPda,
        node: nodePda,
        operator: operator.publicKey,
        treasury: treasury.publicKey,
      })
      .rpc();

    job = await program.account.job.fetch(jobPda);
    assert.equal(statusName(job.status), "settled");

    const fee = Math.floor((JOB_AMOUNT * 500) / 10_000);
    const nodeShare = JOB_AMOUNT - fee;
    const treasuryAfter = await connection.getBalance(treasury.publicKey);
    const operatorAfter = await connection.getBalance(operator.publicKey);

    assert.equal(treasuryAfter - treasuryBefore, fee);
    assert.equal(operatorAfter - operatorBefore, nodeShare);

    const node = await program.account.node.fetch(nodePda);
    assert.equal(node.jobsCompleted.toNumber(), 1);
  });

  it("cancel_job refunds while Open", async () => {
    const other = Keypair.generate();
    await fund(other.publicKey, 1);
    const cancelId = new anchor.BN(99);
    const [cancelJob] = PublicKey.findProgramAddressSync(
      [Buffer.from("job"), other.publicKey.toBuffer(), cancelId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    await program.methods
      .createJob(cancelId, new anchor.BN(MIN_JOB), "cancel me")
      .accounts({
        payer: other.publicKey,
        config: configPda,
        job: cancelJob,
        systemProgram: SystemProgram.programId,
      })
      .signers([other])
      .rpc();

    const before = await connection.getBalance(other.publicKey);
    await program.methods
      .cancelJob()
      .accounts({
        payer: other.publicKey,
        job: cancelJob,
      })
      .signers([other])
      .rpc();

    const job = await program.account.job.fetch(cancelJob);
    assert.equal(statusName(job.status), "cancelled");
    const after = await connection.getBalance(other.publicKey);
    assert.isAtLeast(after, before + MIN_JOB - 10_000);
  });

  it("rejects job below 0.01 SOL", async () => {
    const cheap = Keypair.generate();
    await fund(cheap.publicKey, 1);
    const cheapId = new anchor.BN(7);
    const [cheapJob] = PublicKey.findProgramAddressSync(
      [Buffer.from("job"), cheap.publicKey.toBuffer(), cheapId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    try {
      await program.methods
        .createJob(cheapId, new anchor.BN(1_000_000), "too small")
        .accounts({
          payer: cheap.publicKey,
          config: configPda,
          job: cheapJob,
          systemProgram: SystemProgram.programId,
        })
        .signers([cheap])
        .rpc();
      assert.fail("should reject dust job");
    } catch (err: unknown) {
      const msg = String(err);
      assert.match(msg, /JobTooSmall|0\.01 SOL|6000/i);
    }
  });
});
