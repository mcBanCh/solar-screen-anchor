use anchor_lang::prelude::*;
use anchor_lang::system_program;

declare_id!("5g3gKqHF5kKyjpwTy87FS1ntjVM46265BEjc6kHKauCs");

pub const PROTOCOL_FEE_BPS: u64 = 500; // 5%
pub const MIN_JOB_LAMPORTS: u64 = 10_000_000; // 0.01 SOL
pub const MIN_STAKE_LAMPORTS: u64 = 50_000_000; // 0.05 SOL
pub const CONFIG_SEED: &[u8] = b"config";
pub const NODE_SEED: &[u8] = b"node";
pub const JOB_SEED: &[u8] = b"job";

#[program]
pub mod solar_screen {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>, treasury: Pubkey) -> Result<()> {
        let config = &mut ctx.accounts.config;
        config.authority = ctx.accounts.authority.key();
        config.treasury = treasury;
        config.job_count = 0;
        config.fee_bps = PROTOCOL_FEE_BPS;
        config.bump = ctx.bumps.config;
        Ok(())
    }

    pub fn register_node(ctx: Context<RegisterNode>, endpoint: String) -> Result<()> {
        require!(endpoint.len() <= 128, ScreenError::EndpointTooLong);
        require!(
            ctx.accounts.operator.lamports() >= MIN_STAKE_LAMPORTS,
            ScreenError::InsufficientStake
        );

        // Stake sits as extra lamports on the Node PDA.
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.operator.to_account_info(),
                    to: ctx.accounts.node.to_account_info(),
                },
            ),
            MIN_STAKE_LAMPORTS,
        )?;

        let node = &mut ctx.accounts.node;
        node.operator = ctx.accounts.operator.key();
        node.endpoint = endpoint;
        node.stake_lamports = MIN_STAKE_LAMPORTS;
        node.jobs_completed = 0;
        node.active = true;
        node.bump = ctx.bumps.node;
        Ok(())
    }

    pub fn create_job(
        ctx: Context<CreateJob>,
        job_id: u64,
        amount: u64,
        brief: String,
    ) -> Result<()> {
        require!(amount >= MIN_JOB_LAMPORTS, ScreenError::JobTooSmall);
        require!(brief.len() <= 256, ScreenError::BriefTooLong);

        // Escrow lives as extra lamports on the Job PDA itself.
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                system_program::Transfer {
                    from: ctx.accounts.payer.to_account_info(),
                    to: ctx.accounts.job.to_account_info(),
                },
            ),
            amount,
        )?;

        let job = &mut ctx.accounts.job;
        job.id = job_id;
        job.payer = ctx.accounts.payer.key();
        job.node = Pubkey::default();
        job.amount = amount;
        job.brief = brief;
        job.result_hash = [0u8; 32];
        job.result_uri = String::new();
        job.status = JobStatus::Open;
        job.created_at = Clock::get()?.unix_timestamp;
        job.bump = ctx.bumps.job;

        ctx.accounts.config.job_count = ctx.accounts.config.job_count.saturating_add(1);
        Ok(())
    }

    pub fn claim_job(ctx: Context<ClaimJob>) -> Result<()> {
        let node = &ctx.accounts.node;
        require!(node.active, ScreenError::NodeInactive);
        require!(node.operator == ctx.accounts.operator.key(), ScreenError::NotNodeOperator);

        let job = &mut ctx.accounts.job;
        require!(job.status == JobStatus::Open, ScreenError::JobNotOpen);

        job.node = node.key();
        job.status = JobStatus::Claimed;
        Ok(())
    }

    pub fn submit_result(
        ctx: Context<SubmitResult>,
        result_hash: [u8; 32],
        result_uri: String,
    ) -> Result<()> {
        require!(result_uri.len() <= 200, ScreenError::UriTooLong);
        require!(result_hash != [0u8; 32], ScreenError::EmptyHash);

        let node = &ctx.accounts.node;
        require!(node.operator == ctx.accounts.operator.key(), ScreenError::NotNodeOperator);

        let job = &mut ctx.accounts.job;
        require!(job.status == JobStatus::Claimed, ScreenError::JobNotClaimed);
        require!(job.node == node.key(), ScreenError::WrongNode);

        job.result_hash = result_hash;
        job.result_uri = result_uri;
        job.status = JobStatus::Submitted;
        Ok(())
    }

    pub fn settle(ctx: Context<Settle>) -> Result<()> {
        let amount = ctx.accounts.job.amount;
        require!(
            ctx.accounts.job.status == JobStatus::Submitted,
            ScreenError::JobNotSubmitted
        );

        let fee = amount
            .checked_mul(ctx.accounts.config.fee_bps)
            .ok_or(ScreenError::Math)?
            .checked_div(10_000)
            .ok_or(ScreenError::Math)?;
        let node_share = amount.checked_sub(fee).ok_or(ScreenError::Math)?;

        // Move lamports out of the program-owned Job PDA.
        let job_info = ctx.accounts.job.to_account_info();
        let op_info = ctx.accounts.operator.to_account_info();
        let tr_info = ctx.accounts.treasury.to_account_info();
        let job_bal = job_info.lamports().checked_sub(amount).ok_or(ScreenError::Math)?;
        let op_bal = op_info.lamports().checked_add(node_share).ok_or(ScreenError::Math)?;
        **job_info.try_borrow_mut_lamports()? = job_bal;
        **op_info.try_borrow_mut_lamports()? = op_bal;
        if fee > 0 {
            let tr_bal = tr_info.lamports().checked_add(fee).ok_or(ScreenError::Math)?;
            **tr_info.try_borrow_mut_lamports()? = tr_bal;
        }

        ctx.accounts.node.jobs_completed = ctx.accounts.node.jobs_completed.saturating_add(1);
        ctx.accounts.job.status = JobStatus::Settled;
        Ok(())
    }

    pub fn cancel_job(ctx: Context<CancelJob>) -> Result<()> {
        require!(ctx.accounts.job.status == JobStatus::Open, ScreenError::JobNotOpen);
        let amount = ctx.accounts.job.amount;

        **ctx
            .accounts
            .job
            .to_account_info()
            .try_borrow_mut_lamports()? -= amount;
        **ctx
            .accounts
            .payer
            .to_account_info()
            .try_borrow_mut_lamports()? += amount;

        ctx.accounts.job.status = JobStatus::Cancelled;
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Open,
    Claimed,
    Submitted,
    Settled,
    Cancelled,
}

#[account]
pub struct Config {
    pub authority: Pubkey,
    pub treasury: Pubkey,
    pub job_count: u64,
    pub fee_bps: u64,
    pub bump: u8,
}

#[account]
pub struct Node {
    pub operator: Pubkey,
    pub endpoint: String,
    pub stake_lamports: u64,
    pub jobs_completed: u64,
    pub active: bool,
    pub bump: u8,
}

#[account]
pub struct Job {
    pub id: u64,
    pub payer: Pubkey,
    pub node: Pubkey,
    pub amount: u64,
    pub brief: String,
    pub result_hash: [u8; 32],
    pub result_uri: String,
    pub status: JobStatus,
    pub created_at: i64,
    pub bump: u8,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(
        init,
        payer = authority,
        space = 8 + 32 + 32 + 8 + 8 + 1,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, Config>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RegisterNode<'info> {
    #[account(mut)]
    pub operator: Signer<'info>,
    #[account(
        init,
        payer = operator,
        space = 8 + 32 + 4 + 128 + 8 + 8 + 1 + 1,
        seeds = [NODE_SEED, operator.key().as_ref()],
        bump
    )]
    pub node: Account<'info, Node>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(job_id: u64)]
pub struct CreateJob<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    #[account(
        init,
        payer = payer,
        space = 8 + 8 + 32 + 32 + 8 + 4 + 256 + 32 + 4 + 200 + 1 + 8 + 1,
        seeds = [JOB_SEED, payer.key().as_ref(), &job_id.to_le_bytes()],
        bump
    )]
    pub job: Account<'info, Job>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimJob<'info> {
    pub operator: Signer<'info>,
    #[account(
        seeds = [NODE_SEED, operator.key().as_ref()],
        bump = node.bump,
        has_one = operator @ ScreenError::NotNodeOperator,
        constraint = node.active @ ScreenError::NodeInactive
    )]
    pub node: Account<'info, Node>,
    #[account(mut, seeds = [JOB_SEED, job.payer.as_ref(), &job.id.to_le_bytes()], bump = job.bump)]
    pub job: Account<'info, Job>,
}

#[derive(Accounts)]
pub struct SubmitResult<'info> {
    pub operator: Signer<'info>,
    #[account(
        seeds = [NODE_SEED, operator.key().as_ref()],
        bump = node.bump,
        has_one = operator @ ScreenError::NotNodeOperator
    )]
    pub node: Account<'info, Node>,
    #[account(
        mut,
        seeds = [JOB_SEED, job.payer.as_ref(), &job.id.to_le_bytes()], bump = job.bump,
        has_one = node @ ScreenError::WrongNode
    )]
    pub job: Account<'info, Job>,
}

#[derive(Accounts)]
pub struct Settle<'info> {
    /// Only the node operator or the protocol authority may settle.
    #[account(
        constraint = settler.key() == node.operator || settler.key() == config.authority
            @ ScreenError::Unauthorized
    )]
    pub settler: Signer<'info>,
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, Config>,
    /// Job must belong to this node (prevents redirecting payout to another node).
    #[account(
        mut,
        seeds = [JOB_SEED, job.payer.as_ref(), &job.id.to_le_bytes()], bump = job.bump,
        has_one = node @ ScreenError::WrongNode
    )]
    pub job: Account<'info, Job>,
    #[account(mut, seeds = [NODE_SEED, operator.key().as_ref()], bump = node.bump)]
    pub node: Account<'info, Node>,
    /// CHECK: node operator wallet receiving SOL.
    #[account(mut, address = node.operator)]
    pub operator: AccountInfo<'info>,
    /// CHECK: protocol treasury.
    #[account(mut, address = config.treasury)]
    pub treasury: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct CancelJob<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(mut, seeds = [JOB_SEED, job.payer.as_ref(), &job.id.to_le_bytes()], bump = job.bump, has_one = payer @ ScreenError::NotPayer)]
    pub job: Account<'info, Job>,
}

#[error_code]
pub enum ScreenError {
    #[msg("Endpoint string too long")]
    EndpointTooLong,
    #[msg("Stake below minimum (0.05 SOL)")]
    InsufficientStake,
    #[msg("Job payment below minimum (0.01 SOL)")]
    JobTooSmall,
    #[msg("Brief too long")]
    BriefTooLong,
    #[msg("Result URI too long")]
    UriTooLong,
    #[msg("Empty result hash")]
    EmptyHash,
    #[msg("Node is not active")]
    NodeInactive,
    #[msg("Signer is not the node operator")]
    NotNodeOperator,
    #[msg("Job is not open")]
    JobNotOpen,
    #[msg("Job is not claimed")]
    JobNotClaimed,
    #[msg("Job is not submitted")]
    JobNotSubmitted,
    #[msg("Wrong node for this job")]
    WrongNode,
    #[msg("Signer is not the job payer")]
    NotPayer,
    #[msg("Signer may not settle this job")]
    Unauthorized,
    #[msg("Math overflow")]
    Math,
}
