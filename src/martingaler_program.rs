//! Executable Martingaler V1 account boundary.
//!
//! The current Anchor-v2 compatibility bridge fixes legacy `AccountInfo` data
//! slice lengths at instruction entry. Account creation is therefore an atomic
//! two-instruction transaction: a prepare instruction allocates the canonical
//! PDA and its matching initialize instruction enters with fresh views and
//! writes state.

use crate::martingaler_state::{
    account_body_matches, account_len, close_cash_position, close_nonpositive_claim_position,
    close_profitable_cash_position, close_profitable_claim_position, decode_account,
    decode_account_boxed, decode_account_header, initialize_account,
    initialize_queue_page_zero_account, open_cash_position_from_idle_guarded,
    open_claim_position_from_queue_guarded, overwrite_account, pop_fifo_head_with_claim,
    record_cash_deposit, resolve_bankrupt_claim_position, service_fifo_claim,
    transition_market_status, transition_pool_status, validate_queue_page_zero_account,
    withdraw_idle_cash, ClaimState, ControllerRiskState, HaircutConfig, Key32, MarketRiskState,
    MartingalerAccount, OraclePolicyState, OracleRelaySource, OracleRelayState, PoolState,
    PoolStatus, PositionSide, PositionState, QueuePageState, TradingSessionState,
    MAX_ORACLE_CONFIDENCE_BPS, MAX_ORACLE_STALENESS_SECS, NO_CLAIM_SEQUENCE, V1_MAX_LEVERAGE_X,
    V1_MAX_NET_PROFIT_PER_POSITION_QUOTE, V1_MAX_POSITION_NOTIONAL_QUOTE,
    V1_MAX_RESERVED_PROFIT_PER_CONTROLLER_QUOTE, V1_MIN_PAYOUT_FACTOR_BPS,
    V1_PAYOUT_FACTOR_SCALE_BPS, V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
};
use alloc::{boxed::Box, vec::Vec};
use percolator_martingaler_engine::martingaler::V1_QUOTE_DECIMALS;
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction as SolanaInstruction},
    log::sol_log_data,
    program::{get_return_data, invoke, invoke_signed},
    program_error::ProgramError,
    program_option::COption,
    program_pack::Pack,
    pubkey::Pubkey,
    rent::Rent,
    system_instruction, system_program,
    sysvar::{clock::Clock, Sysvar},
};
use spl_token::state::{Account as TokenAccount, AccountState, Mint};

pub const MARTINGALER_IX_MAGIC: [u8; 8] = *b"NINMIX01";
pub const MARTINGALER_IX_VERSION: u16 = 1;
pub const MARTINGALER_IX_PREFIX_LEN: usize = 11;
pub const POOL_IX_LEN: usize = 19;
pub const PREPARE_MARKET_IX_LEN: usize = 13;
pub const INITIALIZE_MARKET_IX_LEN: usize = 71;
pub const CONTROLLER_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN;
pub const INITIALIZE_VAULT_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN;
pub const DEPOSIT_CASH_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8;
pub const PREPARE_ORACLE_POLICY_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 2;
pub const INITIALIZE_ORACLE_POLICY_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 2 + 32 + 32 + 8 + 2;
pub const PREPARE_POSITION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8;
pub const OPEN_CASH_POSITION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8 + 2 + 1 + 8 + 8 + 2;
pub const CLOSE_CASH_POSITION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8;
pub const PREPARE_CLAIM_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8;
pub const PREPARE_QUEUE_PAGE_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8;
pub const CLOSE_PROFITABLE_CASH_POSITION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8 + 1;
pub const SERVICE_FIFO_HEAD_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN;
pub const POP_FIFO_HEAD_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 1;
pub const OPEN_CLAIM_POSITION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8 + 2 + 1 + 8 + 8 + 2;
pub const CLOSE_CLAIM_POSITION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8 + 1;
pub const RESOLVE_BANKRUPT_CLAIM_POSITION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8;
pub const SET_STATUS_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 1;
pub const WITHDRAW_IDLE_CASH_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8;
pub const PREPARE_ORACLE_RELAY_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 2 + 32;
pub const INITIALIZE_ORACLE_RELAY_IX_LEN: usize =
    MARTINGALER_IX_PREFIX_LEN + 2 + 32 + 32 + 1 + 8 + 8 + 2 + 2;
pub const UPDATE_ORACLE_RELAY_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8 + 8 + 8 + 8;
pub const REFRESH_PHOENIX_ORACLE_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 1 + 1;
pub const PREPARE_TRADING_SESSION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN;
pub const INITIALIZE_TRADING_SESSION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 32 + 8;
pub const REVOKE_TRADING_SESSION_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 8;
pub const SESSION_PREPARE_POSITION_IX_LEN: usize = PREPARE_POSITION_IX_LEN + 16;
pub const SESSION_OPEN_POSITION_IX_LEN: usize = OPEN_CASH_POSITION_IX_LEN + 16;
pub const SESSION_CLOSE_POSITION_IX_LEN: usize = CLOSE_CASH_POSITION_IX_LEN + 16;
pub const SESSION_CLOSE_PROFITABLE_POSITION_IX_LEN: usize =
    CLOSE_PROFITABLE_CASH_POSITION_IX_LEN + 16;
pub const GUARDED_OPEN_POSITION_IX_LEN: usize = OPEN_CASH_POSITION_IX_LEN + 2;
pub const SESSION_GUARDED_OPEN_POSITION_IX_LEN: usize = SESSION_OPEN_POSITION_IX_LEN + 2;
pub const UPDATE_LEVERAGE_LIMIT_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 2 + 2;
pub const UPDATE_RISK_LIMITS_IX_LEN: usize = MARTINGALER_IX_PREFIX_LEN + 2 + 2 + 8 * 6;
pub const MARTINGALER_IX_LEN: usize = POOL_IX_LEN;
pub const PREPARE_POOL_TAG: u8 = 0;
pub const INITIALIZE_POOL_TAG: u8 = 1;
pub const PREPARE_MARKET_TAG: u8 = 2;
pub const INITIALIZE_MARKET_TAG: u8 = 3;
pub const PREPARE_CONTROLLER_TAG: u8 = 4;
pub const INITIALIZE_CONTROLLER_TAG: u8 = 5;
pub const INITIALIZE_VAULT_TAG: u8 = 6;
pub const DEPOSIT_CASH_TAG: u8 = 7;
pub const PREPARE_ORACLE_POLICY_TAG: u8 = 8;
pub const INITIALIZE_ORACLE_POLICY_TAG: u8 = 9;
pub const PREPARE_POSITION_TAG: u8 = 10;
pub const OPEN_CASH_POSITION_TAG: u8 = 11;
pub const CLOSE_NONPOSITIVE_CASH_POSITION_TAG: u8 = 12;
pub const RESOLVE_BANKRUPT_CASH_POSITION_TAG: u8 = 13;
pub const PREPARE_CLAIM_TAG: u8 = 14;
pub const PREPARE_QUEUE_PAGE_TAG: u8 = 15;
pub const CLOSE_PROFITABLE_CASH_POSITION_TAG: u8 = 16;
pub const SERVICE_FIFO_HEAD_TAG: u8 = 17;
pub const POP_FIFO_HEAD_TAG: u8 = 18;
pub const OPEN_CLAIM_POSITION_TAG: u8 = 19;
pub const CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG: u8 = 20;
pub const RESOLVE_BANKRUPT_CLAIM_POSITION_TAG: u8 = 21;
pub const CLOSE_PROFITABLE_CLAIM_POSITION_TAG: u8 = 22;
pub const SET_POOL_STATUS_TAG: u8 = 23;
pub const SET_MARKET_STATUS_TAG: u8 = 24;
pub const WITHDRAW_IDLE_CASH_TAG: u8 = 25;
pub const PREPARE_ORACLE_RELAY_TAG: u8 = 26;
pub const INITIALIZE_ORACLE_RELAY_TAG: u8 = 27;
pub const UPDATE_ORACLE_RELAY_TAG: u8 = 28;
pub const REFRESH_PHOENIX_ORACLE_TAG: u8 = 29;
pub const SERVICE_FIFO_HEAD_INTERNAL_TAG: u8 = 30;
pub const PREPARE_TRADING_SESSION_TAG: u8 = 31;
pub const INITIALIZE_TRADING_SESSION_TAG: u8 = 32;
pub const REVOKE_TRADING_SESSION_TAG: u8 = 33;
pub const SESSION_PREPARE_POSITION_TAG: u8 = 34;
pub const SESSION_OPEN_CASH_POSITION_TAG: u8 = 35;
pub const SESSION_CLOSE_NONPOSITIVE_CASH_POSITION_TAG: u8 = 36;
pub const SESSION_PREPARE_CLAIM_TAG: u8 = 37;
pub const SESSION_CLOSE_PROFITABLE_CASH_POSITION_TAG: u8 = 38;
pub const SESSION_OPEN_CLAIM_POSITION_TAG: u8 = 39;
pub const SESSION_CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG: u8 = 40;
pub const SESSION_CLOSE_PROFITABLE_CLAIM_POSITION_TAG: u8 = 41;
pub const GUARDED_OPEN_CASH_POSITION_TAG: u8 = 42;
pub const GUARDED_OPEN_CLAIM_POSITION_TAG: u8 = 43;
pub const SESSION_GUARDED_OPEN_CASH_POSITION_TAG: u8 = 44;
pub const SESSION_GUARDED_OPEN_CLAIM_POSITION_TAG: u8 = 45;
pub const UPDATE_LEVERAGE_LIMIT_TAG: u8 = 46;
pub const UPDATE_RISK_LIMITS_TAG: u8 = 47;
pub const MARTINGALER_PROGRAM_ERROR_BASE: u32 = 31_000;
pub const MARTINGALER_HISTORY_MAGIC: [u8; 8] = *b"NINHST01";
pub const MARTINGALER_HISTORY_VERSION: u8 = 1;
pub const MARTINGALER_HISTORY_OPEN_KIND: u8 = 1;
pub const MARTINGALER_HISTORY_CLOSE_KIND: u8 = 2;
pub const MARTINGALER_HISTORY_OPEN_LEN: usize = 178;
pub const MARTINGALER_HISTORY_CLOSE_LEN: usize = 280;
pub const MARTINGALER_CLOSE_NONPOSITIVE: u8 = 1;
pub const MARTINGALER_CLOSE_PROFITABLE: u8 = 2;
pub const MARTINGALER_CLOSE_BANKRUPT: u8 = 3;

const POOL_SEED: &[u8] = b"martingaler_pool";
const QUEUE_SEED: &[u8] = b"martingaler_queue";
const VAULT_SEED: &[u8] = b"martingaler_vault";
const MARKET_SEED: &[u8] = b"martingaler_market";
const CONTROLLER_SEED: &[u8] = b"martingaler_controller";
const ORACLE_POLICY_SEED: &[u8] = b"martingaler_oracle";
const ORACLE_RELAY_SEED: &[u8] = b"martingaler_relay";
const TRADING_SESSION_SEED: &[u8] = b"martingaler_session";
const POSITION_SEED: &[u8] = b"martingaler_position";
const CLAIM_SEED: &[u8] = b"martingaler_claim";
const PAGE_ZERO: [u8; 8] = 0u64.to_le_bytes();
const POOL_ACCOUNT_COUNT: usize = 5;
const REGISTRATION_ACCOUNT_COUNT: usize = 4;
const INITIALIZE_VAULT_ACCOUNT_COUNT: usize = 8;
const DEPOSIT_CASH_ACCOUNT_COUNT: usize = 7;
const ORACLE_POLICY_ACCOUNT_COUNT: usize = 5;
const PREPARE_POSITION_ACCOUNT_COUNT: usize = 5;
const OPEN_CASH_BASE_ACCOUNT_COUNT: usize = 8;
const CLOSE_CASH_ACCOUNT_COUNT: usize = 12;
const BANKRUPTCY_BASE_ACCOUNT_COUNT: usize = 7;
const PREPARE_CLAIM_ACCOUNT_COUNT: usize = 4;
const PREPARE_QUEUE_PAGE_ACCOUNT_COUNT: usize = 4;
const PROFITABLE_CLOSE_BASE_ACCOUNT_COUNT: usize = 12;
const SERVICE_FIFO_HEAD_ACCOUNT_COUNT: usize = 9;
const SERVICE_FIFO_HEAD_INTERNAL_ACCOUNT_COUNT: usize = 4;
const PREPARE_TRADING_SESSION_ACCOUNT_COUNT: usize = 4;
const INITIALIZE_TRADING_SESSION_ACCOUNT_COUNT: usize = 5;
const REVOKE_TRADING_SESSION_ACCOUNT_COUNT: usize = 2;
const SESSION_AUTH_ACCOUNT_COUNT: usize = 2;
const POP_FIFO_HEAD_BASE_ACCOUNT_COUNT: usize = 3;
const OPEN_CLAIM_BASE_ACCOUNT_COUNT: usize = 10;
const CLAIM_CLOSE_BASE_ACCOUNT_COUNT: usize = 13;
const CLAIM_BANKRUPTCY_BASE_ACCOUNT_COUNT: usize = 8;
const SET_POOL_STATUS_ACCOUNT_COUNT: usize = 2;
const SET_MARKET_STATUS_ACCOUNT_COUNT: usize = 3;
const UPDATE_LEVERAGE_LIMIT_ACCOUNT_COUNT: usize = 3;
const UPDATE_RISK_LIMITS_ACCOUNT_COUNT: usize = 3;
const WITHDRAW_IDLE_CASH_ACCOUNT_COUNT: usize = 8;
const ORACLE_RELAY_REGISTRATION_ACCOUNT_COUNT: usize = 5;
const UPDATE_ORACLE_RELAY_ACCOUNT_COUNT: usize = 2;
const REFRESH_PHOENIX_FIXED_ACCOUNT_COUNT: usize = 7;
const MAX_PHOENIX_DYNAMIC_ACCOUNTS_PER_KIND: usize = 16;
const ORACLE_RELAY_FUTURE_TOLERANCE_SECS: i64 = 5;
const ORACLE_RELAY_MAX_INGRESS_AGE_SECS: i64 = 120;

pub const HAWKEYE_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("RiSeVw3ZjNfsaXPRb4mgaqYaEEt41pNNJoDvVh7pgQj");
pub const PHOENIX_MAINNET_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("EtrnLzgbS7nMMy5fbD42kXiUzGg8XQzJ972Xtk1cjWih");
pub const PHOENIX_BETA_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("phDEVv4w6BcfkLrLNeXr8HhhgQxnxziVGXpGPcaadMf");
pub const PHOENIX_MAINNET_GLOBAL_CONFIG: Pubkey =
    solana_program::pubkey!("2zskx2iyCvb6Stg7RBZkt1f6MrF4dpYtMG3yMvKwqtUZ");
pub const PHOENIX_BETA_GLOBAL_CONFIG: Pubkey =
    solana_program::pubkey!("3CkM38UaZW6nyTJku4ABE5jjS5AComQErrkd55LGTfxa");
pub const PHOENIX_VIEW_BBO_DISCRIMINANT: [u8; 8] = [55, 95, 35, 45, 83, 175, 18, 82];
pub const PHOENIX_VIEW_BBO_RETURN_MAGIC: u64 = 17_278_657_705_831_514_481;
pub const PHOENIX_HAWKEYE_RETURN_VERSION: u16 = 1;
const PHOENIX_VIEW_BBO_RETURN_LEN: usize = 64;
const PHOENIX_VIEW_BBO_HAS_BID: u8 = 1;
const PHOENIX_VIEW_BBO_HAS_ASK: u8 = 2;

const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

fn history_u64(value: u128) -> Result<u64, ProgramError> {
    u64::try_from(value).map_err(|_| BoundaryError::ArithmeticOverflow.into())
}

pub fn encode_martingaler_open_history_receipt(
    pool: &Pubkey,
    position: &Pubkey,
    state: &PositionState,
    max_net_profit_per_position: u128,
) -> Result<Vec<u8>, ProgramError> {
    let mut data = Vec::with_capacity(MARTINGALER_HISTORY_OPEN_LEN);
    data.extend_from_slice(&MARTINGALER_HISTORY_MAGIC);
    data.push(MARTINGALER_HISTORY_VERSION);
    data.push(MARTINGALER_HISTORY_OPEN_KIND);
    data.push(state.funding_kind);
    data.push(state.side);
    data.extend_from_slice(pool.as_ref());
    data.extend_from_slice(&state.controller.0);
    data.extend_from_slice(position.as_ref());
    data.extend_from_slice(&state.market_id.get().to_le_bytes());
    data.extend_from_slice(&state.position_id.get().to_le_bytes());
    data.extend_from_slice(&history_u64(state.collateral.get())?.to_le_bytes());
    data.extend_from_slice(&history_u64(state.notional.get())?.to_le_bytes());
    data.extend_from_slice(&state.leverage_x.get().to_le_bytes());
    data.extend_from_slice(&state.entry_price_e6.get().to_le_bytes());
    data.extend_from_slice(&state.secondary_entry_price_e6.get().to_le_bytes());
    let nominal_profit = core::cmp::min(state.notional.get(), max_net_profit_per_position);
    let payout_factor_bps = state
        .max_payable_profit
        .get()
        .checked_mul(u128::from(V1_PAYOUT_FACTOR_SCALE_BPS))
        .and_then(|value| value.checked_div(nominal_profit))
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    data.extend_from_slice(
        &u16::try_from(payout_factor_bps)
            .map_err(|_| BoundaryError::ArithmeticOverflow)?
            .to_le_bytes(),
    );
    data.extend_from_slice(&history_u64(state.max_payable_profit.get())?.to_le_bytes());
    data.extend_from_slice(&state.source_claim_sequence.get().to_le_bytes());
    if data.len() != MARTINGALER_HISTORY_OPEN_LEN {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(data)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MartingalerCloseReceiptAmounts {
    principal_refund: u64,
    gross_loss: u128,
    haircut_withheld: u128,
    win_fee: u128,
    profit_cap_withheld: u128,
    immediate_profit_paid: u64,
    queued_profit: u128,
    residual_bad_debt_added: u128,
    child_claim_sequence: u128,
}

fn signed_raw_pnl(position: &PositionState, exit_price_e6: u64) -> Result<i128, ProgramError> {
    use crate::martingaler_state::OracleCashPnl;
    let amount = crate::martingaler_state::oracle_cash_pnl(
        PositionSide::try_from(position.side)?,
        position.notional.get(),
        position.entry_price_e6.get(),
        exit_price_e6,
    )?;
    match amount {
        OracleCashPnl::Profit(value) => {
            i128::try_from(value).map_err(|_| BoundaryError::ArithmeticOverflow.into())
        }
        OracleCashPnl::Flat => Ok(0),
        OracleCashPnl::Loss(value) => i128::try_from(value)
            .map(|value| -value)
            .map_err(|_| BoundaryError::ArithmeticOverflow.into()),
    }
}

fn encode_martingaler_close_history_receipt(
    pool: &Pubkey,
    position: &Pubkey,
    state: &PositionState,
    close_kind: u8,
    exit_price_e6: u64,
    secondary_exit_price_e6: Option<u64>,
    amounts: MartingalerCloseReceiptAmounts,
) -> Result<Vec<u8>, ProgramError> {
    let mut data = Vec::with_capacity(MARTINGALER_HISTORY_CLOSE_LEN);
    data.extend_from_slice(&MARTINGALER_HISTORY_MAGIC);
    data.push(MARTINGALER_HISTORY_VERSION);
    data.push(MARTINGALER_HISTORY_CLOSE_KIND);
    data.push(state.funding_kind);
    data.push(state.side);
    data.push(close_kind);
    data.push(0);
    data.extend_from_slice(pool.as_ref());
    data.extend_from_slice(&state.controller.0);
    data.extend_from_slice(position.as_ref());
    data.extend_from_slice(&state.market_id.get().to_le_bytes());
    data.extend_from_slice(&state.position_id.get().to_le_bytes());
    data.extend_from_slice(&exit_price_e6.to_le_bytes());
    data.extend_from_slice(&secondary_exit_price_e6.unwrap_or(0).to_le_bytes());
    data.extend_from_slice(&signed_raw_pnl(state, exit_price_e6)?.to_le_bytes());
    data.extend_from_slice(&amounts.principal_refund.to_le_bytes());
    data.extend_from_slice(&amounts.gross_loss.to_le_bytes());
    data.extend_from_slice(&amounts.haircut_withheld.to_le_bytes());
    data.extend_from_slice(&amounts.win_fee.to_le_bytes());
    data.extend_from_slice(&amounts.profit_cap_withheld.to_le_bytes());
    data.extend_from_slice(&amounts.immediate_profit_paid.to_le_bytes());
    data.extend_from_slice(&amounts.queued_profit.to_le_bytes());
    data.extend_from_slice(&amounts.residual_bad_debt_added.to_le_bytes());
    data.extend_from_slice(&amounts.child_claim_sequence.to_le_bytes());
    if data.len() != MARTINGALER_HISTORY_CLOSE_LEN {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(data)
}

fn emit_martingaler_history(data: &[u8]) {
    sol_log_data(&[data]);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BoundaryError {
    InvalidInstruction = 1,
    InvalidAccountCount = 2,
    ExpectedSigner = 3,
    ExpectedWritable = 4,
    ExpectedReadonly = 5,
    DuplicateAccount = 6,
    InvalidProgram = 7,
    InvalidPda = 8,
    InvalidFreshAccount = 9,
    InvalidPreparedAccount = 10,
    InvalidMint = 11,
    InvalidQuoteDecimals = 12,
    ArithmeticOverflow = 13,
    PostconditionFailed = 14,
    InvalidPool = 15,
    InvalidAuthority = 16,
    InvalidMarketLimits = 17,
    InvalidVault = 18,
    InvalidTokenAccount = 19,
    CustodyMismatch = 20,
    InvalidTokenDelta = 21,
    InvalidMarket = 22,
    InvalidOraclePolicy = 23,
    InvalidOracle = 24,
    OracleDivergence = 25,
    InvalidPosition = 26,
    InvalidClaim = 27,
    InvalidQueuePage = 28,
    SettlementShapeMismatch = 29,
}

impl From<BoundaryError> for ProgramError {
    fn from(error: BoundaryError) -> Self {
        ProgramError::Custom(MARTINGALER_PROGRAM_ERROR_BASE + error as u32)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ProfitSettlementShape {
    NoClaim = 0,
    CurrentTail = 1,
    Rollover = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PopFifoShape {
    CurrentPageOrEmpty = 0,
    NextPage = 1,
}

impl TryFrom<u8> for PopFifoShape {
    type Error = ProgramError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::CurrentPageOrEmpty),
            1 => Ok(Self::NextPage),
            _ => Err(BoundaryError::InvalidInstruction.into()),
        }
    }
}

impl TryFrom<u8> for ProfitSettlementShape {
    type Error = ProgramError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::NoClaim),
            1 => Ok(Self::CurrentTail),
            2 => Ok(Self::Rollover),
            _ => Err(BoundaryError::InvalidInstruction.into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Instruction {
    PreparePool {
        risk_pool_id: u64,
    },
    InitializePool {
        risk_pool_id: u64,
    },
    PrepareMarket {
        market_id: u16,
    },
    InitializeMarket {
        market_id: u16,
        divergence_limit_bps: u16,
        max_long_open_interest: u128,
        max_short_open_interest: u128,
        haircut: HaircutConfig,
    },
    PrepareController,
    InitializeController,
    InitializeVault,
    DepositCash {
        amount: u64,
    },
    PrepareOraclePolicy {
        market_id: u16,
    },
    InitializeOraclePolicy {
        market_id: u16,
        primary_feed_id: [u8; 32],
        secondary_feed_id: [u8; 32],
        max_staleness_secs: u64,
        confidence_limit_bps: u16,
    },
    PreparePosition {
        position_id: u64,
    },
    OpenCashPosition {
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        collateral: u64,
        notional: u64,
        leverage_x: u16,
    },
    GuardedOpenCashPosition {
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        collateral: u64,
        notional: u64,
        leverage_x: u16,
        min_payout_factor_bps: u16,
    },
    CloseNonPositiveCashPosition {
        position_id: u64,
    },
    ResolveBankruptCashPosition {
        position_id: u64,
    },
    PrepareClaim {
        position_id: u64,
    },
    PrepareQueuePage {
        page_index: u64,
    },
    CloseProfitableCashPosition {
        position_id: u64,
        settlement_shape: ProfitSettlementShape,
    },
    ServiceFifoHead,
    PopFifoHead {
        shape: PopFifoShape,
    },
    OpenClaimPosition {
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        claim_amount: u64,
        notional: u64,
        leverage_x: u16,
    },
    GuardedOpenClaimPosition {
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        claim_amount: u64,
        notional: u64,
        leverage_x: u16,
        min_payout_factor_bps: u16,
    },
    CloseNonPositiveClaimPosition {
        position_id: u64,
        settlement_shape: ProfitSettlementShape,
    },
    ResolveBankruptClaimPosition {
        position_id: u64,
    },
    CloseProfitableClaimPosition {
        position_id: u64,
        settlement_shape: ProfitSettlementShape,
    },
    SetPoolStatus {
        target: PoolStatus,
    },
    SetMarketStatus {
        target: PoolStatus,
    },
    WithdrawIdleCash {
        amount: u64,
    },
    PrepareOracleRelay {
        market_id: u16,
        feed_id: [u8; 32],
    },
    InitializeOracleRelay {
        market_id: u16,
        feed_id: [u8; 32],
        authority: [u8; 32],
        source: OracleRelaySource,
        price_e6_per_tick: u64,
        max_source_staleness_slots: u64,
        max_spread_bps: u16,
        max_mark_index_divergence_bps: u16,
    },
    UpdateOracleRelay {
        price_e6: u64,
        confidence_e6: u64,
        publish_time: i64,
        source_posted_slot: u64,
    },
    RefreshPhoenixOracle {
        global_trader_index_count: u8,
        active_trader_buffer_count: u8,
    },
    ServiceFifoHeadInternal,
    PrepareTradingSession,
    InitializeTradingSession {
        session_key: [u8; 32],
        expected_epoch: u64,
    },
    RevokeTradingSession {
        expected_epoch: u64,
    },
    SessionPreparePosition {
        position_id: u64,
        epoch: u64,
        nonce: u64,
    },
    SessionOpenCashPosition {
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        collateral: u64,
        notional: u64,
        leverage_x: u16,
        epoch: u64,
        nonce: u64,
    },
    SessionGuardedOpenCashPosition {
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        collateral: u64,
        notional: u64,
        leverage_x: u16,
        min_payout_factor_bps: u16,
        epoch: u64,
        nonce: u64,
    },
    SessionCloseNonPositiveCashPosition {
        position_id: u64,
        epoch: u64,
        nonce: u64,
    },
    SessionPrepareClaim {
        position_id: u64,
        epoch: u64,
        nonce: u64,
    },
    SessionCloseProfitableCashPosition {
        position_id: u64,
        settlement_shape: ProfitSettlementShape,
        epoch: u64,
        nonce: u64,
    },
    SessionOpenClaimPosition {
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        claim_amount: u64,
        notional: u64,
        leverage_x: u16,
        epoch: u64,
        nonce: u64,
    },
    SessionGuardedOpenClaimPosition {
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        claim_amount: u64,
        notional: u64,
        leverage_x: u16,
        min_payout_factor_bps: u16,
        epoch: u64,
        nonce: u64,
    },
    SessionCloseNonPositiveClaimPosition {
        position_id: u64,
        settlement_shape: ProfitSettlementShape,
        epoch: u64,
        nonce: u64,
    },
    SessionCloseProfitableClaimPosition {
        position_id: u64,
        settlement_shape: ProfitSettlementShape,
        epoch: u64,
        nonce: u64,
    },
    UpdateLeverageLimit {
        market_id: u16,
        max_leverage_x: u16,
    },
    UpdateRiskLimits {
        market_id: u16,
        max_leverage_x: u16,
        max_position_notional: u64,
        max_net_profit_per_position: u64,
        max_reserved_profit_per_controller: u64,
        total_liability_open_gate: u64,
        max_long_open_interest: u64,
        max_short_open_interest: u64,
    },
}

impl Instruction {
    pub fn encode(self) -> Vec<u8> {
        let (tag, body_len) = match self {
            Self::PreparePool { .. } => (PREPARE_POOL_TAG, 8),
            Self::InitializePool { .. } => (INITIALIZE_POOL_TAG, 8),
            Self::PrepareMarket { .. } => (PREPARE_MARKET_TAG, 2),
            Self::InitializeMarket { .. } => (INITIALIZE_MARKET_TAG, 60),
            Self::PrepareController => (PREPARE_CONTROLLER_TAG, 0),
            Self::InitializeController => (INITIALIZE_CONTROLLER_TAG, 0),
            Self::InitializeVault => (INITIALIZE_VAULT_TAG, 0),
            Self::DepositCash { .. } => (DEPOSIT_CASH_TAG, 8),
            Self::PrepareOraclePolicy { .. } => (PREPARE_ORACLE_POLICY_TAG, 2),
            Self::InitializeOraclePolicy { .. } => (INITIALIZE_ORACLE_POLICY_TAG, 76),
            Self::PreparePosition { .. } => (PREPARE_POSITION_TAG, 8),
            Self::OpenCashPosition { .. } => (OPEN_CASH_POSITION_TAG, 29),
            Self::GuardedOpenCashPosition { .. } => (GUARDED_OPEN_CASH_POSITION_TAG, 31),
            Self::CloseNonPositiveCashPosition { .. } => (CLOSE_NONPOSITIVE_CASH_POSITION_TAG, 8),
            Self::ResolveBankruptCashPosition { .. } => (RESOLVE_BANKRUPT_CASH_POSITION_TAG, 8),
            Self::PrepareClaim { .. } => (PREPARE_CLAIM_TAG, 8),
            Self::PrepareQueuePage { .. } => (PREPARE_QUEUE_PAGE_TAG, 8),
            Self::CloseProfitableCashPosition { .. } => (CLOSE_PROFITABLE_CASH_POSITION_TAG, 9),
            Self::ServiceFifoHead => (SERVICE_FIFO_HEAD_TAG, 0),
            Self::PopFifoHead { .. } => (POP_FIFO_HEAD_TAG, 1),
            Self::OpenClaimPosition { .. } => (OPEN_CLAIM_POSITION_TAG, 29),
            Self::GuardedOpenClaimPosition { .. } => (GUARDED_OPEN_CLAIM_POSITION_TAG, 31),
            Self::CloseNonPositiveClaimPosition { .. } => (CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG, 9),
            Self::ResolveBankruptClaimPosition { .. } => (RESOLVE_BANKRUPT_CLAIM_POSITION_TAG, 8),
            Self::CloseProfitableClaimPosition { .. } => (CLOSE_PROFITABLE_CLAIM_POSITION_TAG, 9),
            Self::SetPoolStatus { .. } => (SET_POOL_STATUS_TAG, 1),
            Self::SetMarketStatus { .. } => (SET_MARKET_STATUS_TAG, 1),
            Self::WithdrawIdleCash { .. } => (WITHDRAW_IDLE_CASH_TAG, 8),
            Self::PrepareOracleRelay { .. } => (PREPARE_ORACLE_RELAY_TAG, 34),
            Self::InitializeOracleRelay { .. } => (INITIALIZE_ORACLE_RELAY_TAG, 87),
            Self::UpdateOracleRelay { .. } => (UPDATE_ORACLE_RELAY_TAG, 32),
            Self::RefreshPhoenixOracle { .. } => (REFRESH_PHOENIX_ORACLE_TAG, 2),
            Self::ServiceFifoHeadInternal => (SERVICE_FIFO_HEAD_INTERNAL_TAG, 0),
            Self::PrepareTradingSession => (PREPARE_TRADING_SESSION_TAG, 0),
            Self::InitializeTradingSession { .. } => (INITIALIZE_TRADING_SESSION_TAG, 40),
            Self::RevokeTradingSession { .. } => (REVOKE_TRADING_SESSION_TAG, 8),
            Self::SessionPreparePosition { .. } => (SESSION_PREPARE_POSITION_TAG, 24),
            Self::SessionOpenCashPosition { .. } => (SESSION_OPEN_CASH_POSITION_TAG, 45),
            Self::SessionGuardedOpenCashPosition { .. } => {
                (SESSION_GUARDED_OPEN_CASH_POSITION_TAG, 47)
            }
            Self::SessionCloseNonPositiveCashPosition { .. } => {
                (SESSION_CLOSE_NONPOSITIVE_CASH_POSITION_TAG, 24)
            }
            Self::SessionPrepareClaim { .. } => (SESSION_PREPARE_CLAIM_TAG, 24),
            Self::SessionCloseProfitableCashPosition { .. } => {
                (SESSION_CLOSE_PROFITABLE_CASH_POSITION_TAG, 25)
            }
            Self::SessionOpenClaimPosition { .. } => (SESSION_OPEN_CLAIM_POSITION_TAG, 45),
            Self::SessionGuardedOpenClaimPosition { .. } => {
                (SESSION_GUARDED_OPEN_CLAIM_POSITION_TAG, 47)
            }
            Self::SessionCloseNonPositiveClaimPosition { .. } => {
                (SESSION_CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG, 25)
            }
            Self::SessionCloseProfitableClaimPosition { .. } => {
                (SESSION_CLOSE_PROFITABLE_CLAIM_POSITION_TAG, 25)
            }
            Self::UpdateLeverageLimit { .. } => (UPDATE_LEVERAGE_LIMIT_TAG, 4),
            Self::UpdateRiskLimits { .. } => (UPDATE_RISK_LIMITS_TAG, 52),
        };
        let mut data = Vec::with_capacity(MARTINGALER_IX_PREFIX_LEN + body_len);
        data.extend_from_slice(&MARTINGALER_IX_MAGIC);
        data.extend_from_slice(&MARTINGALER_IX_VERSION.to_le_bytes());
        data.push(tag);
        match self {
            Self::PreparePool { risk_pool_id } | Self::InitializePool { risk_pool_id } => {
                data.extend_from_slice(&risk_pool_id.to_le_bytes());
            }
            Self::PrepareMarket { market_id } => {
                data.extend_from_slice(&market_id.to_le_bytes());
            }
            Self::InitializeMarket {
                market_id,
                divergence_limit_bps,
                max_long_open_interest,
                max_short_open_interest,
                haircut,
            } => {
                data.extend_from_slice(&market_id.to_le_bytes());
                data.extend_from_slice(&divergence_limit_bps.to_le_bytes());
                data.extend_from_slice(&max_long_open_interest.to_le_bytes());
                data.extend_from_slice(&max_short_open_interest.to_le_bytes());
                data.extend_from_slice(&haircut.base_rate_ppm.get().to_le_bytes());
                data.extend_from_slice(&haircut.deadband_move_ppm.get().to_le_bytes());
                data.extend_from_slice(&haircut.linear_coefficient_ppm.get().to_le_bytes());
                data.extend_from_slice(&haircut.quadratic_coefficient_ppm.get().to_le_bytes());
                data.extend_from_slice(&haircut.max_rate_ppm.get().to_le_bytes());
                data.extend_from_slice(&haircut.win_fee_ppm.get().to_le_bytes());
            }
            Self::DepositCash { amount } => data.extend_from_slice(&amount.to_le_bytes()),
            Self::PrepareOraclePolicy { market_id } => {
                data.extend_from_slice(&market_id.to_le_bytes());
            }
            Self::InitializeOraclePolicy {
                market_id,
                primary_feed_id,
                secondary_feed_id,
                max_staleness_secs,
                confidence_limit_bps,
            } => {
                data.extend_from_slice(&market_id.to_le_bytes());
                data.extend_from_slice(&primary_feed_id);
                data.extend_from_slice(&secondary_feed_id);
                data.extend_from_slice(&max_staleness_secs.to_le_bytes());
                data.extend_from_slice(&confidence_limit_bps.to_le_bytes());
            }
            Self::PreparePosition { position_id } => {
                data.extend_from_slice(&position_id.to_le_bytes());
            }
            Self::CloseNonPositiveCashPosition { position_id }
            | Self::ResolveBankruptCashPosition { position_id }
            | Self::PrepareClaim { position_id } => {
                data.extend_from_slice(&position_id.to_le_bytes());
            }
            Self::PrepareQueuePage { page_index } => {
                data.extend_from_slice(&page_index.to_le_bytes());
            }
            Self::CloseProfitableCashPosition {
                position_id,
                settlement_shape,
            }
            | Self::CloseNonPositiveClaimPosition {
                position_id,
                settlement_shape,
            }
            | Self::CloseProfitableClaimPosition {
                position_id,
                settlement_shape,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.push(settlement_shape as u8);
            }
            Self::ResolveBankruptClaimPosition { position_id } => {
                data.extend_from_slice(&position_id.to_le_bytes());
            }
            Self::SetPoolStatus { target } | Self::SetMarketStatus { target } => {
                data.push(target as u8);
            }
            Self::WithdrawIdleCash { amount } => data.extend_from_slice(&amount.to_le_bytes()),
            Self::PrepareOracleRelay { market_id, feed_id } => {
                data.extend_from_slice(&market_id.to_le_bytes());
                data.extend_from_slice(&feed_id);
            }
            Self::InitializeOracleRelay {
                market_id,
                feed_id,
                authority,
                source,
                price_e6_per_tick,
                max_source_staleness_slots,
                max_spread_bps,
                max_mark_index_divergence_bps,
            } => {
                data.extend_from_slice(&market_id.to_le_bytes());
                data.extend_from_slice(&feed_id);
                data.extend_from_slice(&authority);
                data.push(source as u8);
                data.extend_from_slice(&price_e6_per_tick.to_le_bytes());
                data.extend_from_slice(&max_source_staleness_slots.to_le_bytes());
                data.extend_from_slice(&max_spread_bps.to_le_bytes());
                data.extend_from_slice(&max_mark_index_divergence_bps.to_le_bytes());
            }
            Self::UpdateOracleRelay {
                price_e6,
                confidence_e6,
                publish_time,
                source_posted_slot,
            } => {
                data.extend_from_slice(&price_e6.to_le_bytes());
                data.extend_from_slice(&confidence_e6.to_le_bytes());
                data.extend_from_slice(&publish_time.to_le_bytes());
                data.extend_from_slice(&source_posted_slot.to_le_bytes());
            }
            Self::RefreshPhoenixOracle {
                global_trader_index_count,
                active_trader_buffer_count,
            } => {
                data.push(global_trader_index_count);
                data.push(active_trader_buffer_count);
            }
            Self::InitializeTradingSession {
                session_key,
                expected_epoch,
            } => {
                data.extend_from_slice(&session_key);
                data.extend_from_slice(&expected_epoch.to_le_bytes());
            }
            Self::RevokeTradingSession { expected_epoch } => {
                data.extend_from_slice(&expected_epoch.to_le_bytes());
            }
            Self::SessionPreparePosition {
                position_id,
                epoch,
                nonce,
            }
            | Self::SessionPrepareClaim {
                position_id,
                epoch,
                nonce,
            }
            | Self::SessionCloseNonPositiveCashPosition {
                position_id,
                epoch,
                nonce,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&epoch.to_le_bytes());
                data.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::SessionCloseProfitableCashPosition {
                position_id,
                settlement_shape,
                epoch,
                nonce,
            }
            | Self::SessionCloseNonPositiveClaimPosition {
                position_id,
                settlement_shape,
                epoch,
                nonce,
            }
            | Self::SessionCloseProfitableClaimPosition {
                position_id,
                settlement_shape,
                epoch,
                nonce,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.push(settlement_shape as u8);
                data.extend_from_slice(&epoch.to_le_bytes());
                data.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::SessionOpenCashPosition {
                position_id,
                market_id,
                side,
                collateral,
                notional,
                leverage_x,
                epoch,
                nonce,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&market_id.to_le_bytes());
                data.push(side as u8);
                data.extend_from_slice(&collateral.to_le_bytes());
                data.extend_from_slice(&notional.to_le_bytes());
                data.extend_from_slice(&leverage_x.to_le_bytes());
                data.extend_from_slice(&epoch.to_le_bytes());
                data.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::SessionGuardedOpenCashPosition {
                position_id,
                market_id,
                side,
                collateral,
                notional,
                leverage_x,
                min_payout_factor_bps,
                epoch,
                nonce,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&market_id.to_le_bytes());
                data.push(side as u8);
                data.extend_from_slice(&collateral.to_le_bytes());
                data.extend_from_slice(&notional.to_le_bytes());
                data.extend_from_slice(&leverage_x.to_le_bytes());
                data.extend_from_slice(&min_payout_factor_bps.to_le_bytes());
                data.extend_from_slice(&epoch.to_le_bytes());
                data.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::SessionOpenClaimPosition {
                position_id,
                market_id,
                side,
                claim_amount,
                notional,
                leverage_x,
                epoch,
                nonce,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&market_id.to_le_bytes());
                data.push(side as u8);
                data.extend_from_slice(&claim_amount.to_le_bytes());
                data.extend_from_slice(&notional.to_le_bytes());
                data.extend_from_slice(&leverage_x.to_le_bytes());
                data.extend_from_slice(&epoch.to_le_bytes());
                data.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::SessionGuardedOpenClaimPosition {
                position_id,
                market_id,
                side,
                claim_amount,
                notional,
                leverage_x,
                min_payout_factor_bps,
                epoch,
                nonce,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&market_id.to_le_bytes());
                data.push(side as u8);
                data.extend_from_slice(&claim_amount.to_le_bytes());
                data.extend_from_slice(&notional.to_le_bytes());
                data.extend_from_slice(&leverage_x.to_le_bytes());
                data.extend_from_slice(&min_payout_factor_bps.to_le_bytes());
                data.extend_from_slice(&epoch.to_le_bytes());
                data.extend_from_slice(&nonce.to_le_bytes());
            }
            Self::PopFifoHead { shape } => data.push(shape as u8),
            Self::OpenCashPosition {
                position_id,
                market_id,
                side,
                collateral,
                notional,
                leverage_x,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&market_id.to_le_bytes());
                data.push(side as u8);
                data.extend_from_slice(&collateral.to_le_bytes());
                data.extend_from_slice(&notional.to_le_bytes());
                data.extend_from_slice(&leverage_x.to_le_bytes());
            }
            Self::GuardedOpenCashPosition {
                position_id,
                market_id,
                side,
                collateral,
                notional,
                leverage_x,
                min_payout_factor_bps,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&market_id.to_le_bytes());
                data.push(side as u8);
                data.extend_from_slice(&collateral.to_le_bytes());
                data.extend_from_slice(&notional.to_le_bytes());
                data.extend_from_slice(&leverage_x.to_le_bytes());
                data.extend_from_slice(&min_payout_factor_bps.to_le_bytes());
            }
            Self::OpenClaimPosition {
                position_id,
                market_id,
                side,
                claim_amount,
                notional,
                leverage_x,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&market_id.to_le_bytes());
                data.push(side as u8);
                data.extend_from_slice(&claim_amount.to_le_bytes());
                data.extend_from_slice(&notional.to_le_bytes());
                data.extend_from_slice(&leverage_x.to_le_bytes());
            }
            Self::GuardedOpenClaimPosition {
                position_id,
                market_id,
                side,
                claim_amount,
                notional,
                leverage_x,
                min_payout_factor_bps,
            } => {
                data.extend_from_slice(&position_id.to_le_bytes());
                data.extend_from_slice(&market_id.to_le_bytes());
                data.push(side as u8);
                data.extend_from_slice(&claim_amount.to_le_bytes());
                data.extend_from_slice(&notional.to_le_bytes());
                data.extend_from_slice(&leverage_x.to_le_bytes());
                data.extend_from_slice(&min_payout_factor_bps.to_le_bytes());
            }
            Self::UpdateLeverageLimit {
                market_id,
                max_leverage_x,
            } => {
                data.extend_from_slice(&market_id.to_le_bytes());
                data.extend_from_slice(&max_leverage_x.to_le_bytes());
            }
            Self::UpdateRiskLimits {
                market_id,
                max_leverage_x,
                max_position_notional,
                max_net_profit_per_position,
                max_reserved_profit_per_controller,
                total_liability_open_gate,
                max_long_open_interest,
                max_short_open_interest,
            } => {
                data.extend_from_slice(&market_id.to_le_bytes());
                data.extend_from_slice(&max_leverage_x.to_le_bytes());
                data.extend_from_slice(&max_position_notional.to_le_bytes());
                data.extend_from_slice(&max_net_profit_per_position.to_le_bytes());
                data.extend_from_slice(&max_reserved_profit_per_controller.to_le_bytes());
                data.extend_from_slice(&total_liability_open_gate.to_le_bytes());
                data.extend_from_slice(&max_long_open_interest.to_le_bytes());
                data.extend_from_slice(&max_short_open_interest.to_le_bytes());
            }
            Self::PrepareController
            | Self::InitializeController
            | Self::InitializeVault
            | Self::ServiceFifoHead
            | Self::ServiceFifoHeadInternal
            | Self::PrepareTradingSession => {}
        }
        data
    }

    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() < MARTINGALER_IX_PREFIX_LEN || data[..8] != MARTINGALER_IX_MAGIC {
            return Err(BoundaryError::InvalidInstruction.into());
        }
        let version = u16::from_le_bytes(
            data[8..10]
                .try_into()
                .map_err(|_| BoundaryError::InvalidInstruction)?,
        );
        if version != MARTINGALER_IX_VERSION {
            return Err(BoundaryError::InvalidInstruction.into());
        }
        match data[10] {
            PREPARE_POOL_TAG | INITIALIZE_POOL_TAG => {
                if data.len() != POOL_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let risk_pool_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if risk_pool_id == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                if data[10] == PREPARE_POOL_TAG {
                    Ok(Self::PreparePool { risk_pool_id })
                } else {
                    Ok(Self::InitializePool { risk_pool_id })
                }
            }
            PREPARE_MARKET_TAG => {
                if data.len() != PREPARE_MARKET_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let market_id = u16::from_le_bytes(
                    data[11..13]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if market_id == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::PrepareMarket { market_id })
            }
            INITIALIZE_MARKET_TAG => {
                if data.len() != INITIALIZE_MARKET_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let market_id = u16::from_le_bytes(
                    data[11..13]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let divergence_limit_bps = u16::from_le_bytes(
                    data[13..15]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let max_long_open_interest = u128::from_le_bytes(
                    data[15..31]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let max_short_open_interest = u128::from_le_bytes(
                    data[31..47]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let haircut = HaircutConfig::new(
                    u32::from_le_bytes(
                        data[47..51]
                            .try_into()
                            .map_err(|_| BoundaryError::InvalidInstruction)?,
                    ),
                    u32::from_le_bytes(
                        data[51..55]
                            .try_into()
                            .map_err(|_| BoundaryError::InvalidInstruction)?,
                    ),
                    u32::from_le_bytes(
                        data[55..59]
                            .try_into()
                            .map_err(|_| BoundaryError::InvalidInstruction)?,
                    ),
                    u32::from_le_bytes(
                        data[59..63]
                            .try_into()
                            .map_err(|_| BoundaryError::InvalidInstruction)?,
                    ),
                    u32::from_le_bytes(
                        data[63..67]
                            .try_into()
                            .map_err(|_| BoundaryError::InvalidInstruction)?,
                    ),
                    u32::from_le_bytes(
                        data[67..71]
                            .try_into()
                            .map_err(|_| BoundaryError::InvalidInstruction)?,
                    ),
                );
                if market_id == 0
                    || divergence_limit_bps == 0
                    || divergence_limit_bps > 10_000
                    || max_long_open_interest == 0
                    || max_long_open_interest > V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
                    || max_short_open_interest == 0
                    || max_short_open_interest > V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
                    || haircut.validate().is_err()
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::InitializeMarket {
                    market_id,
                    divergence_limit_bps,
                    max_long_open_interest,
                    max_short_open_interest,
                    haircut,
                })
            }
            PREPARE_CONTROLLER_TAG if data.len() == CONTROLLER_IX_LEN => {
                Ok(Self::PrepareController)
            }
            INITIALIZE_CONTROLLER_TAG if data.len() == CONTROLLER_IX_LEN => {
                Ok(Self::InitializeController)
            }
            INITIALIZE_VAULT_TAG if data.len() == INITIALIZE_VAULT_IX_LEN => {
                Ok(Self::InitializeVault)
            }
            DEPOSIT_CASH_TAG => {
                if data.len() != DEPOSIT_CASH_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let amount = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if amount == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::DepositCash { amount })
            }
            PREPARE_ORACLE_POLICY_TAG => {
                if data.len() != PREPARE_ORACLE_POLICY_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let market_id = u16::from_le_bytes(
                    data[11..13]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if market_id == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::PrepareOraclePolicy { market_id })
            }
            INITIALIZE_ORACLE_POLICY_TAG => {
                if data.len() != INITIALIZE_ORACLE_POLICY_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let market_id = u16::from_le_bytes(
                    data[11..13]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let primary_feed_id: [u8; 32] = data[13..45]
                    .try_into()
                    .map_err(|_| BoundaryError::InvalidInstruction)?;
                let secondary_feed_id: [u8; 32] = data[45..77]
                    .try_into()
                    .map_err(|_| BoundaryError::InvalidInstruction)?;
                let max_staleness_secs = u64::from_le_bytes(
                    data[77..85]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let confidence_limit_bps = u16::from_le_bytes(
                    data[85..87]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if market_id == 0
                    || primary_feed_id == [0; 32]
                    || (secondary_feed_id != [0; 32] && secondary_feed_id == primary_feed_id)
                    || max_staleness_secs == 0
                    || max_staleness_secs > MAX_ORACLE_STALENESS_SECS
                    || confidence_limit_bps == 0
                    || confidence_limit_bps > MAX_ORACLE_CONFIDENCE_BPS
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::InitializeOraclePolicy {
                    market_id,
                    primary_feed_id,
                    secondary_feed_id,
                    max_staleness_secs,
                    confidence_limit_bps,
                })
            }
            PREPARE_POSITION_TAG => {
                if data.len() != PREPARE_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::PreparePosition { position_id })
            }
            OPEN_CASH_POSITION_TAG => {
                if data.len() != OPEN_CASH_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let market_id = u16::from_le_bytes(
                    data[19..21]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let side = PositionSide::try_from(data[21])
                    .map_err(|_| ProgramError::from(BoundaryError::InvalidInstruction))?;
                let collateral = u64::from_le_bytes(
                    data[22..30]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let notional = u64::from_le_bytes(
                    data[30..38]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let leverage_x = u16::from_le_bytes(
                    data[38..40]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0
                    || market_id == 0
                    || collateral == 0
                    || notional == 0
                    || u128::from(notional) > V1_MAX_POSITION_NOTIONAL_QUOTE
                    || leverage_x == 0
                    || leverage_x > V1_MAX_LEVERAGE_X
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::OpenCashPosition {
                    position_id,
                    market_id,
                    side,
                    collateral,
                    notional,
                    leverage_x,
                })
            }
            GUARDED_OPEN_CASH_POSITION_TAG | GUARDED_OPEN_CLAIM_POSITION_TAG => {
                if data.len() != GUARDED_OPEN_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let market_id = u16::from_le_bytes(
                    data[19..21]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let side = PositionSide::try_from(data[21])
                    .map_err(|_| ProgramError::from(BoundaryError::InvalidInstruction))?;
                let collateral = u64::from_le_bytes(
                    data[22..30]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let notional = u64::from_le_bytes(
                    data[30..38]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let leverage_x = u16::from_le_bytes(
                    data[38..40]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let min_payout_factor_bps = u16::from_le_bytes(
                    data[40..42]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0
                    || market_id == 0
                    || collateral == 0
                    || notional == 0
                    || u128::from(notional) > V1_MAX_POSITION_NOTIONAL_QUOTE
                    || leverage_x == 0
                    || leverage_x > V1_MAX_LEVERAGE_X
                    || (min_payout_factor_bps != 0
                        && !(V1_MIN_PAYOUT_FACTOR_BPS..=V1_PAYOUT_FACTOR_SCALE_BPS)
                            .contains(&min_payout_factor_bps))
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                if data[10] == GUARDED_OPEN_CASH_POSITION_TAG {
                    Ok(Self::GuardedOpenCashPosition {
                        position_id,
                        market_id,
                        side,
                        collateral,
                        notional,
                        leverage_x,
                        min_payout_factor_bps,
                    })
                } else {
                    Ok(Self::GuardedOpenClaimPosition {
                        position_id,
                        market_id,
                        side,
                        claim_amount: collateral,
                        notional,
                        leverage_x,
                        min_payout_factor_bps,
                    })
                }
            }
            CLOSE_NONPOSITIVE_CASH_POSITION_TAG | RESOLVE_BANKRUPT_CASH_POSITION_TAG => {
                if data.len() != CLOSE_CASH_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                if data[10] == CLOSE_NONPOSITIVE_CASH_POSITION_TAG {
                    Ok(Self::CloseNonPositiveCashPosition { position_id })
                } else {
                    Ok(Self::ResolveBankruptCashPosition { position_id })
                }
            }
            PREPARE_CLAIM_TAG | PREPARE_QUEUE_PAGE_TAG => {
                if data.len() != PREPARE_CLAIM_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let identifier = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if identifier == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                if data[10] == PREPARE_CLAIM_TAG {
                    Ok(Self::PrepareClaim {
                        position_id: identifier,
                    })
                } else {
                    Ok(Self::PrepareQueuePage {
                        page_index: identifier,
                    })
                }
            }
            CLOSE_PROFITABLE_CASH_POSITION_TAG => {
                if data.len() != CLOSE_PROFITABLE_CASH_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::CloseProfitableCashPosition {
                    position_id,
                    settlement_shape: ProfitSettlementShape::try_from(data[19])?,
                })
            }
            SERVICE_FIFO_HEAD_TAG => {
                if data.len() != SERVICE_FIFO_HEAD_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::ServiceFifoHead)
            }
            SERVICE_FIFO_HEAD_INTERNAL_TAG => {
                if data.len() != SERVICE_FIFO_HEAD_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::ServiceFifoHeadInternal)
            }
            POP_FIFO_HEAD_TAG => {
                if data.len() != POP_FIFO_HEAD_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::PopFifoHead {
                    shape: PopFifoShape::try_from(data[11])?,
                })
            }
            OPEN_CLAIM_POSITION_TAG => {
                if data.len() != OPEN_CLAIM_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let market_id = u16::from_le_bytes(
                    data[19..21]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let side = PositionSide::try_from(data[21])
                    .map_err(|_| ProgramError::from(BoundaryError::InvalidInstruction))?;
                let claim_amount = u64::from_le_bytes(
                    data[22..30]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let notional = u64::from_le_bytes(
                    data[30..38]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let leverage_x = u16::from_le_bytes(
                    data[38..40]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0
                    || market_id == 0
                    || claim_amount == 0
                    || notional == 0
                    || u128::from(notional) > V1_MAX_POSITION_NOTIONAL_QUOTE
                    || leverage_x == 0
                    || leverage_x > V1_MAX_LEVERAGE_X
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::OpenClaimPosition {
                    position_id,
                    market_id,
                    side,
                    claim_amount,
                    notional,
                    leverage_x,
                })
            }
            CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG | CLOSE_PROFITABLE_CLAIM_POSITION_TAG => {
                if data.len() != CLOSE_CLAIM_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let settlement_shape = ProfitSettlementShape::try_from(data[19])?;
                if data[10] == CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG {
                    Ok(Self::CloseNonPositiveClaimPosition {
                        position_id,
                        settlement_shape,
                    })
                } else {
                    Ok(Self::CloseProfitableClaimPosition {
                        position_id,
                        settlement_shape,
                    })
                }
            }
            RESOLVE_BANKRUPT_CLAIM_POSITION_TAG => {
                if data.len() != RESOLVE_BANKRUPT_CLAIM_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::ResolveBankruptClaimPosition { position_id })
            }
            SET_POOL_STATUS_TAG | SET_MARKET_STATUS_TAG => {
                if data.len() != SET_STATUS_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let target = PoolStatus::try_from(data[11])
                    .map_err(|_| ProgramError::from(BoundaryError::InvalidInstruction))?;
                if data[10] == SET_POOL_STATUS_TAG {
                    Ok(Self::SetPoolStatus { target })
                } else {
                    Ok(Self::SetMarketStatus { target })
                }
            }
            WITHDRAW_IDLE_CASH_TAG => {
                if data.len() != WITHDRAW_IDLE_CASH_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let amount = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if amount == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::WithdrawIdleCash { amount })
            }
            PREPARE_ORACLE_RELAY_TAG => {
                if data.len() != PREPARE_ORACLE_RELAY_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let market_id = u16::from_le_bytes(
                    data[11..13]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let feed_id = data[13..45]
                    .try_into()
                    .map_err(|_| BoundaryError::InvalidInstruction)?;
                if market_id == 0 || feed_id == [0; 32] {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::PrepareOracleRelay { market_id, feed_id })
            }
            INITIALIZE_ORACLE_RELAY_TAG => {
                if data.len() != INITIALIZE_ORACLE_RELAY_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let market_id = u16::from_le_bytes(
                    data[11..13]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let feed_id = data[13..45]
                    .try_into()
                    .map_err(|_| BoundaryError::InvalidInstruction)?;
                let authority = data[45..77]
                    .try_into()
                    .map_err(|_| BoundaryError::InvalidInstruction)?;
                let source = OracleRelaySource::try_from(data[77])
                    .map_err(|_| ProgramError::from(BoundaryError::InvalidInstruction))?;
                let price_e6_per_tick = u64::from_le_bytes(
                    data[78..86]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let max_source_staleness_slots = u64::from_le_bytes(
                    data[86..94]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let max_spread_bps = u16::from_le_bytes(
                    data[94..96]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let max_mark_index_divergence_bps = u16::from_le_bytes(
                    data[96..98]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if market_id == 0 || feed_id == [0; 32] || authority == [0; 32] {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::InitializeOracleRelay {
                    market_id,
                    feed_id,
                    authority,
                    source,
                    price_e6_per_tick,
                    max_source_staleness_slots,
                    max_spread_bps,
                    max_mark_index_divergence_bps,
                })
            }
            UPDATE_ORACLE_RELAY_TAG => {
                if data.len() != UPDATE_ORACLE_RELAY_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let price_e6 = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let confidence_e6 = u64::from_le_bytes(
                    data[19..27]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let publish_time = i64::from_le_bytes(
                    data[27..35]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let source_posted_slot = u64::from_le_bytes(
                    data[35..43]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if price_e6 == 0
                    || confidence_e6 > price_e6
                    || publish_time <= 0
                    || source_posted_slot == 0
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::UpdateOracleRelay {
                    price_e6,
                    confidence_e6,
                    publish_time,
                    source_posted_slot,
                })
            }
            REFRESH_PHOENIX_ORACLE_TAG => {
                if data.len() != REFRESH_PHOENIX_ORACLE_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let global_trader_index_count = data[11];
                let active_trader_buffer_count = data[12];
                if global_trader_index_count == 0
                    || usize::from(global_trader_index_count)
                        > MAX_PHOENIX_DYNAMIC_ACCOUNTS_PER_KIND
                    || active_trader_buffer_count == 0
                    || usize::from(active_trader_buffer_count)
                        > MAX_PHOENIX_DYNAMIC_ACCOUNTS_PER_KIND
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::RefreshPhoenixOracle {
                    global_trader_index_count,
                    active_trader_buffer_count,
                })
            }
            PREPARE_TRADING_SESSION_TAG => {
                if data.len() != PREPARE_TRADING_SESSION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::PrepareTradingSession)
            }
            INITIALIZE_TRADING_SESSION_TAG => {
                if data.len() != INITIALIZE_TRADING_SESSION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let session_key = data[11..43]
                    .try_into()
                    .map_err(|_| BoundaryError::InvalidInstruction)?;
                let expected_epoch = u64::from_le_bytes(
                    data[43..51]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if session_key == [0; 32] {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::InitializeTradingSession {
                    session_key,
                    expected_epoch,
                })
            }
            REVOKE_TRADING_SESSION_TAG => {
                if data.len() != REVOKE_TRADING_SESSION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let expected_epoch = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if expected_epoch == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::RevokeTradingSession { expected_epoch })
            }
            SESSION_PREPARE_POSITION_TAG
            | SESSION_CLOSE_NONPOSITIVE_CASH_POSITION_TAG
            | SESSION_PREPARE_CLAIM_TAG => {
                if data.len() != SESSION_PREPARE_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let epoch = u64::from_le_bytes(
                    data[19..27]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let nonce = u64::from_le_bytes(
                    data[27..35]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0 || epoch == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                match data[10] {
                    SESSION_PREPARE_POSITION_TAG => Ok(Self::SessionPreparePosition {
                        position_id,
                        epoch,
                        nonce,
                    }),
                    SESSION_CLOSE_NONPOSITIVE_CASH_POSITION_TAG => {
                        Ok(Self::SessionCloseNonPositiveCashPosition {
                            position_id,
                            epoch,
                            nonce,
                        })
                    }
                    _ => Ok(Self::SessionPrepareClaim {
                        position_id,
                        epoch,
                        nonce,
                    }),
                }
            }
            SESSION_OPEN_CASH_POSITION_TAG | SESSION_OPEN_CLAIM_POSITION_TAG => {
                if data.len() != SESSION_OPEN_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let market_id = u16::from_le_bytes(
                    data[19..21]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let side = PositionSide::try_from(data[21])
                    .map_err(|_| ProgramError::from(BoundaryError::InvalidInstruction))?;
                let collateral = u64::from_le_bytes(
                    data[22..30]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let notional = u64::from_le_bytes(
                    data[30..38]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let leverage_x = u16::from_le_bytes(
                    data[38..40]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let epoch = u64::from_le_bytes(
                    data[40..48]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let nonce = u64::from_le_bytes(
                    data[48..56]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0
                    || market_id == 0
                    || collateral == 0
                    || notional == 0
                    || u128::from(notional) > V1_MAX_POSITION_NOTIONAL_QUOTE
                    || leverage_x == 0
                    || leverage_x > V1_MAX_LEVERAGE_X
                    || epoch == 0
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                if data[10] == SESSION_OPEN_CASH_POSITION_TAG {
                    Ok(Self::SessionOpenCashPosition {
                        position_id,
                        market_id,
                        side,
                        collateral,
                        notional,
                        leverage_x,
                        epoch,
                        nonce,
                    })
                } else {
                    Ok(Self::SessionOpenClaimPosition {
                        position_id,
                        market_id,
                        side,
                        claim_amount: collateral,
                        notional,
                        leverage_x,
                        epoch,
                        nonce,
                    })
                }
            }
            SESSION_GUARDED_OPEN_CASH_POSITION_TAG | SESSION_GUARDED_OPEN_CLAIM_POSITION_TAG => {
                if data.len() != SESSION_GUARDED_OPEN_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let market_id = u16::from_le_bytes(
                    data[19..21]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let side = PositionSide::try_from(data[21])
                    .map_err(|_| ProgramError::from(BoundaryError::InvalidInstruction))?;
                let collateral = u64::from_le_bytes(
                    data[22..30]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let notional = u64::from_le_bytes(
                    data[30..38]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let leverage_x = u16::from_le_bytes(
                    data[38..40]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let min_payout_factor_bps = u16::from_le_bytes(
                    data[40..42]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let epoch = u64::from_le_bytes(
                    data[42..50]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let nonce = u64::from_le_bytes(
                    data[50..58]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0
                    || market_id == 0
                    || collateral == 0
                    || notional == 0
                    || u128::from(notional) > V1_MAX_POSITION_NOTIONAL_QUOTE
                    || leverage_x == 0
                    || leverage_x > V1_MAX_LEVERAGE_X
                    || (min_payout_factor_bps != 0
                        && !(V1_MIN_PAYOUT_FACTOR_BPS..=V1_PAYOUT_FACTOR_SCALE_BPS)
                            .contains(&min_payout_factor_bps))
                    || epoch == 0
                {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                if data[10] == SESSION_GUARDED_OPEN_CASH_POSITION_TAG {
                    Ok(Self::SessionGuardedOpenCashPosition {
                        position_id,
                        market_id,
                        side,
                        collateral,
                        notional,
                        leverage_x,
                        min_payout_factor_bps,
                        epoch,
                        nonce,
                    })
                } else {
                    Ok(Self::SessionGuardedOpenClaimPosition {
                        position_id,
                        market_id,
                        side,
                        claim_amount: collateral,
                        notional,
                        leverage_x,
                        min_payout_factor_bps,
                        epoch,
                        nonce,
                    })
                }
            }
            SESSION_CLOSE_PROFITABLE_CASH_POSITION_TAG
            | SESSION_CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG
            | SESSION_CLOSE_PROFITABLE_CLAIM_POSITION_TAG => {
                if data.len() != SESSION_CLOSE_PROFITABLE_POSITION_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let position_id = u64::from_le_bytes(
                    data[11..19]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let settlement_shape = ProfitSettlementShape::try_from(data[19])?;
                let epoch = u64::from_le_bytes(
                    data[20..28]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let nonce = u64::from_le_bytes(
                    data[28..36]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if position_id == 0 || epoch == 0 {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                match data[10] {
                    SESSION_CLOSE_PROFITABLE_CASH_POSITION_TAG => {
                        Ok(Self::SessionCloseProfitableCashPosition {
                            position_id,
                            settlement_shape,
                            epoch,
                            nonce,
                        })
                    }
                    SESSION_CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG => {
                        Ok(Self::SessionCloseNonPositiveClaimPosition {
                            position_id,
                            settlement_shape,
                            epoch,
                            nonce,
                        })
                    }
                    _ => Ok(Self::SessionCloseProfitableClaimPosition {
                        position_id,
                        settlement_shape,
                        epoch,
                        nonce,
                    }),
                }
            }
            UPDATE_LEVERAGE_LIMIT_TAG => {
                if data.len() != UPDATE_LEVERAGE_LIMIT_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let market_id = u16::from_le_bytes(
                    data[11..13]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let max_leverage_x = u16::from_le_bytes(
                    data[13..15]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                if market_id == 0 || max_leverage_x == 0 || max_leverage_x > V1_MAX_LEVERAGE_X {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(Self::UpdateLeverageLimit {
                    market_id,
                    max_leverage_x,
                })
            }
            UPDATE_RISK_LIMITS_TAG => {
                if data.len() != UPDATE_RISK_LIMITS_IX_LEN {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                let market_id = u16::from_le_bytes(
                    data[11..13]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let max_leverage_x = u16::from_le_bytes(
                    data[13..15]
                        .try_into()
                        .map_err(|_| BoundaryError::InvalidInstruction)?,
                );
                let read = |offset: usize| -> Result<u64, ProgramError> {
                    Ok(u64::from_le_bytes(
                        data[offset..offset + 8]
                            .try_into()
                            .map_err(|_| BoundaryError::InvalidInstruction)?,
                    ))
                };
                let candidate = Self::UpdateRiskLimits {
                    market_id,
                    max_leverage_x,
                    max_position_notional: read(15)?,
                    max_net_profit_per_position: read(23)?,
                    max_reserved_profit_per_controller: read(31)?,
                    total_liability_open_gate: read(39)?,
                    max_long_open_interest: read(47)?,
                    max_short_open_interest: read(55)?,
                };
                if market_id == 0 || max_leverage_x == 0 || max_leverage_x > V1_MAX_LEVERAGE_X {
                    return Err(BoundaryError::InvalidInstruction.into());
                }
                Ok(candidate)
            }
            _ => Err(BoundaryError::InvalidInstruction.into()),
        }
    }
}

pub fn has_namespace(data: &[u8]) -> bool {
    data.starts_with(&MARTINGALER_IX_MAGIC)
}

pub fn derive_pool(
    program_id: &Pubkey,
    admin: &Pubkey,
    quote_mint: &Pubkey,
    risk_pool_id: u64,
) -> (Pubkey, u8) {
    let risk_pool_id = risk_pool_id.to_le_bytes();
    Pubkey::find_program_address(
        &[
            POOL_SEED,
            admin.as_ref(),
            quote_mint.as_ref(),
            &risk_pool_id,
        ],
        program_id,
    )
}

pub fn derive_queue_page_zero(program_id: &Pubkey, pool: &Pubkey) -> (Pubkey, u8) {
    derive_queue_page(program_id, pool, 0)
}

pub fn derive_queue_page(program_id: &Pubkey, pool: &Pubkey, page_index: u64) -> (Pubkey, u8) {
    let page_index = page_index.to_le_bytes();
    Pubkey::find_program_address(&[QUEUE_SEED, pool.as_ref(), &page_index], program_id)
}

pub fn derive_vault_authority(program_id: &Pubkey, pool: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT_SEED, pool.as_ref()], program_id)
}

pub fn derive_market(program_id: &Pubkey, pool: &Pubkey, market_id: u16) -> (Pubkey, u8) {
    let market_id = market_id.to_le_bytes();
    Pubkey::find_program_address(&[MARKET_SEED, pool.as_ref(), &market_id], program_id)
}

pub fn derive_controller(program_id: &Pubkey, pool: &Pubkey, controller: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[CONTROLLER_SEED, pool.as_ref(), controller.as_ref()],
        program_id,
    )
}

pub fn derive_trading_session(program_id: &Pubkey, pool: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[TRADING_SESSION_SEED, pool.as_ref(), owner.as_ref()],
        program_id,
    )
}

pub fn derive_oracle_policy(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ORACLE_POLICY_SEED, market.as_ref()], program_id)
}

pub fn derive_oracle_relay(
    program_id: &Pubkey,
    market: &Pubkey,
    feed_id: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ORACLE_RELAY_SEED, market.as_ref(), feed_id], program_id)
}

pub fn derive_position(
    program_id: &Pubkey,
    pool: &Pubkey,
    controller: &Pubkey,
    position_id: u64,
) -> (Pubkey, u8) {
    let position_id = position_id.to_le_bytes();
    Pubkey::find_program_address(
        &[
            POSITION_SEED,
            pool.as_ref(),
            controller.as_ref(),
            &position_id,
        ],
        program_id,
    )
}

pub fn derive_claim(program_id: &Pubkey, position: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CLAIM_SEED, position.as_ref()], program_id)
}

pub fn derive_canonical_vault(vault_authority: &Pubkey, quote_mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[
            vault_authority.as_ref(),
            spl_token::ID.as_ref(),
            quote_mint.as_ref(),
        ],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

#[inline(never)]
pub fn process_instruction<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    instruction_data: &[u8],
) -> ProgramResult {
    match Instruction::decode(instruction_data)? {
        Instruction::PreparePool { risk_pool_id } => {
            process_prepare_pool(program_id, accounts, risk_pool_id)
        }
        Instruction::InitializePool { risk_pool_id } => {
            process_initialize_pool(program_id, accounts, risk_pool_id)
        }
        Instruction::PrepareMarket { market_id } => {
            process_prepare_market(program_id, accounts, market_id)
        }
        Instruction::InitializeMarket {
            market_id,
            divergence_limit_bps,
            max_long_open_interest,
            max_short_open_interest,
            haircut,
        } => process_initialize_market(
            program_id,
            accounts,
            market_id,
            divergence_limit_bps,
            max_long_open_interest,
            max_short_open_interest,
            haircut,
        ),
        Instruction::PrepareController => process_prepare_controller(program_id, accounts),
        Instruction::InitializeController => process_initialize_controller(program_id, accounts),
        Instruction::InitializeVault => process_initialize_vault(program_id, accounts),
        Instruction::DepositCash { amount } => process_deposit_cash(program_id, accounts, amount),
        Instruction::PrepareOraclePolicy { market_id } => {
            process_prepare_oracle_policy(program_id, accounts, market_id)
        }
        Instruction::InitializeOraclePolicy {
            market_id,
            primary_feed_id,
            secondary_feed_id,
            max_staleness_secs,
            confidence_limit_bps,
        } => process_initialize_oracle_policy(
            program_id,
            accounts,
            market_id,
            primary_feed_id,
            secondary_feed_id,
            max_staleness_secs,
            confidence_limit_bps,
        ),
        Instruction::PreparePosition { position_id } => {
            process_prepare_position(program_id, accounts, position_id, None)
        }
        Instruction::OpenCashPosition {
            position_id,
            market_id,
            side,
            collateral,
            notional,
            leverage_x,
        } => process_open_cash_position(
            program_id,
            accounts,
            position_id,
            market_id,
            side,
            collateral,
            notional,
            leverage_x,
            // Legacy wire tags remain decodable, but admission uses the same
            // mandatory V1 floor and dynamic payout quote as guarded opens.
            // A zero guard means "accept the displayed protocol factor".
            0,
            None,
        ),
        Instruction::GuardedOpenCashPosition {
            position_id,
            market_id,
            side,
            collateral,
            notional,
            leverage_x,
            min_payout_factor_bps,
        } => process_open_cash_position(
            program_id,
            accounts,
            position_id,
            market_id,
            side,
            collateral,
            notional,
            leverage_x,
            min_payout_factor_bps,
            None,
        ),
        Instruction::CloseNonPositiveCashPosition { position_id } => {
            process_close_nonpositive_cash_position(program_id, accounts, position_id, None)
        }
        Instruction::ResolveBankruptCashPosition { position_id } => {
            process_resolve_bankrupt_cash_position(program_id, accounts, position_id)
        }
        Instruction::PrepareClaim { position_id } => {
            process_prepare_claim(program_id, accounts, position_id, None)
        }
        Instruction::PrepareQueuePage { page_index } => {
            process_prepare_queue_page(program_id, accounts, page_index)
        }
        Instruction::CloseProfitableCashPosition {
            position_id,
            settlement_shape,
        } => process_close_profitable_cash_position(
            program_id,
            accounts,
            position_id,
            settlement_shape,
            None,
        ),
        Instruction::ServiceFifoHead => process_service_fifo_head(program_id, accounts),
        Instruction::ServiceFifoHeadInternal => {
            process_service_fifo_head_internal(program_id, accounts)
        }
        Instruction::PopFifoHead { shape } => process_pop_fifo_head(program_id, accounts, shape),
        Instruction::OpenClaimPosition {
            position_id,
            market_id,
            side,
            claim_amount,
            notional,
            leverage_x,
        } => process_open_claim_position(
            program_id,
            accounts,
            position_id,
            market_id,
            side,
            claim_amount,
            notional,
            leverage_x,
            0,
            None,
        ),
        Instruction::GuardedOpenClaimPosition {
            position_id,
            market_id,
            side,
            claim_amount,
            notional,
            leverage_x,
            min_payout_factor_bps,
        } => process_open_claim_position(
            program_id,
            accounts,
            position_id,
            market_id,
            side,
            claim_amount,
            notional,
            leverage_x,
            min_payout_factor_bps,
            None,
        ),
        Instruction::CloseNonPositiveClaimPosition {
            position_id,
            settlement_shape,
        } => process_close_claim_position(
            program_id,
            accounts,
            position_id,
            settlement_shape,
            ClaimVoluntaryCloseKind::NonPositive,
            None,
        ),
        Instruction::ResolveBankruptClaimPosition { position_id } => {
            process_resolve_bankrupt_claim_position(program_id, accounts, position_id)
        }
        Instruction::CloseProfitableClaimPosition {
            position_id,
            settlement_shape,
        } => process_close_claim_position(
            program_id,
            accounts,
            position_id,
            settlement_shape,
            ClaimVoluntaryCloseKind::Profitable,
            None,
        ),
        Instruction::SetPoolStatus { target } => {
            process_set_pool_status(program_id, accounts, target)
        }
        Instruction::SetMarketStatus { target } => {
            process_set_market_status(program_id, accounts, target)
        }
        Instruction::WithdrawIdleCash { amount } => {
            process_withdraw_idle_cash(program_id, accounts, amount)
        }
        Instruction::PrepareOracleRelay { market_id, feed_id } => {
            process_prepare_oracle_relay(program_id, accounts, market_id, feed_id)
        }
        Instruction::InitializeOracleRelay {
            market_id,
            feed_id,
            authority,
            source,
            price_e6_per_tick,
            max_source_staleness_slots,
            max_spread_bps,
            max_mark_index_divergence_bps,
        } => process_initialize_oracle_relay(
            program_id,
            accounts,
            market_id,
            feed_id,
            authority,
            source,
            price_e6_per_tick,
            max_source_staleness_slots,
            max_spread_bps,
            max_mark_index_divergence_bps,
        ),
        Instruction::UpdateOracleRelay {
            price_e6,
            confidence_e6,
            publish_time,
            source_posted_slot,
        } => process_update_oracle_relay(
            program_id,
            accounts,
            price_e6,
            confidence_e6,
            publish_time,
            source_posted_slot,
        ),
        Instruction::RefreshPhoenixOracle {
            global_trader_index_count,
            active_trader_buffer_count,
        } => process_refresh_phoenix_oracle(
            program_id,
            accounts,
            global_trader_index_count,
            active_trader_buffer_count,
        ),
        Instruction::PrepareTradingSession => process_prepare_trading_session(program_id, accounts),
        Instruction::InitializeTradingSession {
            session_key,
            expected_epoch,
        } => process_initialize_trading_session(
            program_id,
            accounts,
            Pubkey::new_from_array(session_key),
            expected_epoch,
        ),
        Instruction::RevokeTradingSession { expected_epoch } => {
            process_revoke_trading_session(program_id, accounts, expected_epoch)
        }
        Instruction::SessionPreparePosition {
            position_id,
            epoch,
            nonce,
        } => process_session_prepare_position(program_id, accounts, position_id, epoch, nonce),
        Instruction::SessionOpenCashPosition {
            position_id,
            market_id,
            side,
            collateral,
            notional,
            leverage_x,
            epoch,
            nonce,
        } => process_session_open_cash_position(
            program_id,
            accounts,
            position_id,
            market_id,
            side,
            collateral,
            notional,
            leverage_x,
            0,
            epoch,
            nonce,
        ),
        Instruction::SessionGuardedOpenCashPosition {
            position_id,
            market_id,
            side,
            collateral,
            notional,
            leverage_x,
            min_payout_factor_bps,
            epoch,
            nonce,
        } => process_session_open_cash_position(
            program_id,
            accounts,
            position_id,
            market_id,
            side,
            collateral,
            notional,
            leverage_x,
            min_payout_factor_bps,
            epoch,
            nonce,
        ),
        Instruction::SessionCloseNonPositiveCashPosition {
            position_id,
            epoch,
            nonce,
        } => process_session_close_nonpositive_cash_position(
            program_id,
            accounts,
            position_id,
            epoch,
            nonce,
        ),
        Instruction::SessionPrepareClaim {
            position_id,
            epoch,
            nonce,
        } => process_session_prepare_claim(program_id, accounts, position_id, epoch, nonce),
        Instruction::SessionCloseProfitableCashPosition {
            position_id,
            settlement_shape,
            epoch,
            nonce,
        } => process_session_close_profitable_cash_position(
            program_id,
            accounts,
            position_id,
            settlement_shape,
            epoch,
            nonce,
        ),
        Instruction::SessionOpenClaimPosition {
            position_id,
            market_id,
            side,
            claim_amount,
            notional,
            leverage_x,
            epoch,
            nonce,
        } => process_session_open_claim_position(
            program_id,
            accounts,
            position_id,
            market_id,
            side,
            claim_amount,
            notional,
            leverage_x,
            0,
            epoch,
            nonce,
        ),
        Instruction::SessionGuardedOpenClaimPosition {
            position_id,
            market_id,
            side,
            claim_amount,
            notional,
            leverage_x,
            min_payout_factor_bps,
            epoch,
            nonce,
        } => process_session_open_claim_position(
            program_id,
            accounts,
            position_id,
            market_id,
            side,
            claim_amount,
            notional,
            leverage_x,
            min_payout_factor_bps,
            epoch,
            nonce,
        ),
        Instruction::SessionCloseNonPositiveClaimPosition {
            position_id,
            settlement_shape,
            epoch,
            nonce,
        } => process_session_close_claim_position(
            program_id,
            accounts,
            position_id,
            settlement_shape,
            ClaimVoluntaryCloseKind::NonPositive,
            epoch,
            nonce,
        ),
        Instruction::SessionCloseProfitableClaimPosition {
            position_id,
            settlement_shape,
            epoch,
            nonce,
        } => process_session_close_claim_position(
            program_id,
            accounts,
            position_id,
            settlement_shape,
            ClaimVoluntaryCloseKind::Profitable,
            epoch,
            nonce,
        ),
        Instruction::UpdateLeverageLimit {
            market_id,
            max_leverage_x,
        } => process_update_leverage_limit(program_id, accounts, market_id, max_leverage_x),
        Instruction::UpdateRiskLimits {
            market_id,
            max_leverage_x,
            max_position_notional,
            max_net_profit_per_position,
            max_reserved_profit_per_controller,
            total_liability_open_gate,
            max_long_open_interest,
            max_short_open_interest,
        } => process_update_risk_limits(
            program_id,
            accounts,
            market_id,
            max_leverage_x,
            max_position_notional,
            max_net_profit_per_position,
            max_reserved_profit_per_controller,
            total_liability_open_gate,
            max_long_open_interest,
            max_short_open_interest,
        ),
    }
}

#[inline(never)]
fn process_prepare_trading_session<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> ProgramResult {
    if accounts.len() != PREPARE_TRADING_SESSION_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let owner = &accounts[0];
    let pool = &accounts[1];
    let session = &accounts[2];
    let system_program_ai = &accounts[3];
    validate_payer_authority(owner)?;
    expect_readonly(pool)?;
    load_canonical_pool(program_id, pool)?;
    expect_writable(session)?;
    expect_nonsigner(session)?;
    validate_external_program(system_program_ai, &system_program::ID)?;
    let (expected_session, bump) = derive_trading_session(program_id, pool.key, owner.key);
    if session.key != &expected_session {
        return Err(BoundaryError::InvalidPda.into());
    }
    validate_fresh_target(session)?;
    let bump = [bump];
    let signer_seeds: &[&[u8]] = &[
        TRADING_SESSION_SEED,
        pool.key.as_ref(),
        owner.key.as_ref(),
        &bump,
    ];
    allocate_pda(
        owner,
        session,
        system_program_ai,
        program_id,
        account_len::<TradingSessionState>(),
        signer_seeds,
    )
}

#[inline(never)]
fn process_initialize_trading_session<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    session_key: Pubkey,
    expected_epoch: u64,
) -> ProgramResult {
    if accounts.len() != INITIALIZE_TRADING_SESSION_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let owner = &accounts[0];
    let pool = &accounts[1];
    let controller = &accounts[2];
    let session = &accounts[3];
    let session_key_ai = &accounts[4];
    validate_payer_authority(owner)?;
    expect_nonsigner(pool)?;
    load_canonical_pool(program_id, pool)?;
    expect_nonsigner(controller)?;
    load_canonical_controller(program_id, pool.key, owner.key, controller)?;
    expect_writable(session)?;
    expect_nonsigner(session)?;
    expect_signer(session_key_ai)?;
    if session_key_ai.key != &session_key
        || session_key_ai.owner != &system_program::ID
        || session_key_ai.executable
        || session_key_ai.data_len() != 0
        || session_key == *owner.key
    {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    let (expected_session, bump) = derive_trading_session(program_id, pool.key, owner.key);
    if session.key != &expected_session
        || session.owner != program_id
        || session.executable
        || session.data_len() != account_len::<TradingSessionState>()
        || !Rent::get()?.is_exempt(session.lamports(), session.data_len())
    {
        return Err(BoundaryError::InvalidPda.into());
    }
    let now = Clock::get()?.unix_timestamp;
    let is_prepared = session.try_borrow_data()?.iter().all(|byte| *byte == 0);
    let next = if is_prepared {
        if expected_epoch != 0 {
            return Err(BoundaryError::InvalidAuthority.into());
        }
        TradingSessionState::new(
            Key32::new(pool.key.to_bytes()),
            Key32::new(owner.key.to_bytes()),
            Key32::new(session_key.to_bytes()),
            now,
        )?
    } else {
        let current = load_canonical_trading_session(program_id, pool.key, owner.key, session)?;
        current.renew(
            Key32::new(owner.key.to_bytes()),
            Key32::new(session_key.to_bytes()),
            expected_epoch,
            now,
        )?
    };
    if is_prepared {
        initialize_account(&mut session.try_borrow_mut_data()?, bump, &next)?;
    } else {
        overwrite_account(&mut session.try_borrow_mut_data()?, &next)?;
    }
    if load_canonical_trading_session(program_id, pool.key, owner.key, session)? != next {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_revoke_trading_session<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    expected_epoch: u64,
) -> ProgramResult {
    if accounts.len() != REVOKE_TRADING_SESSION_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let owner = &accounts[0];
    let session = &accounts[1];
    validate_payer_authority(owner)?;
    expect_writable(session)?;
    expect_nonsigner(session)?;
    if session.owner != program_id || session.data_len() != account_len::<TradingSessionState>() {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    let current = decode_account::<TradingSessionState>(&session.try_borrow_data()?)?;
    let pool = Pubkey::new_from_array(current.pool.0);
    let canonical = load_canonical_trading_session(program_id, &pool, owner.key, session)?;
    let next = canonical.revoke(Key32::new(owner.key.to_bytes()), expected_epoch)?;
    overwrite_account(&mut session.try_borrow_mut_data()?, &next)?;
    if load_canonical_trading_session(program_id, &pool, owner.key, session)? != next {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

fn validate_session_action<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    core_account_count: usize,
    epoch: u64,
    nonce: u64,
    opened_notional: u64,
) -> Result<
    (
        &'a [AccountInfo<'a>],
        &'a AccountInfo<'a>,
        Pubkey,
        TradingSessionState,
    ),
    ProgramError,
> {
    if accounts.len() != core_account_count + SESSION_AUTH_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let core = &accounts[..core_account_count];
    let session_signer = &core[0];
    let session = &accounts[core_account_count];
    let owner = &accounts[core_account_count + 1];
    validate_payer_authority(session_signer)?;
    expect_writable(session)?;
    expect_nonsigner(session)?;
    expect_readonly(owner)?;
    expect_nonsigner(owner)?;
    if owner.owner != &system_program::ID || owner.executable || owner.data_len() != 0 {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    if session.owner != program_id || session.data_len() != account_len::<TradingSessionState>() {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    let candidate = decode_account::<TradingSessionState>(&session.try_borrow_data()?)?;
    let pool = Pubkey::new_from_array(candidate.pool.0);
    let state = load_canonical_trading_session(program_id, &pool, owner.key, session)?;
    let next = state.authorize_action(
        Key32::new(pool.to_bytes()),
        Key32::new(owner.key.to_bytes()),
        Key32::new(session_signer.key.to_bytes()),
        epoch,
        nonce,
        Clock::get()?.unix_timestamp,
        u128::from(opened_notional),
    )?;
    Ok((core, session, *owner.key, next))
}

fn persist_session_action(
    program_id: &Pubkey,
    session: &AccountInfo<'_>,
    owner: &Pubkey,
    next: TradingSessionState,
) -> ProgramResult {
    overwrite_account(&mut session.try_borrow_mut_data()?, &next)?;
    let pool = Pubkey::new_from_array(next.pool.0);
    if load_canonical_trading_session(program_id, &pool, owner, session)? != next {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_session_prepare_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    epoch: u64,
    nonce: u64,
) -> ProgramResult {
    let (core, session, owner, next) = validate_session_action(
        program_id,
        accounts,
        PREPARE_POSITION_ACCOUNT_COUNT,
        epoch,
        nonce,
        0,
    )?;
    process_prepare_position(program_id, core, position_id, Some(&owner))?;
    persist_session_action(program_id, session, &owner, next)
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn process_session_open_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    market_id: u16,
    side: PositionSide,
    collateral: u64,
    notional: u64,
    leverage_x: u16,
    min_payout_factor_bps: u16,
    epoch: u64,
    nonce: u64,
) -> ProgramResult {
    let core_account_count = accounts
        .len()
        .checked_sub(SESSION_AUTH_ACCOUNT_COUNT)
        .ok_or(BoundaryError::InvalidAccountCount)?;
    if !(OPEN_CASH_BASE_ACCOUNT_COUNT..=OPEN_CASH_BASE_ACCOUNT_COUNT + 1)
        .contains(&core_account_count)
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    let (core, session, owner, next) = validate_session_action(
        program_id,
        accounts,
        core_account_count,
        epoch,
        nonce,
        notional,
    )?;
    process_open_cash_position(
        program_id,
        core,
        position_id,
        market_id,
        side,
        collateral,
        notional,
        leverage_x,
        min_payout_factor_bps,
        Some(&owner),
    )?;
    persist_session_action(program_id, session, &owner, next)
}

#[inline(never)]
fn process_session_close_nonpositive_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    epoch: u64,
    nonce: u64,
) -> ProgramResult {
    let core_account_count = accounts
        .len()
        .checked_sub(SESSION_AUTH_ACCOUNT_COUNT)
        .ok_or(BoundaryError::InvalidAccountCount)?;
    if !(CLOSE_CASH_ACCOUNT_COUNT..=CLOSE_CASH_ACCOUNT_COUNT + 1).contains(&core_account_count) {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    let (core, session, owner, next) =
        validate_session_action(program_id, accounts, core_account_count, epoch, nonce, 0)?;
    process_close_nonpositive_cash_position(program_id, core, position_id, Some(&owner))?;
    persist_session_action(program_id, session, &owner, next)
}

#[inline(never)]
fn process_session_prepare_claim<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    epoch: u64,
    nonce: u64,
) -> ProgramResult {
    let (core, session, owner, next) = validate_session_action(
        program_id,
        accounts,
        PREPARE_CLAIM_ACCOUNT_COUNT,
        epoch,
        nonce,
        0,
    )?;
    process_prepare_claim(program_id, core, position_id, Some(&owner))?;
    persist_session_action(program_id, session, &owner, next)
}

#[inline(never)]
fn process_session_close_profitable_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    settlement_shape: ProfitSettlementShape,
    epoch: u64,
    nonce: u64,
) -> ProgramResult {
    let base_account_count = match settlement_shape {
        ProfitSettlementShape::NoClaim => PROFITABLE_CLOSE_BASE_ACCOUNT_COUNT,
        ProfitSettlementShape::CurrentTail => PROFITABLE_CLOSE_BASE_ACCOUNT_COUNT + 2,
        ProfitSettlementShape::Rollover => PROFITABLE_CLOSE_BASE_ACCOUNT_COUNT + 3,
    };
    let core_account_count = accounts
        .len()
        .checked_sub(SESSION_AUTH_ACCOUNT_COUNT)
        .ok_or(BoundaryError::InvalidAccountCount)?;
    if !(base_account_count..=base_account_count + 1).contains(&core_account_count) {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    let (core, session, owner, next) =
        validate_session_action(program_id, accounts, core_account_count, epoch, nonce, 0)?;
    process_close_profitable_cash_position(
        program_id,
        core,
        position_id,
        settlement_shape,
        Some(&owner),
    )?;
    persist_session_action(program_id, session, &owner, next)
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn process_session_open_claim_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    market_id: u16,
    side: PositionSide,
    claim_amount: u64,
    notional: u64,
    leverage_x: u16,
    min_payout_factor_bps: u16,
    epoch: u64,
    nonce: u64,
) -> ProgramResult {
    let core_account_count = accounts
        .len()
        .checked_sub(SESSION_AUTH_ACCOUNT_COUNT)
        .ok_or(BoundaryError::InvalidAccountCount)?;
    if !(OPEN_CLAIM_BASE_ACCOUNT_COUNT..=OPEN_CLAIM_BASE_ACCOUNT_COUNT + 1)
        .contains(&core_account_count)
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    let (core, session, owner, next) = validate_session_action(
        program_id,
        accounts,
        core_account_count,
        epoch,
        nonce,
        notional,
    )?;
    process_open_claim_position(
        program_id,
        core,
        position_id,
        market_id,
        side,
        claim_amount,
        notional,
        leverage_x,
        min_payout_factor_bps,
        Some(&owner),
    )?;
    persist_session_action(program_id, session, &owner, next)
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn process_session_close_claim_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    settlement_shape: ProfitSettlementShape,
    close_kind: ClaimVoluntaryCloseKind,
    epoch: u64,
    nonce: u64,
) -> ProgramResult {
    let base_account_count = match settlement_shape {
        ProfitSettlementShape::NoClaim => CLAIM_CLOSE_BASE_ACCOUNT_COUNT,
        ProfitSettlementShape::CurrentTail => CLAIM_CLOSE_BASE_ACCOUNT_COUNT + 2,
        ProfitSettlementShape::Rollover => CLAIM_CLOSE_BASE_ACCOUNT_COUNT + 3,
    };
    let core_account_count = accounts
        .len()
        .checked_sub(SESSION_AUTH_ACCOUNT_COUNT)
        .ok_or(BoundaryError::InvalidAccountCount)?;
    if !(base_account_count..=base_account_count + 1).contains(&core_account_count) {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    let (core, session, owner, next) =
        validate_session_action(program_id, accounts, core_account_count, epoch, nonce, 0)?;
    process_close_claim_position(
        program_id,
        core,
        position_id,
        settlement_shape,
        close_kind,
        Some(&owner),
    )?;
    persist_session_action(program_id, session, &owner, next)
}

#[inline(never)]
fn process_prepare_pool<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    risk_pool_id: u64,
) -> ProgramResult {
    let boundary = validate_common(program_id, accounts, risk_pool_id)?;
    validate_fresh_target(boundary.pool)?;
    validate_fresh_target(boundary.page_zero)?;

    let risk_pool_id_bytes = risk_pool_id.to_le_bytes();
    let pool_bump = [boundary.pool_bump];
    let pool_signer_seeds: &[&[u8]] = &[
        POOL_SEED,
        boundary.admin.key.as_ref(),
        boundary.quote_mint.key.as_ref(),
        &risk_pool_id_bytes,
        &pool_bump,
    ];
    allocate_pda(
        boundary.admin,
        boundary.pool,
        boundary.system_program,
        program_id,
        account_len::<PoolState>(),
        pool_signer_seeds,
    )?;

    let page_bump = [boundary.page_bump];
    let page_signer_seeds: &[&[u8]] = &[
        QUEUE_SEED,
        boundary.pool.key.as_ref(),
        &PAGE_ZERO,
        &page_bump,
    ];
    allocate_pda(
        boundary.admin,
        boundary.page_zero,
        boundary.system_program,
        program_id,
        account_len::<QueuePageState>(),
        page_signer_seeds,
    )
}

#[inline(never)]
fn process_initialize_pool<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    risk_pool_id: u64,
) -> ProgramResult {
    let boundary = validate_common(program_id, accounts, risk_pool_id)?;
    validate_prepared_target(boundary.pool, program_id, account_len::<PoolState>())?;
    validate_prepared_target(
        boundary.page_zero,
        program_id,
        account_len::<QueuePageState>(),
    )?;

    let (vault_authority, _) = derive_vault_authority(program_id, boundary.pool.key);
    let vault = derive_canonical_vault(&vault_authority, boundary.quote_mint.key);
    let pool_state = PoolState::new(
        risk_pool_id,
        Key32::new(boundary.quote_mint.key.to_bytes()),
        Key32::new(vault.to_bytes()),
        Key32::new(boundary.admin.key.to_bytes()),
    );
    let pool_key = Key32::new(boundary.pool.key.to_bytes());

    initialize_account(
        &mut boundary.pool.try_borrow_mut_data()?,
        boundary.pool_bump,
        &pool_state,
    )?;
    initialize_queue_page_zero_account(
        &mut boundary.page_zero.try_borrow_mut_data()?,
        boundary.page_bump,
        pool_key,
    )?;

    let pool_data = boundary.pool.try_borrow_data()?;
    let page_data = boundary.page_zero.try_borrow_data()?;
    let pool_header = decode_account_header::<PoolState>(&pool_data)?;
    let page_header = decode_account_header::<QueuePageState>(&page_data)?;
    let decoded_pool = decode_account::<PoolState>(&pool_data)?;
    if pool_header.bump != boundary.pool_bump
        || page_header.bump != boundary.page_bump
        || decoded_pool != pool_state
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    validate_queue_page_zero_account(&page_data, boundary.page_bump, pool_key)?;
    Ok(())
}

#[inline(never)]
fn process_prepare_market<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    market_id: u16,
) -> ProgramResult {
    let boundary = validate_market_identity(program_id, accounts, market_id)?;
    validate_fresh_target(boundary.target)?;

    let market_id_bytes = market_id.to_le_bytes();
    let target_bump = [boundary.target_bump];
    let signer_seeds: &[&[u8]] = &[
        MARKET_SEED,
        boundary.pool.key.as_ref(),
        &market_id_bytes,
        &target_bump,
    ];
    allocate_pda(
        boundary.authority,
        boundary.target,
        boundary.system_program,
        program_id,
        account_len::<MarketRiskState>(),
        signer_seeds,
    )
}

#[inline(never)]
fn process_initialize_market<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    market_id: u16,
    divergence_limit_bps: u16,
    max_long_open_interest: u128,
    max_short_open_interest: u128,
    haircut: HaircutConfig,
) -> ProgramResult {
    let boundary = validate_market_registration(
        program_id,
        accounts,
        market_id,
        divergence_limit_bps,
        max_long_open_interest,
        max_short_open_interest,
        haircut,
    )?;
    validate_prepared_target(
        boundary.target,
        program_id,
        account_len::<MarketRiskState>(),
    )?;

    let state = MarketRiskState::new(
        Key32::new(boundary.pool.key.to_bytes()),
        &boundary.pool_state,
        market_id,
        divergence_limit_bps,
        max_long_open_interest,
        max_short_open_interest,
        haircut,
    )?;
    initialize_account(
        &mut boundary.target.try_borrow_mut_data()?,
        boundary.target_bump,
        &state,
    )?;

    let data = boundary.target.try_borrow_data()?;
    let header = decode_account_header::<MarketRiskState>(&data)?;
    let decoded = decode_account::<MarketRiskState>(&data)?;
    if header.bump != boundary.target_bump
        || decoded != state
        || decoded.pool != Key32::new(boundary.pool.key.to_bytes())
        || decoded.market_id.get() != market_id
        || decoded.status != PoolStatus::Active as u8
        || decoded.long_open_interest.get() != 0
        || decoded.short_open_interest.get() != 0
        || decoded.reserved_profit.get() != 0
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_prepare_controller<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> ProgramResult {
    let boundary = validate_controller_registration(program_id, accounts)?;
    validate_fresh_target(boundary.target)?;

    let target_bump = [boundary.target_bump];
    let signer_seeds: &[&[u8]] = &[
        CONTROLLER_SEED,
        boundary.pool.key.as_ref(),
        boundary.authority.key.as_ref(),
        &target_bump,
    ];
    allocate_pda(
        boundary.authority,
        boundary.target,
        boundary.system_program,
        program_id,
        account_len::<ControllerRiskState>(),
        signer_seeds,
    )
}

#[inline(never)]
fn process_initialize_controller<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> ProgramResult {
    let boundary = validate_controller_registration(program_id, accounts)?;
    validate_prepared_target(
        boundary.target,
        program_id,
        account_len::<ControllerRiskState>(),
    )?;

    let state = ControllerRiskState::new(
        Key32::new(boundary.pool.key.to_bytes()),
        Key32::new(boundary.authority.key.to_bytes()),
    )?;
    initialize_account(
        &mut boundary.target.try_borrow_mut_data()?,
        boundary.target_bump,
        &state,
    )?;

    let data = boundary.target.try_borrow_data()?;
    let header = decode_account_header::<ControllerRiskState>(&data)?;
    let decoded = decode_account::<ControllerRiskState>(&data)?;
    if header.bump != boundary.target_bump
        || decoded != state
        || decoded.pool != Key32::new(boundary.pool.key.to_bytes())
        || decoded.controller != Key32::new(boundary.authority.key.to_bytes())
        || decoded.available_cash_collateral.get() != 0
        || decoded.reserved_profit.get() != 0
        || decoded.queued_claim_face.get() != 0
        || decoded.claim_collateral_in_use.get() != 0
        || decoded.open_positions.get() != 0
        || decoded.active_claims.get() != 0
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_initialize_vault<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> ProgramResult {
    if accounts.len() != INITIALIZE_VAULT_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let admin = &accounts[0];
    let pool = &accounts[1];
    let quote_mint = &accounts[2];
    let vault_authority = &accounts[3];
    let vault = &accounts[4];
    let system_program_ai = &accounts[5];
    let token_program = &accounts[6];
    let associated_token_program = &accounts[7];

    validate_payer_authority(admin)?;
    expect_readonly(pool)?;
    let pool_state = load_canonical_pool(program_id, pool)?;
    if pool_state.admin.0 != admin.key.to_bytes()
        || pool_state.quote_mint.0 != quote_mint.key.to_bytes()
    {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    expect_readonly(quote_mint)?;
    validate_quote_mint(quote_mint)?;
    expect_readonly(vault_authority)?;
    if vault_authority.executable {
        return Err(BoundaryError::InvalidVault.into());
    }
    let (expected_authority, _) = derive_vault_authority(program_id, pool.key);
    let expected_vault = derive_canonical_vault(&expected_authority, quote_mint.key);
    if vault_authority.key != &expected_authority
        || vault.key != &expected_vault
        || pool_state.vault.0 != expected_vault.to_bytes()
    {
        return Err(BoundaryError::InvalidVault.into());
    }
    expect_writable(vault)?;
    expect_nonsigner(vault)?;
    validate_external_program(system_program_ai, &system_program::ID)?;
    validate_external_program(token_program, &spl_token::ID)?;
    validate_external_program(associated_token_program, &ASSOCIATED_TOKEN_PROGRAM_ID)?;

    if vault.owner == &spl_token::ID && vault.data_len() == TokenAccount::LEN {
        validate_vault_token_account(vault, quote_mint.key, vault_authority.key)?;
        return Ok(());
    }
    if vault.owner != &system_program::ID || vault.executable || vault.data_len() != 0 {
        return Err(BoundaryError::InvalidVault.into());
    }

    let create = SolanaInstruction {
        program_id: ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: alloc::vec![
            AccountMeta::new(*admin.key, true),
            AccountMeta::new(*vault.key, false),
            AccountMeta::new_readonly(*vault_authority.key, false),
            AccountMeta::new_readonly(*quote_mint.key, false),
            AccountMeta::new_readonly(system_program::ID, false),
            AccountMeta::new_readonly(spl_token::ID, false),
        ],
        // Canonical Associated Token Account CreateIdempotent discriminator.
        data: alloc::vec![1],
    };
    invoke(
        &create,
        &[
            admin.clone(),
            vault.clone(),
            vault_authority.clone(),
            quote_mint.clone(),
            system_program_ai.clone(),
            token_program.clone(),
            associated_token_program.clone(),
        ],
    )?;
    validate_vault_token_account(vault, quote_mint.key, vault_authority.key)?;
    Ok(())
}

#[inline(never)]
fn process_deposit_cash<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    amount: u64,
) -> ProgramResult {
    if accounts.len() != DEPOSIT_CASH_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let controller = &accounts[2];
    let source = &accounts[3];
    let vault = &accounts[4];
    let quote_mint = &accounts[5];
    let token_program = &accounts[6];

    validate_controller_authority(authority)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool(program_id, pool)?;
    if pool_state.quote_mint.0 != quote_mint.key.to_bytes()
        || pool_state.vault.0 != vault.key.to_bytes()
    {
        return Err(BoundaryError::InvalidPool.into());
    }
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, authority.key, controller)?;
    expect_writable(source)?;
    expect_nonsigner(source)?;
    expect_writable(vault)?;
    expect_nonsigner(vault)?;
    expect_readonly(quote_mint)?;
    validate_quote_mint(quote_mint)?;
    validate_external_program(token_program, &spl_token::ID)?;

    let (vault_authority, _) = derive_vault_authority(program_id, pool.key);
    let expected_vault = derive_canonical_vault(&vault_authority, quote_mint.key);
    if vault.key != &expected_vault {
        return Err(BoundaryError::InvalidVault.into());
    }
    let source_before = validate_source_token_account(source, quote_mint.key, authority.key)?;
    let vault_before = validate_vault_token_account(vault, quote_mint.key, &vault_authority)?;
    let mint_supply_before = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    let outcome = record_cash_deposit(
        Key32::new(pool.key.to_bytes()),
        &mut pool_state,
        &mut controller_state,
        vault_before.amount,
        amount,
    )?;

    let transfer = spl_token::instruction::transfer_checked(
        &spl_token::ID,
        source.key,
        quote_mint.key,
        vault.key,
        authority.key,
        &[],
        amount,
        V1_QUOTE_DECIMALS,
    )?;
    invoke(
        &transfer,
        &[
            source.clone(),
            quote_mint.clone(),
            vault.clone(),
            authority.clone(),
            token_program.clone(),
        ],
    )?;

    let source_after = validate_source_token_account(source, quote_mint.key, authority.key)?;
    let vault_after = validate_vault_token_account(vault, quote_mint.key, &vault_authority)?;
    let mint_supply_after = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    if source_after.amount
        != source_before
            .amount
            .checked_sub(amount)
            .ok_or(BoundaryError::InvalidTokenDelta)?
        || vault_after.amount
            != vault_before
                .amount
                .checked_add(amount)
                .ok_or(BoundaryError::InvalidTokenDelta)?
        || mint_supply_after != mint_supply_before
        || vault_after.amount != outcome.represented_custody_after
    {
        return Err(BoundaryError::InvalidTokenDelta.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    let persisted_pool = decode_account::<PoolState>(&pool.try_borrow_data()?)?;
    let persisted_controller =
        decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?;
    if persisted_pool != pool_state
        || persisted_controller != controller_state
        || persisted_pool.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

struct OraclePolicyRegistrationAccounts<'a, 'info> {
    authority: &'a AccountInfo<'info>,
    pool: &'a AccountInfo<'info>,
    market: &'a AccountInfo<'info>,
    target: &'a AccountInfo<'info>,
    system_program: &'a AccountInfo<'info>,
    pool_state: PoolState,
    market_state: MarketRiskState,
    target_bump: u8,
}

#[inline(never)]
fn process_prepare_oracle_policy<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    market_id: u16,
) -> ProgramResult {
    let boundary = validate_oracle_policy_registration(program_id, accounts, market_id)?;
    validate_fresh_target(boundary.target)?;

    let target_bump = [boundary.target_bump];
    let signer_seeds: &[&[u8]] = &[
        ORACLE_POLICY_SEED,
        boundary.market.key.as_ref(),
        &target_bump,
    ];
    allocate_pda(
        boundary.authority,
        boundary.target,
        boundary.system_program,
        program_id,
        account_len::<OraclePolicyState>(),
        signer_seeds,
    )
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn process_initialize_oracle_policy<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    market_id: u16,
    primary_feed_id: [u8; 32],
    secondary_feed_id: [u8; 32],
    max_staleness_secs: u64,
    confidence_limit_bps: u16,
) -> ProgramResult {
    let boundary = validate_oracle_policy_registration(program_id, accounts, market_id)?;
    validate_prepared_target(
        boundary.target,
        program_id,
        account_len::<OraclePolicyState>(),
    )?;
    if boundary.pool_state.status != PoolStatus::Active as u8
        || boundary.market_state.status != PoolStatus::Active as u8
    {
        return Err(BoundaryError::InvalidOraclePolicy.into());
    }

    let state = OraclePolicyState::new(
        Key32::new(boundary.pool.key.to_bytes()),
        Key32::new(boundary.market.key.to_bytes()),
        Key32::new(primary_feed_id),
        Key32::new(secondary_feed_id),
        max_staleness_secs,
        confidence_limit_bps,
    )?;
    initialize_account(
        &mut boundary.target.try_borrow_mut_data()?,
        boundary.target_bump,
        &state,
    )?;

    let data = boundary.target.try_borrow_data()?;
    let header = decode_account_header::<OraclePolicyState>(&data)?;
    let decoded = decode_account::<OraclePolicyState>(&data)?;
    if header.bump != boundary.target_bump || decoded != state {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

fn validate_oracle_policy_registration<'a, 'info>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'info>],
    market_id: u16,
) -> Result<OraclePolicyRegistrationAccounts<'a, 'info>, ProgramError> {
    if accounts.len() != ORACLE_POLICY_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    let target = &accounts[3];
    let system_program_ai = &accounts[4];

    validate_payer_authority(authority)?;
    expect_readonly(pool)?;
    let pool_state = load_canonical_pool(program_id, pool)?;
    if pool_state.admin.0 != authority.key.to_bytes() {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    expect_readonly(market)?;
    let market_state = load_canonical_market(program_id, pool.key, market_id, market)?;
    expect_writable(target)?;
    expect_nonsigner(target)?;
    validate_external_program(system_program_ai, &system_program::ID)?;
    let (expected_target, target_bump) = derive_oracle_policy(program_id, market.key);
    if target.key != &expected_target {
        return Err(BoundaryError::InvalidPda.into());
    }
    Ok(OraclePolicyRegistrationAccounts {
        authority,
        pool,
        market,
        target,
        system_program: system_program_ai,
        pool_state,
        market_state,
        target_bump,
    })
}

struct OracleRelayRegistrationAccounts<'a, 'info> {
    authority: &'a AccountInfo<'info>,
    pool: &'a AccountInfo<'info>,
    market: &'a AccountInfo<'info>,
    target: &'a AccountInfo<'info>,
    system_program: &'a AccountInfo<'info>,
    target_bump: u8,
}

fn validate_oracle_relay_registration<'a, 'info>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'info>],
    market_id: u16,
    feed_id: &[u8; 32],
) -> Result<OracleRelayRegistrationAccounts<'a, 'info>, ProgramError> {
    if accounts.len() != ORACLE_RELAY_REGISTRATION_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    let target = &accounts[3];
    let system_program_ai = &accounts[4];

    validate_payer_authority(authority)?;
    expect_readonly(pool)?;
    let pool_state = load_canonical_pool(program_id, pool)?;
    if pool_state.admin.0 != authority.key.to_bytes()
        || pool_state.status != PoolStatus::Active as u8
    {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    expect_readonly(market)?;
    let market_state = load_canonical_market(program_id, pool.key, market_id, market)?;
    if market_state.status != PoolStatus::Active as u8 {
        return Err(BoundaryError::InvalidMarket.into());
    }
    expect_writable(target)?;
    expect_nonsigner(target)?;
    validate_external_program(system_program_ai, &system_program::ID)?;
    let (expected_target, target_bump) = derive_oracle_relay(program_id, market.key, feed_id);
    if target.key != &expected_target {
        return Err(BoundaryError::InvalidPda.into());
    }
    Ok(OracleRelayRegistrationAccounts {
        authority,
        pool,
        market,
        target,
        system_program: system_program_ai,
        target_bump,
    })
}

#[inline(never)]
fn process_prepare_oracle_relay<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    market_id: u16,
    feed_id: [u8; 32],
) -> ProgramResult {
    let boundary = validate_oracle_relay_registration(program_id, accounts, market_id, &feed_id)?;
    validate_fresh_target(boundary.target)?;
    let bump = [boundary.target_bump];
    let signer_seeds: &[&[u8]] = &[
        ORACLE_RELAY_SEED,
        boundary.market.key.as_ref(),
        &feed_id,
        &bump,
    ];
    allocate_pda(
        boundary.authority,
        boundary.target,
        boundary.system_program,
        program_id,
        account_len::<OracleRelayState>(),
        signer_seeds,
    )
}

#[inline(never)]
fn process_initialize_oracle_relay<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    market_id: u16,
    feed_id: [u8; 32],
    relay_authority: [u8; 32],
    source: OracleRelaySource,
    price_e6_per_tick: u64,
    max_source_staleness_slots: u64,
    max_spread_bps: u16,
    max_mark_index_divergence_bps: u16,
) -> ProgramResult {
    if source == OracleRelaySource::PhoenixHawkeye
        && relay_authority != PHOENIX_MAINNET_PROGRAM_ID.to_bytes()
        && relay_authority != PHOENIX_BETA_PROGRAM_ID.to_bytes()
    {
        return Err(BoundaryError::InvalidOracle.into());
    }
    let boundary = validate_oracle_relay_registration(program_id, accounts, market_id, &feed_id)?;
    validate_prepared_target(
        boundary.target,
        program_id,
        account_len::<OracleRelayState>(),
    )?;
    let state = OracleRelayState::new(
        Key32::new(boundary.pool.key.to_bytes()),
        Key32::new(boundary.market.key.to_bytes()),
        Key32::new(feed_id),
        Key32::new(relay_authority),
        source,
        price_e6_per_tick,
        max_source_staleness_slots,
        max_spread_bps,
        max_mark_index_divergence_bps,
    )?;
    initialize_account(
        &mut boundary.target.try_borrow_mut_data()?,
        boundary.target_bump,
        &state,
    )?;
    let data = boundary.target.try_borrow_data()?;
    let header = decode_account_header::<OracleRelayState>(&data)?;
    if header.bump != boundary.target_bump || decode_account::<OracleRelayState>(&data)? != state {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_update_oracle_relay<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    price_e6: u64,
    confidence_e6: u64,
    publish_time: i64,
    source_posted_slot: u64,
) -> ProgramResult {
    if accounts.len() != UPDATE_ORACLE_RELAY_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let relay = &accounts[1];
    validate_controller_authority(authority)?;
    expect_writable(relay)?;
    expect_nonsigner(relay)?;
    let mut state = load_canonical_oracle_relay(program_id, relay)?;
    let source = OracleRelaySource::try_from(state.source)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    if !matches!(
        source,
        OracleRelaySource::MagicBlockDemo | OracleRelaySource::PhoenixMainnetRelay
    ) || state.authority.0 != authority.key.to_bytes()
    {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    let clock = Clock::get()?;
    if publish_time
        > clock
            .unix_timestamp
            .saturating_add(ORACLE_RELAY_FUTURE_TOLERANCE_SECS)
        || clock.unix_timestamp.saturating_sub(publish_time) > ORACLE_RELAY_MAX_INGRESS_AGE_SECS
    {
        return Err(BoundaryError::InvalidOracle.into());
    }
    state
        .record_observation(
            price_e6,
            confidence_e6,
            publish_time,
            source_posted_slot,
            clock.slot,
        )
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    overwrite_account(&mut relay.try_borrow_mut_data()?, &state)?;
    if decode_account::<OracleRelayState>(&relay.try_borrow_data()?)? != state {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhoenixBboReturn {
    pub best_bid_ticks: u64,
    pub best_ask_ticks: u64,
    pub mark_price_ticks: u64,
    pub index_price_ticks: u64,
    pub mark_price_last_updated_slot: u64,
    pub index_price_last_updated_slot: u64,
}

pub fn decode_phoenix_bbo_return(data: &[u8]) -> Result<PhoenixBboReturn, ProgramError> {
    if data.len() != PHOENIX_VIEW_BBO_RETURN_LEN
        || u64::from_le_bytes(
            data[0..8]
                .try_into()
                .map_err(|_| BoundaryError::InvalidOracle)?,
        ) != PHOENIX_VIEW_BBO_RETURN_MAGIC
        || u16::from_le_bytes(
            data[8..10]
                .try_into()
                .map_err(|_| BoundaryError::InvalidOracle)?,
        ) != PHOENIX_HAWKEYE_RETURN_VERSION
        || data[10] != (PHOENIX_VIEW_BBO_HAS_BID | PHOENIX_VIEW_BBO_HAS_ASK)
        || data[11..16].iter().any(|byte| *byte != 0)
    {
        return Err(BoundaryError::InvalidOracle.into());
    }
    let read_u64 = |range: core::ops::Range<usize>| -> Result<u64, ProgramError> {
        Ok(u64::from_le_bytes(data[range].try_into().map_err(
            |_| ProgramError::from(BoundaryError::InvalidOracle),
        )?))
    };
    let decoded = PhoenixBboReturn {
        best_bid_ticks: read_u64(16..24)?,
        best_ask_ticks: read_u64(24..32)?,
        mark_price_ticks: read_u64(32..40)?,
        index_price_ticks: read_u64(40..48)?,
        mark_price_last_updated_slot: read_u64(48..56)?,
        index_price_last_updated_slot: read_u64(56..64)?,
    };
    if decoded.best_bid_ticks == 0
        || decoded.best_ask_ticks == 0
        || decoded.best_bid_ticks > decoded.best_ask_ticks
        || decoded.mark_price_ticks == 0
        || decoded.index_price_ticks == 0
        || decoded.mark_price_last_updated_slot == 0
        || decoded.index_price_last_updated_slot == 0
    {
        return Err(BoundaryError::InvalidOracle.into());
    }
    Ok(decoded)
}

fn phoenix_spread_bps_ceil(best_bid_ticks: u64, best_ask_ticks: u64) -> Option<u64> {
    if best_bid_ticks == 0 || best_ask_ticks < best_bid_ticks {
        return None;
    }
    if best_bid_ticks == best_ask_ticks {
        return Some(0);
    }
    let numerator = u128::from(best_ask_ticks.checked_sub(best_bid_ticks)?).checked_mul(20_000)?;
    let denominator = u128::from(best_ask_ticks).checked_add(u128::from(best_bid_ticks))?;
    let bps = numerator
        .checked_add(denominator.checked_sub(1)?)?
        .checked_div(denominator)?;
    u64::try_from(bps).ok()
}

fn validate_phoenix_data_account(account: &AccountInfo<'_>, owner: &Pubkey) -> ProgramResult {
    expect_readonly(account)?;
    expect_nonsigner(account)?;
    if account.executable || account.owner != owner || account.data_len() == 0 {
        return Err(BoundaryError::InvalidOracle.into());
    }
    Ok(())
}

#[inline(never)]
fn process_refresh_phoenix_oracle<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    global_trader_index_count: u8,
    active_trader_buffer_count: u8,
) -> ProgramResult {
    let global_trader_index_count = usize::from(global_trader_index_count);
    let active_trader_buffer_count = usize::from(active_trader_buffer_count);
    if global_trader_index_count == 0
        || global_trader_index_count > MAX_PHOENIX_DYNAMIC_ACCOUNTS_PER_KIND
        || active_trader_buffer_count == 0
        || active_trader_buffer_count > MAX_PHOENIX_DYNAMIC_ACCOUNTS_PER_KIND
        || accounts.len()
            != REFRESH_PHOENIX_FIXED_ACCOUNT_COUNT
                .checked_add(global_trader_index_count)
                .and_then(|count| count.checked_add(active_trader_buffer_count))
                .ok_or(BoundaryError::ArithmeticOverflow)?
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;

    let relay = &accounts[0];
    let hawkeye_program = &accounts[1];
    let phoenix_program = &accounts[2];
    let global_config = &accounts[3];
    let global_trader_index_start = 4;
    let active_trader_buffer_start = global_trader_index_start + global_trader_index_count;
    let perp_asset_map_index = active_trader_buffer_start + active_trader_buffer_count;
    let orderbook_index = perp_asset_map_index + 1;
    let spline_collection_index = orderbook_index + 1;
    let phoenix_data_accounts = &accounts[3..=spline_collection_index];
    let orderbook = &accounts[orderbook_index];

    expect_writable(relay)?;
    expect_nonsigner(relay)?;
    validate_external_program(hawkeye_program, &HAWKEYE_PROGRAM_ID)?;
    expect_readonly(phoenix_program)?;
    expect_nonsigner(phoenix_program)?;
    if !phoenix_program.executable {
        return Err(BoundaryError::InvalidProgram.into());
    }
    let expected_global_config = if phoenix_program.key == &PHOENIX_MAINNET_PROGRAM_ID {
        PHOENIX_MAINNET_GLOBAL_CONFIG
    } else if phoenix_program.key == &PHOENIX_BETA_PROGRAM_ID {
        PHOENIX_BETA_GLOBAL_CONFIG
    } else {
        return Err(BoundaryError::InvalidProgram.into());
    };
    if global_config.key != &expected_global_config {
        return Err(BoundaryError::InvalidOracle.into());
    }
    for account in phoenix_data_accounts {
        validate_phoenix_data_account(account, phoenix_program.key)?;
    }

    let mut state = load_canonical_oracle_relay(program_id, relay)?;
    if OracleRelaySource::try_from(state.source) != Ok(OracleRelaySource::PhoenixHawkeye)
        || state.authority.0 != phoenix_program.key.to_bytes()
        || state.feed_id.0 != orderbook.key.to_bytes()
    {
        return Err(BoundaryError::InvalidOracle.into());
    }

    let mut cpi_metas = Vec::with_capacity(accounts.len().saturating_sub(2));
    let mut cpi_infos = Vec::with_capacity(accounts.len().saturating_sub(1));
    for account in &accounts[2..] {
        cpi_metas.push(AccountMeta::new_readonly(*account.key, false));
        cpi_infos.push(account.clone());
    }
    cpi_infos.push(hawkeye_program.clone());
    invoke(
        &SolanaInstruction {
            program_id: HAWKEYE_PROGRAM_ID,
            accounts: cpi_metas,
            data: PHOENIX_VIEW_BBO_DISCRIMINANT.to_vec(),
        },
        &cpi_infos,
    )?;

    let (return_program_id, return_data) = get_return_data().ok_or(BoundaryError::InvalidOracle)?;
    if return_program_id != HAWKEYE_PROGRAM_ID {
        return Err(BoundaryError::InvalidOracle.into());
    }
    let observation = decode_phoenix_bbo_return(&return_data)?;
    let clock = Clock::get()?;
    if observation.mark_price_last_updated_slot > clock.slot
        || observation.index_price_last_updated_slot > clock.slot
        || clock
            .slot
            .saturating_sub(observation.mark_price_last_updated_slot)
            > state.max_source_staleness_slots.get()
        || clock
            .slot
            .saturating_sub(observation.index_price_last_updated_slot)
            > state.max_source_staleness_slots.get()
    {
        return Err(BoundaryError::InvalidOracle.into());
    }
    let spread_bps =
        phoenix_spread_bps_ceil(observation.best_bid_ticks, observation.best_ask_ticks)
            .ok_or(BoundaryError::ArithmeticOverflow)?;
    if spread_bps > u64::from(state.max_spread_bps.get()) {
        return Err(BoundaryError::OracleDivergence.into());
    }
    let mark_index_divergence =
        price_divergence_bps_ceil(observation.mark_price_ticks, observation.index_price_ticks)
            .ok_or(BoundaryError::ArithmeticOverflow)?;
    if mark_index_divergence > u64::from(state.max_mark_index_divergence_bps.get()) {
        return Err(BoundaryError::OracleDivergence.into());
    }

    let scale = state.price_e6_per_tick.get();
    let price_e6 = observation
        .mark_price_ticks
        .checked_mul(scale)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    let half_spread_ticks = observation
        .best_ask_ticks
        .checked_sub(observation.best_bid_ticks)
        .and_then(|spread| spread.checked_add(1))
        .map(|spread| spread / 2)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    let uncertainty_ticks = core::cmp::max(
        half_spread_ticks,
        observation
            .mark_price_ticks
            .abs_diff(observation.index_price_ticks),
    );
    let confidence_e6 = uncertainty_ticks
        .checked_mul(scale)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    let source_posted_slot = core::cmp::min(
        observation.mark_price_last_updated_slot,
        observation.index_price_last_updated_slot,
    );
    state
        .record_observation(
            price_e6,
            confidence_e6,
            clock.unix_timestamp,
            source_posted_slot,
            clock.slot,
        )
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    overwrite_account(&mut relay.try_borrow_mut_data()?, &state)?;
    if decode_account::<OracleRelayState>(&relay.try_borrow_data()?)? != state {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

struct PositionPreparationAccounts<'a, 'info> {
    authority: &'a AccountInfo<'info>,
    pool: &'a AccountInfo<'info>,
    target: &'a AccountInfo<'info>,
    system_program: &'a AccountInfo<'info>,
    target_bump: u8,
}

#[inline(never)]
fn process_prepare_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    controller_override: Option<&Pubkey>,
) -> ProgramResult {
    let boundary =
        validate_position_preparation(program_id, accounts, position_id, controller_override)?;
    validate_fresh_target(boundary.target)?;

    let position_id_bytes = position_id.to_le_bytes();
    let target_bump = [boundary.target_bump];
    let signer_seeds: &[&[u8]] = &[
        POSITION_SEED,
        boundary.pool.key.as_ref(),
        controller_override
            .unwrap_or(boundary.authority.key)
            .as_ref(),
        &position_id_bytes,
        &target_bump,
    ];
    allocate_pda(
        boundary.authority,
        boundary.target,
        boundary.system_program,
        program_id,
        account_len::<PositionState>(),
        signer_seeds,
    )
}

fn validate_position_preparation<'a, 'info>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'info>],
    position_id: u64,
    controller_override: Option<&Pubkey>,
) -> Result<PositionPreparationAccounts<'a, 'info>, ProgramError> {
    if accounts.len() != PREPARE_POSITION_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let controller = &accounts[2];
    let target = &accounts[3];
    let system_program_ai = &accounts[4];
    let controller_key = controller_override.unwrap_or(authority.key);

    validate_payer_authority(authority)?;
    // The bundled open instruction mutates pool/controller, so their
    // transaction-wide privileges remain writable during preparation.
    expect_nonsigner(pool)?;
    load_canonical_pool(program_id, pool)?;
    expect_nonsigner(controller)?;
    load_canonical_controller(program_id, pool.key, controller_key, controller)?;
    expect_writable(target)?;
    expect_nonsigner(target)?;
    validate_external_program(system_program_ai, &system_program::ID)?;
    let (expected_target, target_bump) =
        derive_position(program_id, pool.key, controller_key, position_id);
    if target.key != &expected_target {
        return Err(BoundaryError::InvalidPda.into());
    }
    Ok(PositionPreparationAccounts {
        authority,
        pool,
        target,
        system_program: system_program_ai,
        target_bump,
    })
}

#[inline(never)]
fn process_prepare_claim<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    controller_override: Option<&Pubkey>,
) -> ProgramResult {
    if accounts.len() != PREPARE_CLAIM_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let position = &accounts[1];
    let claim = &accounts[2];
    let system_program_ai = &accounts[3];
    let controller_key = controller_override.unwrap_or(authority.key);
    validate_payer_authority(authority)?;
    expect_writable(position)?;
    expect_nonsigner(position)?;
    let position_state = load_self_canonical_position(program_id, position)?;
    if position_state.position_id.get() != position_id
        || position_state.controller.0 != controller_key.to_bytes()
        || position_state.status != crate::martingaler_state::PositionStatus::Open as u8
    {
        return Err(BoundaryError::InvalidPosition.into());
    }
    expect_writable(claim)?;
    expect_nonsigner(claim)?;
    let (expected_claim, claim_bump) = derive_claim(program_id, position.key);
    if claim.key != &expected_claim {
        return Err(BoundaryError::InvalidPda.into());
    }
    validate_external_program(system_program_ai, &system_program::ID)?;
    validate_fresh_target(claim)?;

    let bump = [claim_bump];
    let signer_seeds: &[&[u8]] = &[CLAIM_SEED, position.key.as_ref(), &bump];
    allocate_pda(
        authority,
        claim,
        system_program_ai,
        program_id,
        account_len::<ClaimState>(),
        signer_seeds,
    )?;
    validate_prepared_target(claim, program_id, account_len::<ClaimState>())
}

#[inline(never)]
fn process_prepare_queue_page<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    page_index: u64,
) -> ProgramResult {
    if accounts.len() != PREPARE_QUEUE_PAGE_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let page = &accounts[2];
    let system_program_ai = &accounts[3];
    validate_payer_authority(authority)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let pool_state = load_canonical_pool_for_close(program_id, pool)?;
    if page_index != pool_state.next_page_index.get() {
        return Err(BoundaryError::InvalidQueuePage.into());
    }
    expect_writable(page)?;
    expect_nonsigner(page)?;
    let (expected_page, page_bump) = derive_queue_page(program_id, pool.key, page_index);
    if page.key != &expected_page {
        return Err(BoundaryError::InvalidPda.into());
    }
    validate_external_program(system_program_ai, &system_program::ID)?;
    validate_fresh_target(page)?;

    let index = page_index.to_le_bytes();
    let bump = [page_bump];
    let signer_seeds: &[&[u8]] = &[QUEUE_SEED, pool.key.as_ref(), &index, &bump];
    allocate_pda(
        authority,
        page,
        system_program_ai,
        program_id,
        account_len::<QueuePageState>(),
        signer_seeds,
    )?;
    validate_prepared_target(page, program_id, account_len::<QueuePageState>())
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn process_open_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    market_id: u16,
    side: PositionSide,
    collateral: u64,
    notional: u64,
    leverage_x: u16,
    min_payout_factor_bps: u16,
    controller_override: Option<&Pubkey>,
) -> ProgramResult {
    if accounts.len() != OPEN_CASH_BASE_ACCOUNT_COUNT
        && accounts.len() != OPEN_CASH_BASE_ACCOUNT_COUNT + 1
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    let controller = &accounts[3];
    let position = &accounts[4];
    let oracle_policy = &accounts[5];
    let vault = &accounts[6];
    let primary_oracle = &accounts[7];
    let controller_key = controller_override.unwrap_or(authority.key);

    // The same authority funds the canonical position PDA in the preceding
    // prepare instruction, so transaction-wide privileges keep it writable.
    validate_payer_authority(authority)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool(program_id, pool)?;
    expect_writable(market)?;
    expect_nonsigner(market)?;
    let mut market_state = load_canonical_market(program_id, pool.key, market_id, market)?;
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, controller_key, controller)?;
    expect_writable(position)?;
    expect_nonsigner(position)?;
    let (expected_position, position_bump) =
        derive_position(program_id, pool.key, controller_key, position_id);
    if position.key != &expected_position {
        return Err(BoundaryError::InvalidPda.into());
    }
    validate_prepared_target(position, program_id, account_len::<PositionState>())?;
    expect_readonly(oracle_policy)?;
    let policy_state =
        load_canonical_oracle_policy(program_id, pool.key, market.key, oracle_policy)?;
    let expected_account_count = OPEN_CASH_BASE_ACCOUNT_COUNT
        .checked_add(usize::from(policy_state.oracle_count - 1))
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != expected_account_count {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    expect_readonly(vault)?;
    expect_nonsigner(vault)?;
    let (vault_authority, _) = derive_vault_authority(program_id, pool.key);
    let quote_mint = Pubkey::new_from_array(pool_state.quote_mint.0);
    let expected_vault = derive_canonical_vault(&vault_authority, &quote_mint);
    if vault.key != &expected_vault || pool_state.vault.0 != expected_vault.to_bytes() {
        return Err(BoundaryError::InvalidVault.into());
    }
    let vault_state = validate_vault_token_account(vault, &quote_mint, &vault_authority)?;

    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (primary_observation, secondary_observation) = read_policy_prices(
        program_id,
        &policy_state,
        &market_state,
        primary_oracle,
        (policy_state.oracle_count == 2).then(|| &accounts[8]),
        now_unix_ts,
    )?;
    let (entry_price_e6, entry_publish_time) = primary_observation;
    let (secondary_entry_price_e6, secondary_publish_time) =
        secondary_observation.unwrap_or((0, 0));

    let outcome = open_cash_position_from_idle_guarded(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(controller_key.to_bytes()),
        &mut pool_state,
        &mut market_state,
        &mut controller_state,
        vault_state.amount,
        position_id,
        side,
        u128::from(collateral),
        u128::from(notional),
        leverage_x,
        entry_price_e6,
        entry_publish_time,
        secondary_entry_price_e6,
        secondary_publish_time,
        min_payout_factor_bps,
    )?;

    initialize_account(
        &mut position.try_borrow_mut_data()?,
        position_bump,
        &outcome.position,
    )?;
    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;

    let persisted_pool = decode_account::<PoolState>(&pool.try_borrow_data()?)?;
    let persisted_market = decode_account::<MarketRiskState>(&market.try_borrow_data()?)?;
    let persisted_controller =
        decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?;
    let persisted_position = decode_account::<PositionState>(&position.try_borrow_data()?)?;
    let persisted_header = decode_account_header::<PositionState>(&position.try_borrow_data()?)?;
    if persisted_pool != pool_state
        || persisted_market != market_state
        || persisted_controller != controller_state
        || persisted_position != outcome.position
        || persisted_header.bump != position_bump
        || persisted_pool.represented_quote_custody()? != u128::from(vault_state.amount)
        || outcome.represented_custody_after != vault_state.amount
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    emit_martingaler_history(&encode_martingaler_open_history_receipt(
        pool.key,
        position.key,
        &outcome.position,
        market_state.max_net_profit_per_position.get(),
    )?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn process_open_claim_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    market_id: u16,
    side: PositionSide,
    claim_amount: u64,
    notional: u64,
    leverage_x: u16,
    min_payout_factor_bps: u16,
    controller_override: Option<&Pubkey>,
) -> ProgramResult {
    if accounts.len() != OPEN_CLAIM_BASE_ACCOUNT_COUNT
        && accounts.len() != OPEN_CLAIM_BASE_ACCOUNT_COUNT + 1
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    let controller = &accounts[3];
    let source_claim = &accounts[4];
    let source_page = &accounts[5];
    let position = &accounts[6];
    let oracle_policy = &accounts[7];
    let vault = &accounts[8];
    let primary_oracle = &accounts[9];
    let controller_key = controller_override.unwrap_or(authority.key);

    validate_payer_authority(authority)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool(program_id, pool)?;
    expect_writable(market)?;
    expect_nonsigner(market)?;
    let mut market_state = load_canonical_market(program_id, pool.key, market_id, market)?;
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, controller_key, controller)?;
    expect_writable(source_claim)?;
    expect_nonsigner(source_claim)?;
    let mut source_claim_state = load_canonical_claim(program_id, pool.key, source_claim)?;
    expect_writable(source_page)?;
    expect_nonsigner(source_page)?;
    let mut source_page_state = load_canonical_queue_page(
        program_id,
        pool.key,
        source_claim_state.queue_page_index.get(),
        source_page,
    )?;

    expect_writable(position)?;
    expect_nonsigner(position)?;
    let (expected_position, position_bump) =
        derive_position(program_id, pool.key, controller_key, position_id);
    if position.key != &expected_position {
        return Err(BoundaryError::InvalidPda.into());
    }
    validate_prepared_target(position, program_id, account_len::<PositionState>())?;

    expect_readonly(oracle_policy)?;
    let policy_state =
        load_canonical_oracle_policy(program_id, pool.key, market.key, oracle_policy)?;
    let expected_account_count = OPEN_CLAIM_BASE_ACCOUNT_COUNT
        .checked_add(usize::from(policy_state.oracle_count - 1))
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != expected_account_count {
        return Err(BoundaryError::InvalidAccountCount.into());
    }

    expect_readonly(vault)?;
    expect_nonsigner(vault)?;
    let (vault_authority, _) = derive_vault_authority(program_id, pool.key);
    let quote_mint = Pubkey::new_from_array(pool_state.quote_mint.0);
    let expected_vault = derive_canonical_vault(&vault_authority, &quote_mint);
    if vault.key != &expected_vault || pool_state.vault.0 != expected_vault.to_bytes() {
        return Err(BoundaryError::InvalidVault.into());
    }
    let vault_state = validate_vault_token_account(vault, &quote_mint, &vault_authority)?;

    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (primary_observation, secondary_observation) = read_policy_prices(
        program_id,
        &policy_state,
        &market_state,
        primary_oracle,
        (policy_state.oracle_count == 2).then(|| &accounts[10]),
        now_unix_ts,
    )?;
    let (entry_price_e6, entry_publish_time) = primary_observation;
    let (secondary_entry_price_e6, secondary_publish_time) =
        secondary_observation.unwrap_or((0, 0));

    let outcome = open_claim_position_from_queue_guarded(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(controller_key.to_bytes()),
        Key32::new(source_claim.key.to_bytes()),
        &mut pool_state,
        &mut market_state,
        &mut controller_state,
        source_page_state.as_mut(),
        &mut source_claim_state,
        vault_state.amount,
        position_id,
        side,
        u128::from(claim_amount),
        u128::from(notional),
        leverage_x,
        entry_price_e6,
        entry_publish_time,
        secondary_entry_price_e6,
        secondary_publish_time,
        min_payout_factor_bps,
    )?;

    initialize_account(
        &mut position.try_borrow_mut_data()?,
        position_bump,
        &outcome.position,
    )?;
    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    overwrite_account(
        &mut source_claim.try_borrow_mut_data()?,
        &source_claim_state,
    )?;
    overwrite_account(
        &mut source_page.try_borrow_mut_data()?,
        source_page_state.as_ref(),
    )?;

    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || decode_account::<ClaimState>(&source_claim.try_borrow_data()?)? != source_claim_state
        || !account_body_matches(&source_page.try_borrow_data()?, source_page_state.as_ref())?
        || decode_account::<PositionState>(&position.try_borrow_data()?)? != outcome.position
        || decode_account_header::<PositionState>(&position.try_borrow_data()?)?.bump
            != position_bump
        || pool_state.represented_quote_custody()? != u128::from(vault_state.amount)
        || outcome.represented_custody_after != vault_state.amount
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    emit_martingaler_history(&encode_martingaler_open_history_receipt(
        pool.key,
        position.key,
        &outcome.position,
        market_state.max_net_profit_per_position.get(),
    )?);
    Ok(())
}

#[inline(never)]
fn process_close_nonpositive_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    controller_override: Option<&Pubkey>,
) -> ProgramResult {
    if accounts.len() != CLOSE_CASH_ACCOUNT_COUNT && accounts.len() != CLOSE_CASH_ACCOUNT_COUNT + 1
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    let controller = &accounts[3];
    let position = &accounts[4];
    let oracle_policy = &accounts[5];
    let vault_authority = &accounts[6];
    let vault = &accounts[7];
    let destination = &accounts[8];
    let quote_mint = &accounts[9];
    let token_program = &accounts[10];
    let primary_oracle = &accounts[11];
    let controller_key = controller_override.unwrap_or(authority.key);

    validate_controller_authority(authority)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_for_close(program_id, pool)?;
    if pool_state.quote_mint.0 != quote_mint.key.to_bytes()
        || pool_state.vault.0 != vault.key.to_bytes()
    {
        return Err(BoundaryError::InvalidPool.into());
    }
    expect_writable(market)?;
    expect_nonsigner(market)?;
    expect_writable(position)?;
    expect_nonsigner(position)?;
    let mut position_state =
        load_canonical_position(program_id, pool.key, controller_key, position_id, position)?;
    let mut market_state =
        load_canonical_market(program_id, pool.key, position_state.market_id.get(), market)?;
    if !matches!(
        PoolStatus::try_from(market_state.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    ) {
        return Err(BoundaryError::InvalidMarket.into());
    }
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, controller_key, controller)?;
    expect_readonly(oracle_policy)?;
    let policy_state =
        load_canonical_oracle_policy(program_id, pool.key, market.key, oracle_policy)?;
    let expected_account_count = CLOSE_CASH_ACCOUNT_COUNT
        .checked_add(usize::from(policy_state.oracle_count - 1))
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != expected_account_count {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    if (policy_state.oracle_count == 2) != (position_state.secondary_entry_price_e6.get() != 0) {
        return Err(BoundaryError::InvalidOraclePolicy.into());
    }

    expect_readonly(vault_authority)?;
    if vault_authority.executable {
        return Err(BoundaryError::InvalidVault.into());
    }
    let (expected_vault_authority, _) = derive_vault_authority(program_id, pool.key);
    if vault_authority.key != &expected_vault_authority {
        return Err(BoundaryError::InvalidVault.into());
    }
    expect_writable(vault)?;
    expect_nonsigner(vault)?;
    expect_writable(destination)?;
    expect_nonsigner(destination)?;
    expect_readonly(quote_mint)?;
    validate_quote_mint(quote_mint)?;
    validate_external_program(token_program, &spl_token::ID)?;
    let expected_vault = derive_canonical_vault(&expected_vault_authority, quote_mint.key);
    if vault.key != &expected_vault {
        return Err(BoundaryError::InvalidVault.into());
    }
    let vault_before =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_before =
        validate_destination_token_account(destination, quote_mint.key, controller_key)?;
    let mint_supply_before = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;

    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (exit_price_e6, secondary_exit_price_e6) = read_policy_exit_prices(
        program_id,
        &policy_state,
        &market_state,
        primary_oracle,
        (policy_state.oracle_count == 2).then(|| &accounts[12]),
        now_unix_ts,
    )?;
    let position_before = position_state;
    let outcome = close_cash_position(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(controller_key.to_bytes()),
        &mut pool_state,
        &mut market_state,
        &mut controller_state,
        &mut position_state,
        vault_before.amount,
        exit_price_e6,
        secondary_exit_price_e6,
        false,
    )?;

    let vault_after =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_after =
        validate_destination_token_account(destination, quote_mint.key, controller_key)?;
    let mint_supply_after = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    if vault_after.amount != vault_before.amount
        || destination_after.amount != destination_before.amount
        || mint_supply_after != mint_supply_before
        || vault_after.amount != outcome.represented_custody_after
    {
        return Err(BoundaryError::InvalidTokenDelta.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    overwrite_account(&mut position.try_borrow_mut_data()?, &position_state)?;
    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || decode_account::<PositionState>(&position.try_borrow_data()?)? != position_state
        || pool_state.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    emit_martingaler_history(&encode_martingaler_close_history_receipt(
        pool.key,
        position.key,
        &position_before,
        MARTINGALER_CLOSE_NONPOSITIVE,
        exit_price_e6,
        secondary_exit_price_e6,
        MartingalerCloseReceiptAmounts {
            principal_refund: outcome.principal_refund,
            gross_loss: outcome.gross_loss,
            residual_bad_debt_added: outcome.residual_bad_debt_added,
            child_claim_sequence: NO_CLAIM_SEQUENCE,
            ..MartingalerCloseReceiptAmounts::default()
        },
    )?);
    Ok(())
}

#[inline(never)]
fn process_close_profitable_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    settlement_shape: ProfitSettlementShape,
    controller_override: Option<&Pubkey>,
) -> ProgramResult {
    let settlement_account_count = match settlement_shape {
        ProfitSettlementShape::NoClaim => 0,
        ProfitSettlementShape::CurrentTail => 2,
        ProfitSettlementShape::Rollover => 3,
    };
    let one_feed_count = PROFITABLE_CLOSE_BASE_ACCOUNT_COUNT
        .checked_add(settlement_account_count)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != one_feed_count && accounts.len() != one_feed_count + 1 {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    let controller = &accounts[3];
    let position = &accounts[4];
    let oracle_policy = &accounts[5];
    let vault_authority = &accounts[6];
    let vault = &accounts[7];
    let destination = &accounts[8];
    let quote_mint = &accounts[9];
    let token_program = &accounts[10];
    let primary_oracle = &accounts[11];
    let controller_key = controller_override.unwrap_or(authority.key);

    validate_payer_authority(authority)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_for_close(program_id, pool)?;
    if pool_state.quote_mint.0 != quote_mint.key.to_bytes()
        || pool_state.vault.0 != vault.key.to_bytes()
    {
        return Err(BoundaryError::InvalidPool.into());
    }
    expect_writable(market)?;
    expect_nonsigner(market)?;
    expect_writable(position)?;
    expect_nonsigner(position)?;
    let mut position_state =
        load_canonical_position(program_id, pool.key, controller_key, position_id, position)?;
    let mut market_state =
        load_canonical_market(program_id, pool.key, position_state.market_id.get(), market)?;
    if !matches!(
        PoolStatus::try_from(market_state.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    ) {
        return Err(BoundaryError::InvalidMarket.into());
    }
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, controller_key, controller)?;
    expect_readonly(oracle_policy)?;
    let policy_state =
        load_canonical_oracle_policy(program_id, pool.key, market.key, oracle_policy)?;
    let oracle_account_count = usize::from(policy_state.oracle_count);
    let expected_account_count = one_feed_count
        .checked_add(oracle_account_count - 1)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != expected_account_count {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    if (policy_state.oracle_count == 2) != (position_state.secondary_entry_price_e6.get() != 0) {
        return Err(BoundaryError::InvalidOraclePolicy.into());
    }

    expect_readonly(vault_authority)?;
    if vault_authority.executable {
        return Err(BoundaryError::InvalidVault.into());
    }
    let (expected_vault_authority, _) = derive_vault_authority(program_id, pool.key);
    if vault_authority.key != &expected_vault_authority {
        return Err(BoundaryError::InvalidVault.into());
    }
    expect_writable(vault)?;
    expect_nonsigner(vault)?;
    expect_writable(destination)?;
    expect_nonsigner(destination)?;
    expect_readonly(quote_mint)?;
    validate_quote_mint(quote_mint)?;
    validate_external_program(token_program, &spl_token::ID)?;
    let expected_vault = derive_canonical_vault(&expected_vault_authority, quote_mint.key);
    if vault.key != &expected_vault {
        return Err(BoundaryError::InvalidVault.into());
    }
    let vault_before =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_before =
        validate_destination_token_account(destination, quote_mint.key, controller_key)?;
    let mint_supply_before = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;

    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (exit_price_e6, secondary_exit_price_e6) = read_policy_exit_prices(
        program_id,
        &policy_state,
        &market_state,
        primary_oracle,
        (policy_state.oracle_count == 2).then(|| &accounts[12]),
        now_unix_ts,
    )?;

    let mut claim_bump = None;
    let mut current_tail_state: Option<Box<QueuePageState>> = None;
    let mut rollover_tail_state: Option<Box<QueuePageState>> = None;
    let settlement_start = PROFITABLE_CLOSE_BASE_ACCOUNT_COUNT
        .checked_add(oracle_account_count - 1)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    let claim_key = if settlement_shape == ProfitSettlementShape::NoClaim {
        None
    } else {
        let claim = &accounts[settlement_start];
        expect_writable(claim)?;
        expect_nonsigner(claim)?;
        let (expected_claim, bump) = derive_claim(program_id, position.key);
        if claim.key != &expected_claim {
            return Err(BoundaryError::InvalidClaim.into());
        }
        validate_prepared_target(claim, program_id, account_len::<ClaimState>())?;
        claim_bump = Some(bump);

        let current_tail = &accounts[settlement_start + 1];
        expect_writable(current_tail)?;
        expect_nonsigner(current_tail)?;
        current_tail_state = Some(load_canonical_queue_page(
            program_id,
            pool.key,
            pool_state.tail_page_index.get(),
            current_tail,
        )?);

        if settlement_shape == ProfitSettlementShape::Rollover {
            let rollover_tail = &accounts[settlement_start + 2];
            expect_writable(rollover_tail)?;
            expect_nonsigner(rollover_tail)?;
            let page_index = pool_state.next_page_index.get();
            let (expected_page, _) = derive_queue_page(program_id, pool.key, page_index);
            if rollover_tail.key != &expected_page {
                return Err(BoundaryError::InvalidQueuePage.into());
            }
            validate_prepared_target(rollover_tail, program_id, account_len::<QueuePageState>())?;
            rollover_tail_state = Some(QueuePageState::new_boxed(
                Key32::new(pool.key.to_bytes()),
                page_index,
                pool_state.tail_page_index.get(),
                pool_state.next_claim_sequence.get(),
            )?);
        }
        Some(Key32::new(claim.key.to_bytes()))
    };

    let position_before = position_state;
    let outcome = close_profitable_cash_position(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(controller_key.to_bytes()),
        claim_key,
        &mut pool_state,
        &mut market_state,
        &mut controller_state,
        &mut position_state,
        current_tail_state.as_deref_mut(),
        rollover_tail_state.as_deref_mut(),
        vault_before.amount,
        exit_price_e6,
        secondary_exit_price_e6,
    )?;
    let actual_shape = match (outcome.claim.is_some(), rollover_tail_state.is_some()) {
        (false, false) => ProfitSettlementShape::NoClaim,
        (true, false) => ProfitSettlementShape::CurrentTail,
        (true, true) => ProfitSettlementShape::Rollover,
        (false, true) => return Err(BoundaryError::SettlementShapeMismatch.into()),
    };
    if actual_shape != settlement_shape {
        return Err(BoundaryError::SettlementShapeMismatch.into());
    }

    let vault_after =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_after =
        validate_destination_token_account(destination, quote_mint.key, controller_key)?;
    let mint_supply_after = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    if vault_after.amount != vault_before.amount
        || destination_after.amount != destination_before.amount
        || mint_supply_after != mint_supply_before
        || vault_after.amount != outcome.represented_custody_after
    {
        return Err(BoundaryError::InvalidTokenDelta.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    overwrite_account(&mut position.try_borrow_mut_data()?, &position_state)?;
    if let Some(claim_state) = outcome.claim {
        let claim = &accounts[settlement_start];
        initialize_account(
            &mut claim.try_borrow_mut_data()?,
            claim_bump.ok_or(BoundaryError::InvalidClaim)?,
            &claim_state,
        )?;
        let current_tail = &accounts[settlement_start + 1];
        overwrite_account(
            &mut current_tail.try_borrow_mut_data()?,
            current_tail_state
                .as_deref()
                .ok_or(BoundaryError::InvalidQueuePage)?,
        )?;
        if let Some(rollover_state) = rollover_tail_state.as_deref() {
            let rollover_tail = &accounts[settlement_start + 2];
            let (_, bump) =
                derive_queue_page(program_id, pool.key, rollover_state.page_index.get());
            initialize_account(
                &mut rollover_tail.try_borrow_mut_data()?,
                bump,
                rollover_state,
            )?;
        }
    }

    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || decode_account::<PositionState>(&position.try_borrow_data()?)? != position_state
        || pool_state.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    if let Some(expected_claim) = outcome.claim {
        if decode_account::<ClaimState>(&accounts[settlement_start].try_borrow_data()?)?
            != expected_claim
            || !account_body_matches(
                &accounts[settlement_start + 1].try_borrow_data()?,
                current_tail_state
                    .as_deref()
                    .ok_or(BoundaryError::InvalidQueuePage)?,
            )?
        {
            return Err(BoundaryError::PostconditionFailed.into());
        }
        if let Some(expected_rollover) = rollover_tail_state.as_deref() {
            if !account_body_matches(
                &accounts[settlement_start + 2].try_borrow_data()?,
                expected_rollover,
            )? {
                return Err(BoundaryError::PostconditionFailed.into());
            }
        }
    }
    emit_martingaler_history(&encode_martingaler_close_history_receipt(
        pool.key,
        position.key,
        &position_before,
        MARTINGALER_CLOSE_PROFITABLE,
        exit_price_e6,
        secondary_exit_price_e6,
        MartingalerCloseReceiptAmounts {
            principal_refund: outcome.principal_refund,
            haircut_withheld: outcome.adjustment.haircut_withheld,
            win_fee: outcome.adjustment.win_fee,
            profit_cap_withheld: outcome.adjustment.profit_cap_withheld,
            immediate_profit_paid: outcome.immediate_profit_paid,
            queued_profit: outcome.queued_profit,
            child_claim_sequence: outcome
                .claim
                .map(|claim| claim.sequence.get())
                .unwrap_or(NO_CLAIM_SEQUENCE),
            ..MartingalerCloseReceiptAmounts::default()
        },
    )?);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaimVoluntaryCloseKind {
    NonPositive,
    Profitable,
}

#[inline(never)]
fn process_close_claim_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    settlement_shape: ProfitSettlementShape,
    close_kind: ClaimVoluntaryCloseKind,
    controller_override: Option<&Pubkey>,
) -> ProgramResult {
    let settlement_account_count = match settlement_shape {
        ProfitSettlementShape::NoClaim => 0,
        ProfitSettlementShape::CurrentTail => 2,
        ProfitSettlementShape::Rollover => 3,
    };
    let one_feed_count = CLAIM_CLOSE_BASE_ACCOUNT_COUNT
        .checked_add(settlement_account_count)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != one_feed_count && accounts.len() != one_feed_count + 1 {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    let controller = &accounts[3];
    let position = &accounts[4];
    let source_claim = &accounts[5];
    let oracle_policy = &accounts[6];
    let vault_authority = &accounts[7];
    let vault = &accounts[8];
    let destination = &accounts[9];
    let quote_mint = &accounts[10];
    let token_program = &accounts[11];
    let primary_oracle = &accounts[12];
    let controller_key = controller_override.unwrap_or(authority.key);

    validate_payer_authority(authority)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_for_close(program_id, pool)?;
    if pool_state.quote_mint.0 != quote_mint.key.to_bytes()
        || pool_state.vault.0 != vault.key.to_bytes()
    {
        return Err(BoundaryError::InvalidPool.into());
    }
    expect_writable(position)?;
    expect_nonsigner(position)?;
    let mut position_state =
        load_canonical_position(program_id, pool.key, controller_key, position_id, position)?;
    expect_writable(market)?;
    expect_nonsigner(market)?;
    let mut market_state =
        load_canonical_market(program_id, pool.key, position_state.market_id.get(), market)?;
    if !matches!(
        PoolStatus::try_from(market_state.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    ) {
        return Err(BoundaryError::InvalidMarket.into());
    }
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, controller_key, controller)?;
    expect_writable(source_claim)?;
    expect_nonsigner(source_claim)?;
    let mut source_claim_state = load_canonical_claim(program_id, pool.key, source_claim)?;
    if source_claim_state.sequence != position_state.source_claim_sequence {
        return Err(BoundaryError::InvalidClaim.into());
    }
    expect_readonly(oracle_policy)?;
    let policy_state =
        load_canonical_oracle_policy(program_id, pool.key, market.key, oracle_policy)?;
    let oracle_account_count = usize::from(policy_state.oracle_count);
    let expected_account_count = one_feed_count
        .checked_add(oracle_account_count - 1)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != expected_account_count {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    if (policy_state.oracle_count == 2) != (position_state.secondary_entry_price_e6.get() != 0) {
        return Err(BoundaryError::InvalidOraclePolicy.into());
    }

    expect_readonly(vault_authority)?;
    if vault_authority.executable {
        return Err(BoundaryError::InvalidVault.into());
    }
    let (expected_vault_authority, _) = derive_vault_authority(program_id, pool.key);
    if vault_authority.key != &expected_vault_authority {
        return Err(BoundaryError::InvalidVault.into());
    }
    expect_writable(vault)?;
    expect_nonsigner(vault)?;
    expect_writable(destination)?;
    expect_nonsigner(destination)?;
    expect_readonly(quote_mint)?;
    validate_quote_mint(quote_mint)?;
    validate_external_program(token_program, &spl_token::ID)?;
    let expected_vault = derive_canonical_vault(&expected_vault_authority, quote_mint.key);
    if vault.key != &expected_vault {
        return Err(BoundaryError::InvalidVault.into());
    }
    let vault_before =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_before =
        validate_destination_token_account(destination, quote_mint.key, controller_key)?;
    let mint_supply_before = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;

    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (exit_price_e6, secondary_exit_price_e6) = read_policy_exit_prices(
        program_id,
        &policy_state,
        &market_state,
        primary_oracle,
        (policy_state.oracle_count == 2).then(|| &accounts[13]),
        now_unix_ts,
    )?;

    let mut child_claim_bump = None;
    let mut current_tail_state: Option<Box<QueuePageState>> = None;
    let mut rollover_tail_state: Option<Box<QueuePageState>> = None;
    let settlement_start = CLAIM_CLOSE_BASE_ACCOUNT_COUNT
        .checked_add(oracle_account_count - 1)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    let child_claim_key = if settlement_shape == ProfitSettlementShape::NoClaim {
        None
    } else {
        let child_claim = &accounts[settlement_start];
        expect_writable(child_claim)?;
        expect_nonsigner(child_claim)?;
        let (expected_child_claim, bump) = derive_claim(program_id, position.key);
        if child_claim.key != &expected_child_claim {
            return Err(BoundaryError::InvalidClaim.into());
        }
        validate_prepared_target(child_claim, program_id, account_len::<ClaimState>())?;
        child_claim_bump = Some(bump);

        let current_tail = &accounts[settlement_start + 1];
        expect_writable(current_tail)?;
        expect_nonsigner(current_tail)?;
        current_tail_state = Some(load_canonical_queue_page(
            program_id,
            pool.key,
            pool_state.tail_page_index.get(),
            current_tail,
        )?);

        if settlement_shape == ProfitSettlementShape::Rollover {
            let rollover_tail = &accounts[settlement_start + 2];
            expect_writable(rollover_tail)?;
            expect_nonsigner(rollover_tail)?;
            let page_index = pool_state.next_page_index.get();
            let (expected_page, _) = derive_queue_page(program_id, pool.key, page_index);
            if rollover_tail.key != &expected_page {
                return Err(BoundaryError::InvalidQueuePage.into());
            }
            validate_prepared_target(rollover_tail, program_id, account_len::<QueuePageState>())?;
            rollover_tail_state = Some(QueuePageState::new_boxed(
                Key32::new(pool.key.to_bytes()),
                page_index,
                pool_state.tail_page_index.get(),
                pool_state.next_claim_sequence.get(),
            )?);
        }
        Some(Key32::new(child_claim.key.to_bytes()))
    };

    let position_before = position_state;
    let outcome = match close_kind {
        ClaimVoluntaryCloseKind::NonPositive => close_nonpositive_claim_position(
            Key32::new(pool.key.to_bytes()),
            Key32::new(market.key.to_bytes()),
            Key32::new(controller_key.to_bytes()),
            child_claim_key,
            &mut pool_state,
            &mut market_state,
            &mut controller_state,
            &mut source_claim_state,
            &mut position_state,
            current_tail_state.as_deref_mut(),
            rollover_tail_state.as_deref_mut(),
            vault_before.amount,
            exit_price_e6,
            secondary_exit_price_e6,
        )?,
        ClaimVoluntaryCloseKind::Profitable => close_profitable_claim_position(
            Key32::new(pool.key.to_bytes()),
            Key32::new(market.key.to_bytes()),
            Key32::new(controller_key.to_bytes()),
            child_claim_key,
            &mut pool_state,
            &mut market_state,
            &mut controller_state,
            &mut source_claim_state,
            &mut position_state,
            current_tail_state.as_deref_mut(),
            rollover_tail_state.as_deref_mut(),
            vault_before.amount,
            exit_price_e6,
            secondary_exit_price_e6,
        )?,
    };
    let actual_shape = match (outcome.child_claim.is_some(), rollover_tail_state.is_some()) {
        (false, false) => ProfitSettlementShape::NoClaim,
        (true, false) => ProfitSettlementShape::CurrentTail,
        (true, true) => ProfitSettlementShape::Rollover,
        (false, true) => return Err(BoundaryError::SettlementShapeMismatch.into()),
    };
    if actual_shape != settlement_shape {
        return Err(BoundaryError::SettlementShapeMismatch.into());
    }

    let vault_after =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_after =
        validate_destination_token_account(destination, quote_mint.key, controller_key)?;
    let mint_supply_after = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    if vault_after.amount != vault_before.amount
        || destination_after.amount != destination_before.amount
        || mint_supply_after != mint_supply_before
        || vault_after.amount != outcome.represented_custody_after
    {
        return Err(BoundaryError::InvalidTokenDelta.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    overwrite_account(&mut position.try_borrow_mut_data()?, &position_state)?;
    overwrite_account(
        &mut source_claim.try_borrow_mut_data()?,
        &source_claim_state,
    )?;
    if let Some(child_claim_state) = outcome.child_claim {
        let child_claim = &accounts[settlement_start];
        initialize_account(
            &mut child_claim.try_borrow_mut_data()?,
            child_claim_bump.ok_or(BoundaryError::InvalidClaim)?,
            &child_claim_state,
        )?;
        overwrite_account(
            &mut accounts[settlement_start + 1].try_borrow_mut_data()?,
            current_tail_state
                .as_deref()
                .ok_or(BoundaryError::InvalidQueuePage)?,
        )?;
        if let Some(rollover_state) = rollover_tail_state.as_deref() {
            let (_, bump) =
                derive_queue_page(program_id, pool.key, rollover_state.page_index.get());
            initialize_account(
                &mut accounts[settlement_start + 2].try_borrow_mut_data()?,
                bump,
                rollover_state,
            )?;
        }
    }

    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || decode_account::<PositionState>(&position.try_borrow_data()?)? != position_state
        || decode_account::<ClaimState>(&source_claim.try_borrow_data()?)? != source_claim_state
        || pool_state.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    if let Some(expected_child_claim) = outcome.child_claim {
        if decode_account::<ClaimState>(&accounts[settlement_start].try_borrow_data()?)?
            != expected_child_claim
            || !account_body_matches(
                &accounts[settlement_start + 1].try_borrow_data()?,
                current_tail_state
                    .as_deref()
                    .ok_or(BoundaryError::InvalidQueuePage)?,
            )?
        {
            return Err(BoundaryError::PostconditionFailed.into());
        }
        if let Some(expected_rollover) = rollover_tail_state.as_deref() {
            if !account_body_matches(
                &accounts[settlement_start + 2].try_borrow_data()?,
                expected_rollover,
            )? {
                return Err(BoundaryError::PostconditionFailed.into());
            }
        }
    }
    let adjustment = outcome.adjustment.unwrap_or_default();
    emit_martingaler_history(&encode_martingaler_close_history_receipt(
        pool.key,
        position.key,
        &position_before,
        match close_kind {
            ClaimVoluntaryCloseKind::NonPositive => MARTINGALER_CLOSE_NONPOSITIVE,
            ClaimVoluntaryCloseKind::Profitable => MARTINGALER_CLOSE_PROFITABLE,
        },
        exit_price_e6,
        secondary_exit_price_e6,
        MartingalerCloseReceiptAmounts {
            gross_loss: outcome.gross_loss,
            haircut_withheld: adjustment.haircut_withheld,
            win_fee: adjustment.win_fee,
            profit_cap_withheld: adjustment.profit_cap_withheld,
            immediate_profit_paid: outcome.immediate_payout,
            queued_profit: outcome.queued_payout,
            residual_bad_debt_added: outcome.residual_bad_debt_added,
            child_claim_sequence: outcome
                .child_claim
                .map(|claim| claim.sequence.get())
                .unwrap_or(NO_CLAIM_SEQUENCE),
            ..MartingalerCloseReceiptAmounts::default()
        },
    )?);
    Ok(())
}

#[inline(never)]
fn process_service_fifo_head<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> ProgramResult {
    if accounts.len() != SERVICE_FIFO_HEAD_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let pool = &accounts[0];
    let controller = &accounts[1];
    let claim = &accounts[2];
    let head_page = &accounts[3];
    let vault_authority = &accounts[4];
    let vault = &accounts[5];
    let destination = &accounts[6];
    let quote_mint = &accounts[7];
    let token_program = &accounts[8];

    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_for_claim_service(program_id, pool)?;
    expect_writable(claim)?;
    expect_nonsigner(claim)?;
    let mut claim_state = load_canonical_claim(program_id, pool.key, claim)?;
    let controller_key = Pubkey::new_from_array(claim_state.controller.0);
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, &controller_key, controller)?;
    expect_writable(head_page)?;
    expect_nonsigner(head_page)?;
    let mut head_page_state = load_canonical_queue_page(
        program_id,
        pool.key,
        pool_state.head_page_index.get(),
        head_page,
    )?;

    expect_readonly(vault_authority)?;
    if vault_authority.executable {
        return Err(BoundaryError::InvalidVault.into());
    }
    let (expected_vault_authority, _) = derive_vault_authority(program_id, pool.key);
    if vault_authority.key != &expected_vault_authority {
        return Err(BoundaryError::InvalidVault.into());
    }
    expect_writable(vault)?;
    expect_nonsigner(vault)?;
    expect_writable(destination)?;
    expect_nonsigner(destination)?;
    expect_readonly(quote_mint)?;
    validate_quote_mint(quote_mint)?;
    validate_external_program(token_program, &spl_token::ID)?;
    if pool_state.quote_mint.0 != quote_mint.key.to_bytes()
        || pool_state.vault.0 != vault.key.to_bytes()
        || vault.key != &derive_canonical_vault(&expected_vault_authority, quote_mint.key)
        || destination.key != &derive_canonical_vault(&controller_key, quote_mint.key)
    {
        return Err(BoundaryError::InvalidTokenAccount.into());
    }
    let vault_before =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_before =
        validate_destination_token_account(destination, quote_mint.key, &controller_key)?;
    let mint_supply_before = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    if pool_state.represented_quote_custody()? != u128::from(vault_before.amount) {
        return Err(BoundaryError::CustodyMismatch.into());
    }

    let _outcome = service_fifo_claim(
        Key32::new(pool.key.to_bytes()),
        &mut pool_state,
        &mut controller_state,
        head_page_state.as_mut(),
        Key32::new(claim.key.to_bytes()),
        &mut claim_state,
    )?;
    let vault_after =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_after =
        validate_destination_token_account(destination, quote_mint.key, &controller_key)?;
    let mint_supply_after = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    if vault_after.amount != vault_before.amount
        || destination_after.amount != destination_before.amount
        || mint_supply_after != mint_supply_before
        || pool_state.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::InvalidTokenDelta.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    overwrite_account(&mut claim.try_borrow_mut_data()?, &claim_state)?;
    overwrite_account(
        &mut head_page.try_borrow_mut_data()?,
        head_page_state.as_ref(),
    )?;
    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || decode_account::<ClaimState>(&claim.try_borrow_data()?)? != claim_state
        || !account_body_matches(&head_page.try_borrow_data()?, head_page_state.as_ref())?
        || pool_state.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

/// Reclassifies serviced FIFO value into the claim owner's internal Account
/// USDC balance. No quote token leaves custody; explicit withdrawal remains the
/// only path that can move value from the canonical vault to a wallet ATA.
#[inline(never)]
fn process_service_fifo_head_internal<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
) -> ProgramResult {
    if accounts.len() != SERVICE_FIFO_HEAD_INTERNAL_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let pool = &accounts[0];
    let controller = &accounts[1];
    let claim = &accounts[2];
    let head_page = &accounts[3];

    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_for_claim_service(program_id, pool)?;
    let represented_before = pool_state.represented_quote_custody()?;
    expect_writable(claim)?;
    expect_nonsigner(claim)?;
    let mut claim_state = load_canonical_claim(program_id, pool.key, claim)?;
    let controller_key = Pubkey::new_from_array(claim_state.controller.0);
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, &controller_key, controller)?;
    expect_writable(head_page)?;
    expect_nonsigner(head_page)?;
    let mut head_page_state = load_canonical_queue_page(
        program_id,
        pool.key,
        pool_state.head_page_index.get(),
        head_page,
    )?;

    service_fifo_claim(
        Key32::new(pool.key.to_bytes()),
        &mut pool_state,
        &mut controller_state,
        head_page_state.as_mut(),
        Key32::new(claim.key.to_bytes()),
        &mut claim_state,
    )?;
    if pool_state.represented_quote_custody()? != represented_before {
        return Err(BoundaryError::InvalidTokenDelta.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    overwrite_account(&mut claim.try_borrow_mut_data()?, &claim_state)?;
    overwrite_account(
        &mut head_page.try_borrow_mut_data()?,
        head_page_state.as_ref(),
    )?;
    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || decode_account::<ClaimState>(&claim.try_borrow_data()?)? != claim_state
        || !account_body_matches(&head_page.try_borrow_data()?, head_page_state.as_ref())?
        || pool_state.represented_quote_custody()? != represented_before
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_pop_fifo_head<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    shape: PopFifoShape,
) -> ProgramResult {
    let expected_count = match shape {
        PopFifoShape::CurrentPageOrEmpty => POP_FIFO_HEAD_BASE_ACCOUNT_COUNT,
        PopFifoShape::NextPage => POP_FIFO_HEAD_BASE_ACCOUNT_COUNT + 1,
    };
    if accounts.len() != expected_count {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let pool = &accounts[0];
    let claim = &accounts[1];
    let head_page = &accounts[2];
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_for_claim_service(program_id, pool)?;
    expect_writable(claim)?;
    expect_nonsigner(claim)?;
    let claim_state = load_canonical_claim(program_id, pool.key, claim)?;
    expect_writable(head_page)?;
    expect_nonsigner(head_page)?;
    let mut head_page_state = load_canonical_queue_page(
        program_id,
        pool.key,
        pool_state.head_page_index.get(),
        head_page,
    )?;

    let next_offset = head_page_state
        .head_offset
        .get()
        .checked_add(1)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    let crosses_page = next_offset >= head_page_state.len.get()
        && head_page_state.next_page_index.get() != crate::martingaler_state::NO_PAGE_INDEX;
    let actual_shape = if crosses_page {
        PopFifoShape::NextPage
    } else {
        PopFifoShape::CurrentPageOrEmpty
    };
    if actual_shape != shape {
        return Err(BoundaryError::SettlementShapeMismatch.into());
    }

    let next_page_state = if shape == PopFifoShape::NextPage {
        let next_page = &accounts[3];
        expect_readonly(next_page)?;
        Some(load_canonical_queue_page(
            program_id,
            pool.key,
            head_page_state.next_page_index.get(),
            next_page,
        )?)
    } else {
        None
    };
    let claim_before = claim_state;
    pop_fifo_head_with_claim(
        Key32::new(pool.key.to_bytes()),
        &mut pool_state,
        head_page_state.as_mut(),
        Key32::new(claim.key.to_bytes()),
        &claim_state,
        next_page_state.as_deref(),
    )?;

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(
        &mut head_page.try_borrow_mut_data()?,
        head_page_state.as_ref(),
    )?;
    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<ClaimState>(&claim.try_borrow_data()?)? != claim_before
        || !account_body_matches(&head_page.try_borrow_data()?, head_page_state.as_ref())?
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    if let Some(expected_next) = next_page_state.as_deref() {
        if !account_body_matches(&accounts[3].try_borrow_data()?, expected_next)? {
            return Err(BoundaryError::PostconditionFailed.into());
        }
    }
    Ok(())
}

#[inline(never)]
fn process_resolve_bankrupt_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
) -> ProgramResult {
    if accounts.len() != BANKRUPTCY_BASE_ACCOUNT_COUNT
        && accounts.len() != BANKRUPTCY_BASE_ACCOUNT_COUNT + 1
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let pool = &accounts[0];
    let market = &accounts[1];
    let controller = &accounts[2];
    let position = &accounts[3];
    let oracle_policy = &accounts[4];
    let vault = &accounts[5];
    let primary_oracle = &accounts[6];

    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_for_close(program_id, pool)?;
    expect_writable(position)?;
    expect_nonsigner(position)?;
    if position.owner != program_id || position.data_len() != account_len::<PositionState>() {
        return Err(BoundaryError::InvalidPosition.into());
    }
    let candidate_position = decode_account::<PositionState>(&position.try_borrow_data()?)?;
    let controller_key = Pubkey::new_from_array(candidate_position.controller.0);
    let mut position_state =
        load_canonical_position(program_id, pool.key, &controller_key, position_id, position)?;
    expect_writable(market)?;
    expect_nonsigner(market)?;
    let mut market_state =
        load_canonical_market(program_id, pool.key, position_state.market_id.get(), market)?;
    if !matches!(
        PoolStatus::try_from(market_state.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    ) {
        return Err(BoundaryError::InvalidMarket.into());
    }
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, &controller_key, controller)?;
    expect_readonly(oracle_policy)?;
    let policy_state =
        load_canonical_oracle_policy(program_id, pool.key, market.key, oracle_policy)?;
    let expected_account_count = BANKRUPTCY_BASE_ACCOUNT_COUNT
        .checked_add(usize::from(policy_state.oracle_count - 1))
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != expected_account_count
        || (policy_state.oracle_count == 2) != (position_state.secondary_entry_price_e6.get() != 0)
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }

    expect_readonly(vault)?;
    expect_nonsigner(vault)?;
    let quote_mint = Pubkey::new_from_array(pool_state.quote_mint.0);
    let (vault_authority, _) = derive_vault_authority(program_id, pool.key);
    let expected_vault = derive_canonical_vault(&vault_authority, &quote_mint);
    if vault.key != &expected_vault || pool_state.vault.0 != expected_vault.to_bytes() {
        return Err(BoundaryError::InvalidVault.into());
    }
    let vault_before = validate_vault_token_account(vault, &quote_mint, &vault_authority)?;
    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (primary_exit_price_e6, secondary_exit_price_e6) = read_policy_exit_prices(
        program_id,
        &policy_state,
        &market_state,
        primary_oracle,
        (policy_state.oracle_count == 2).then(|| &accounts[7]),
        now_unix_ts,
    )?;

    let position_before = position_state;
    let outcome = close_cash_position(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(controller_key.to_bytes()),
        &mut pool_state,
        &mut market_state,
        &mut controller_state,
        &mut position_state,
        vault_before.amount,
        primary_exit_price_e6,
        secondary_exit_price_e6,
        true,
    )?;
    if outcome.principal_refund != 0 || outcome.represented_custody_after != vault_before.amount {
        return Err(BoundaryError::PostconditionFailed.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    overwrite_account(&mut position.try_borrow_mut_data()?, &position_state)?;
    let vault_after = validate_vault_token_account(vault, &quote_mint, &vault_authority)?;
    if vault_after != vault_before
        || decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || decode_account::<PositionState>(&position.try_borrow_data()?)? != position_state
        || pool_state.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    emit_martingaler_history(&encode_martingaler_close_history_receipt(
        pool.key,
        position.key,
        &position_before,
        MARTINGALER_CLOSE_BANKRUPT,
        primary_exit_price_e6,
        secondary_exit_price_e6,
        MartingalerCloseReceiptAmounts {
            gross_loss: outcome.gross_loss,
            residual_bad_debt_added: outcome.residual_bad_debt_added,
            child_claim_sequence: NO_CLAIM_SEQUENCE,
            ..MartingalerCloseReceiptAmounts::default()
        },
    )?);
    Ok(())
}

#[inline(never)]
fn process_resolve_bankrupt_claim_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
) -> ProgramResult {
    if accounts.len() != CLAIM_BANKRUPTCY_BASE_ACCOUNT_COUNT
        && accounts.len() != CLAIM_BANKRUPTCY_BASE_ACCOUNT_COUNT + 1
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let pool = &accounts[0];
    let market = &accounts[1];
    let controller = &accounts[2];
    let position = &accounts[3];
    let source_claim = &accounts[4];
    let oracle_policy = &accounts[5];
    let vault = &accounts[6];
    let primary_oracle = &accounts[7];

    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_for_close(program_id, pool)?;
    expect_writable(position)?;
    expect_nonsigner(position)?;
    if position.owner != program_id || position.data_len() != account_len::<PositionState>() {
        return Err(BoundaryError::InvalidPosition.into());
    }
    let candidate_position = decode_account::<PositionState>(&position.try_borrow_data()?)?;
    let controller_key = Pubkey::new_from_array(candidate_position.controller.0);
    let mut position_state =
        load_canonical_position(program_id, pool.key, &controller_key, position_id, position)?;
    expect_writable(market)?;
    expect_nonsigner(market)?;
    let mut market_state =
        load_canonical_market(program_id, pool.key, position_state.market_id.get(), market)?;
    if !matches!(
        PoolStatus::try_from(market_state.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    ) {
        return Err(BoundaryError::InvalidMarket.into());
    }
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, &controller_key, controller)?;
    expect_writable(source_claim)?;
    expect_nonsigner(source_claim)?;
    let mut source_claim_state = load_canonical_claim(program_id, pool.key, source_claim)?;
    if source_claim_state.sequence != position_state.source_claim_sequence {
        return Err(BoundaryError::InvalidClaim.into());
    }
    expect_readonly(oracle_policy)?;
    let policy_state =
        load_canonical_oracle_policy(program_id, pool.key, market.key, oracle_policy)?;
    let expected_account_count = CLAIM_BANKRUPTCY_BASE_ACCOUNT_COUNT
        .checked_add(usize::from(policy_state.oracle_count - 1))
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if accounts.len() != expected_account_count
        || (policy_state.oracle_count == 2) != (position_state.secondary_entry_price_e6.get() != 0)
    {
        return Err(BoundaryError::InvalidAccountCount.into());
    }

    expect_readonly(vault)?;
    expect_nonsigner(vault)?;
    let quote_mint = Pubkey::new_from_array(pool_state.quote_mint.0);
    let (vault_authority, _) = derive_vault_authority(program_id, pool.key);
    let expected_vault = derive_canonical_vault(&vault_authority, &quote_mint);
    if vault.key != &expected_vault || pool_state.vault.0 != expected_vault.to_bytes() {
        return Err(BoundaryError::InvalidVault.into());
    }
    let vault_before = validate_vault_token_account(vault, &quote_mint, &vault_authority)?;
    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (primary_exit_price_e6, secondary_exit_price_e6) = read_policy_exit_prices(
        program_id,
        &policy_state,
        &market_state,
        primary_oracle,
        (policy_state.oracle_count == 2).then(|| &accounts[8]),
        now_unix_ts,
    )?;

    let position_before = position_state;
    let outcome = resolve_bankrupt_claim_position(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(controller_key.to_bytes()),
        &mut pool_state,
        &mut market_state,
        &mut controller_state,
        &mut source_claim_state,
        &mut position_state,
        vault_before.amount,
        primary_exit_price_e6,
        secondary_exit_price_e6,
    )?;
    if outcome.immediate_payout != 0
        || outcome.queued_payout != 0
        || outcome.child_claim.is_some()
        || outcome.represented_custody_after != vault_before.amount
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    overwrite_account(&mut position.try_borrow_mut_data()?, &position_state)?;
    overwrite_account(
        &mut source_claim.try_borrow_mut_data()?,
        &source_claim_state,
    )?;
    let vault_after = validate_vault_token_account(vault, &quote_mint, &vault_authority)?;
    if vault_after != vault_before
        || decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || decode_account::<PositionState>(&position.try_borrow_data()?)? != position_state
        || decode_account::<ClaimState>(&source_claim.try_borrow_data()?)? != source_claim_state
        || pool_state.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    emit_martingaler_history(&encode_martingaler_close_history_receipt(
        pool.key,
        position.key,
        &position_before,
        MARTINGALER_CLOSE_BANKRUPT,
        primary_exit_price_e6,
        secondary_exit_price_e6,
        MartingalerCloseReceiptAmounts {
            gross_loss: outcome.gross_loss,
            residual_bad_debt_added: outcome.residual_bad_debt_added,
            child_claim_sequence: NO_CLAIM_SEQUENCE,
            ..MartingalerCloseReceiptAmounts::default()
        },
    )?);
    Ok(())
}

#[inline(never)]
fn process_set_pool_status<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    target: PoolStatus,
) -> ProgramResult {
    if accounts.len() != SET_POOL_STATUS_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let admin = &accounts[0];
    let pool = &accounts[1];
    validate_controller_authority(admin)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_any_status(program_id, pool)?;
    if pool_state.admin.0 != admin.key.to_bytes() {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    transition_pool_status(&mut pool_state, target)?;
    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_set_market_status<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    target: PoolStatus,
) -> ProgramResult {
    if accounts.len() != SET_MARKET_STATUS_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let admin = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    validate_controller_authority(admin)?;
    expect_readonly(pool)?;
    let pool_state = load_canonical_pool_any_status(program_id, pool)?;
    if pool_state.admin.0 != admin.key.to_bytes() {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    expect_writable(market)?;
    expect_nonsigner(market)?;
    if market.owner != program_id || market.data_len() != account_len::<MarketRiskState>() {
        return Err(BoundaryError::InvalidMarket.into());
    }
    let candidate = decode_account::<MarketRiskState>(&market.try_borrow_data()?)?;
    let mut market_state =
        load_canonical_market(program_id, pool.key, candidate.market_id.get(), market)?;
    transition_market_status(Key32::new(pool.key.to_bytes()), &mut market_state, target)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    if decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_update_leverage_limit<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    market_id: u16,
    max_leverage_x: u16,
) -> ProgramResult {
    if accounts.len() != UPDATE_LEVERAGE_LIMIT_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let admin = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    validate_controller_authority(admin)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool(program_id, pool)?;
    if pool_state.admin.0 != admin.key.to_bytes()
        || pool_state.status != PoolStatus::Active as u8
        || pool_state.reserved_open_profit.get() != 0
        || max_leverage_x < pool_state.max_leverage_x.get()
    {
        return Err(BoundaryError::InvalidMarketLimits.into());
    }
    expect_writable(market)?;
    expect_nonsigner(market)?;
    let mut market_state = load_canonical_market(program_id, pool.key, market_id, market)?;
    if market_state.status != PoolStatus::Active as u8
        || market_state.long_open_interest.get() != 0
        || market_state.short_open_interest.get() != 0
        || market_state.reserved_profit.get() != 0
        || max_leverage_x < market_state.max_leverage_x.get()
        || max_leverage_x > V1_MAX_LEVERAGE_X
    {
        return Err(BoundaryError::InvalidMarketLimits.into());
    }
    pool_state.max_leverage_x = crate::martingaler_state::U16Le::new(max_leverage_x);
    market_state.max_leverage_x = crate::martingaler_state::U16Le::new(max_leverage_x);
    pool_state.validate()?;
    market_state.validate()?;
    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[inline(never)]
fn process_update_risk_limits<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    market_id: u16,
    max_leverage_x: u16,
    max_position_notional: u64,
    max_net_profit_per_position: u64,
    max_reserved_profit_per_controller: u64,
    total_liability_open_gate: u64,
    max_long_open_interest: u64,
    max_short_open_interest: u64,
) -> ProgramResult {
    if accounts.len() != UPDATE_RISK_LIMITS_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let admin = &accounts[0];
    let pool = &accounts[1];
    let market = &accounts[2];
    validate_controller_authority(admin)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool(program_id, pool)?;
    expect_writable(market)?;
    expect_nonsigner(market)?;
    let mut market_state = load_canonical_market(program_id, pool.key, market_id, market)?;

    let max_position_notional = u128::from(max_position_notional);
    let max_net_profit_per_position = u128::from(max_net_profit_per_position);
    let max_reserved_profit_per_controller = u128::from(max_reserved_profit_per_controller);
    let total_liability_open_gate = u128::from(total_liability_open_gate);
    let max_long_open_interest = u128::from(max_long_open_interest);
    let max_short_open_interest = u128::from(max_short_open_interest);
    let invalid_target = max_leverage_x == 0
        || max_leverage_x > V1_MAX_LEVERAGE_X
        || max_position_notional == 0
        || max_position_notional > V1_MAX_POSITION_NOTIONAL_QUOTE
        || max_net_profit_per_position == 0
        || max_net_profit_per_position > V1_MAX_NET_PROFIT_PER_POSITION_QUOTE
        || max_net_profit_per_position > max_position_notional
        || max_reserved_profit_per_controller == 0
        || max_reserved_profit_per_controller > V1_MAX_RESERVED_PROFIT_PER_CONTROLLER_QUOTE
        || total_liability_open_gate == 0
        || total_liability_open_gate > V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
        || max_long_open_interest == 0
        || max_long_open_interest > total_liability_open_gate
        || max_short_open_interest == 0
        || max_short_open_interest > total_liability_open_gate;
    let is_decrease = max_leverage_x < pool_state.max_leverage_x.get()
        || max_position_notional < pool_state.max_position_notional.get()
        || max_net_profit_per_position < pool_state.max_net_profit_per_position.get()
        || max_reserved_profit_per_controller < pool_state.max_reserved_profit_per_controller.get()
        || total_liability_open_gate < pool_state.total_liability_open_gate.get()
        || max_leverage_x < market_state.max_leverage_x.get()
        || max_position_notional < market_state.max_position_notional.get()
        || max_net_profit_per_position < market_state.max_net_profit_per_position.get()
        || max_long_open_interest < market_state.max_long_open_interest.get()
        || max_short_open_interest < market_state.max_short_open_interest.get();
    if pool_state.admin.0 != admin.key.to_bytes()
        || pool_state.status != PoolStatus::Active as u8
        || market_state.status != PoolStatus::Active as u8
        || pool_state.reserved_open_profit.get() != 0
        || market_state.long_open_interest.get() != 0
        || market_state.short_open_interest.get() != 0
        || market_state.reserved_profit.get() != 0
        || invalid_target
        || is_decrease
    {
        return Err(BoundaryError::InvalidMarketLimits.into());
    }

    pool_state.max_leverage_x = crate::martingaler_state::U16Le::new(max_leverage_x);
    pool_state.max_position_notional = crate::martingaler_state::U128Le::new(max_position_notional);
    pool_state.max_net_profit_per_position =
        crate::martingaler_state::U128Le::new(max_net_profit_per_position);
    pool_state.max_reserved_profit_per_controller =
        crate::martingaler_state::U128Le::new(max_reserved_profit_per_controller);
    pool_state.total_liability_open_gate =
        crate::martingaler_state::U128Le::new(total_liability_open_gate);
    market_state.max_leverage_x = crate::martingaler_state::U16Le::new(max_leverage_x);
    market_state.max_position_notional =
        crate::martingaler_state::U128Le::new(max_position_notional);
    market_state.max_net_profit_per_position =
        crate::martingaler_state::U128Le::new(max_net_profit_per_position);
    market_state.max_long_open_interest =
        crate::martingaler_state::U128Le::new(max_long_open_interest);
    market_state.max_short_open_interest =
        crate::martingaler_state::U128Le::new(max_short_open_interest);
    pool_state.validate()?;
    market_state.validate()?;
    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut market.try_borrow_mut_data()?, &market_state)?;
    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<MarketRiskState>(&market.try_borrow_data()?)? != market_state
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[inline(never)]
fn process_withdraw_idle_cash<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    amount: u64,
) -> ProgramResult {
    if accounts.len() != WITHDRAW_IDLE_CASH_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let controller = &accounts[2];
    let vault_authority = &accounts[3];
    let vault = &accounts[4];
    let destination = &accounts[5];
    let quote_mint = &accounts[6];
    let token_program = &accounts[7];

    validate_controller_authority(authority)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    let mut pool_state = load_canonical_pool_any_status(program_id, pool)?;
    if pool_state.quote_mint.0 != quote_mint.key.to_bytes()
        || pool_state.vault.0 != vault.key.to_bytes()
    {
        return Err(BoundaryError::InvalidPool.into());
    }
    expect_writable(controller)?;
    expect_nonsigner(controller)?;
    let mut controller_state =
        load_canonical_controller(program_id, pool.key, authority.key, controller)?;
    expect_readonly(vault_authority)?;
    if vault_authority.executable {
        return Err(BoundaryError::InvalidVault.into());
    }
    let (expected_vault_authority, vault_authority_bump) =
        derive_vault_authority(program_id, pool.key);
    if vault_authority.key != &expected_vault_authority {
        return Err(BoundaryError::InvalidVault.into());
    }
    expect_writable(vault)?;
    expect_nonsigner(vault)?;
    expect_writable(destination)?;
    expect_nonsigner(destination)?;
    expect_readonly(quote_mint)?;
    validate_quote_mint(quote_mint)?;
    validate_external_program(token_program, &spl_token::ID)?;
    let expected_vault = derive_canonical_vault(&expected_vault_authority, quote_mint.key);
    if vault.key != &expected_vault {
        return Err(BoundaryError::InvalidVault.into());
    }
    let vault_before =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_before =
        validate_destination_token_account(destination, quote_mint.key, authority.key)?;
    let mint_supply_before = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;

    let outcome = withdraw_idle_cash(
        Key32::new(pool.key.to_bytes()),
        Key32::new(authority.key.to_bytes()),
        &mut pool_state,
        &mut controller_state,
        vault_before.amount,
        amount,
    )?;
    let transfer = spl_token::instruction::transfer_checked(
        &spl_token::ID,
        vault.key,
        quote_mint.key,
        destination.key,
        vault_authority.key,
        &[],
        amount,
        V1_QUOTE_DECIMALS,
    )?;
    let bump = [vault_authority_bump];
    let signer_seeds: &[&[u8]] = &[VAULT_SEED, pool.key.as_ref(), &bump];
    invoke_signed(
        &transfer,
        &[
            vault.clone(),
            quote_mint.clone(),
            destination.clone(),
            vault_authority.clone(),
            token_program.clone(),
        ],
        &[signer_seeds],
    )?;

    let vault_after =
        validate_vault_token_account(vault, quote_mint.key, &expected_vault_authority)?;
    let destination_after =
        validate_destination_token_account(destination, quote_mint.key, authority.key)?;
    let mint_supply_after = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    if vault_after.amount
        != vault_before
            .amount
            .checked_sub(amount)
            .ok_or(BoundaryError::InvalidTokenDelta)?
        || destination_after.amount
            != destination_before
                .amount
                .checked_add(amount)
                .ok_or(BoundaryError::InvalidTokenDelta)?
        || mint_supply_after != mint_supply_before
        || vault_after.amount != outcome.represented_custody_after
    {
        return Err(BoundaryError::InvalidTokenDelta.into());
    }

    overwrite_account(&mut pool.try_borrow_mut_data()?, &pool_state)?;
    overwrite_account(&mut controller.try_borrow_mut_data()?, &controller_state)?;
    if decode_account::<PoolState>(&pool.try_borrow_data()?)? != pool_state
        || decode_account::<ControllerRiskState>(&controller.try_borrow_data()?)?
            != controller_state
        || pool_state.represented_quote_custody()? != u128::from(vault_after.amount)
    {
        return Err(BoundaryError::PostconditionFailed.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OracleProviderFamily {
    Pyth,
    Switchboard,
    Phoenix,
}

fn validate_direct_oracle_account(
    account: &AccountInfo<'_>,
) -> Result<OracleProviderFamily, ProgramError> {
    expect_readonly(account)?;
    expect_nonsigner(account)?;
    if account.executable || account.data_len() == 0 {
        return Err(BoundaryError::InvalidOracle.into());
    }
    if *account.owner == crate::oracle_v16::PYTH_RECEIVER_PROGRAM_ID {
        Ok(OracleProviderFamily::Pyth)
    } else if *account.owner == crate::oracle_v16::SWITCHBOARD_ON_DEMAND_MAINNET_PROGRAM_ID
        || *account.owner == crate::oracle_v16::SWITCHBOARD_ON_DEMAND_DEVNET_PROGRAM_ID
    {
        Ok(OracleProviderFamily::Switchboard)
    } else {
        Err(BoundaryError::InvalidOracle.into())
    }
}

fn read_policy_oracle_price(
    program_id: &Pubkey,
    policy: &OraclePolicyState,
    feed_id: &[u8; 32],
    account: &AccountInfo<'_>,
    now_unix_ts: i64,
) -> Result<(OracleProviderFamily, u64, i64), ProgramError> {
    if account.owner == program_id {
        expect_readonly(account)?;
        expect_nonsigner(account)?;
        let state = load_canonical_oracle_relay(program_id, account)?;
        let source = OracleRelaySource::try_from(state.source)
            .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
        if state.pool != policy.pool
            || state.market != policy.market
            || state.feed_id.0 != *feed_id
            || state.sequence.get() == 0
            || state.source_posted_slot.get() == 0
            || state.price_e6.get() == 0
            || state.publish_time.get() <= 0
            || state.publish_time.get()
                > now_unix_ts.saturating_add(ORACLE_RELAY_FUTURE_TOLERANCE_SECS)
            || now_unix_ts.saturating_sub(state.publish_time.get())
                > i64::try_from(policy.max_staleness_secs.get())
                    .map_err(|_| BoundaryError::InvalidOracle)?
            || u128::from(state.confidence_e6.get()) * 10_000
                > u128::from(state.price_e6.get()) * u128::from(policy.confidence_limit_bps.get())
        {
            return Err(BoundaryError::InvalidOracle.into());
        }
        // The pinned MagicBlock demo PriceUpdateV3 is Pyth-family transport,
        // while Phoenix Hawkeye is a distinct onchain market/mark family.
        // Keeping the immutable source tag in state prevents either relay
        // from silently satisfying a different independence requirement.
        let provider = match source {
            OracleRelaySource::MagicBlockDemo => OracleProviderFamily::Pyth,
            OracleRelaySource::PhoenixHawkeye | OracleRelaySource::PhoenixMainnetRelay => {
                OracleProviderFamily::Phoenix
            }
        };
        return Ok((provider, state.price_e6.get(), state.publish_time.get()));
    }

    let provider = validate_direct_oracle_account(account)?;
    let (price, publish_time) = crate::oracle_v16::read_oracle_price_e6(
        account,
        feed_id,
        now_unix_ts,
        policy.max_staleness_secs.get(),
        policy.confidence_limit_bps.get(),
    )
    .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    Ok((provider, price, publish_time))
}

fn read_policy_prices(
    program_id: &Pubkey,
    policy: &OraclePolicyState,
    market: &MarketRiskState,
    primary_oracle: &AccountInfo<'_>,
    secondary_oracle: Option<&AccountInfo<'_>>,
    now_unix_ts: i64,
) -> Result<((u64, i64), Option<(u64, i64)>), ProgramError> {
    let (primary_provider, primary_price, primary_publish_time) = read_policy_oracle_price(
        program_id,
        policy,
        &policy.primary_feed_id.0,
        primary_oracle,
        now_unix_ts,
    )?;

    match (policy.oracle_count, secondary_oracle) {
        (1, None) => Ok(((primary_price, primary_publish_time), None)),
        (2, Some(secondary_oracle)) => {
            let (secondary_provider, secondary_price, secondary_publish_time) =
                read_policy_oracle_price(
                    program_id,
                    policy,
                    &policy.secondary_feed_id.0,
                    secondary_oracle,
                    now_unix_ts,
                )?;
            if secondary_provider == primary_provider {
                return Err(BoundaryError::InvalidOracle.into());
            }
            let divergence = price_divergence_bps_ceil(primary_price, secondary_price)
                .ok_or(BoundaryError::ArithmeticOverflow)?;
            if divergence > u64::from(market.divergence_limit_bps.get()) {
                return Err(BoundaryError::OracleDivergence.into());
            }
            Ok((
                (primary_price, primary_publish_time),
                Some((secondary_price, secondary_publish_time)),
            ))
        }
        _ => Err(BoundaryError::InvalidAccountCount.into()),
    }
}

fn read_policy_exit_prices(
    program_id: &Pubkey,
    policy: &OraclePolicyState,
    market: &MarketRiskState,
    primary_oracle: &AccountInfo<'_>,
    secondary_oracle: Option<&AccountInfo<'_>>,
    now_unix_ts: i64,
) -> Result<(u64, Option<u64>), ProgramError> {
    let (primary, secondary) = read_policy_prices(
        program_id,
        policy,
        market,
        primary_oracle,
        secondary_oracle,
        now_unix_ts,
    )?;
    Ok((primary.0, secondary.map(|observation| observation.0)))
}

fn price_divergence_bps_ceil(left: u64, right: u64) -> Option<u64> {
    let denominator = core::cmp::min(left, right);
    if denominator == 0 || left == right {
        return (denominator != 0).then_some(0);
    }
    let numerator = u128::from(left.abs_diff(right)).checked_mul(10_000)?;
    let denominator = u128::from(denominator);
    let bps = numerator
        .checked_add(denominator.checked_sub(1)?)?
        .checked_div(denominator)?;
    u64::try_from(bps).ok()
}

struct RegistrationAccounts<'a, 'info> {
    authority: &'a AccountInfo<'info>,
    pool: &'a AccountInfo<'info>,
    target: &'a AccountInfo<'info>,
    system_program: &'a AccountInfo<'info>,
    pool_state: PoolState,
    target_bump: u8,
}

fn validate_market_registration<'a, 'info>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'info>],
    market_id: u16,
    divergence_limit_bps: u16,
    max_long_open_interest: u128,
    max_short_open_interest: u128,
    haircut: HaircutConfig,
) -> Result<RegistrationAccounts<'a, 'info>, ProgramError> {
    let boundary = validate_market_identity(program_id, accounts, market_id)?;
    if market_id == 0
        || divergence_limit_bps == 0
        || divergence_limit_bps > 10_000
        || max_long_open_interest == 0
        || max_long_open_interest > boundary.pool_state.total_liability_open_gate.get()
        || max_short_open_interest == 0
        || max_short_open_interest > boundary.pool_state.total_liability_open_gate.get()
        || haircut.validate().is_err()
    {
        return Err(BoundaryError::InvalidMarketLimits.into());
    }
    Ok(boundary)
}

fn validate_market_identity<'a, 'info>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'info>],
    market_id: u16,
) -> Result<RegistrationAccounts<'a, 'info>, ProgramError> {
    let (authority, pool, target, system_program_ai, pool_state) =
        validate_registration_accounts(program_id, accounts)?;
    if authority.key.to_bytes() != pool_state.admin.0 {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    if market_id == 0 {
        return Err(BoundaryError::InvalidMarketLimits.into());
    }
    let (expected_target, target_bump) = derive_market(program_id, pool.key, market_id);
    if target.key != &expected_target {
        return Err(BoundaryError::InvalidPda.into());
    }
    Ok(RegistrationAccounts {
        authority,
        pool,
        target,
        system_program: system_program_ai,
        pool_state,
        target_bump,
    })
}

fn validate_controller_registration<'a, 'info>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'info>],
) -> Result<RegistrationAccounts<'a, 'info>, ProgramError> {
    let (authority, pool, target, system_program_ai, pool_state) =
        validate_registration_accounts(program_id, accounts)?;
    let (expected_target, target_bump) = derive_controller(program_id, pool.key, authority.key);
    if target.key != &expected_target {
        return Err(BoundaryError::InvalidPda.into());
    }
    Ok(RegistrationAccounts {
        authority,
        pool,
        target,
        system_program: system_program_ai,
        pool_state,
        target_bump,
    })
}

fn validate_registration_accounts<'a, 'info>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'info>],
) -> Result<
    (
        &'a AccountInfo<'info>,
        &'a AccountInfo<'info>,
        &'a AccountInfo<'info>,
        &'a AccountInfo<'info>,
        PoolState,
    ),
    ProgramError,
> {
    if accounts.len() != REGISTRATION_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let pool = &accounts[1];
    let target = &accounts[2];
    let system_program_ai = &accounts[3];

    validate_payer_authority(authority)?;
    expect_readonly(pool)?;
    let pool_state = load_canonical_pool(program_id, pool)?;
    expect_writable(target)?;
    expect_nonsigner(target)?;
    expect_readonly(system_program_ai)?;
    if system_program_ai.key != &system_program::ID || !system_program_ai.executable {
        return Err(BoundaryError::InvalidProgram.into());
    }
    Ok((authority, pool, target, system_program_ai, pool_state))
}

fn validate_payer_authority(authority: &AccountInfo<'_>) -> ProgramResult {
    expect_signer(authority)?;
    expect_writable(authority)?;
    if authority.owner != &system_program::ID || authority.executable || authority.data_len() != 0 {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    Ok(())
}

fn validate_controller_authority(authority: &AccountInfo<'_>) -> ProgramResult {
    expect_signer(authority)?;
    // Solana promotes the transaction fee payer to writable in the compiled
    // message even when the instruction requests a read-only signer. Browser
    // wallets normally pay their own fees, so writability cannot be rejected
    // here. This program never mutates or borrows authority data or lamports.
    if authority.owner != &system_program::ID || authority.executable || authority.data_len() != 0 {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    Ok(())
}

fn validate_external_program(account: &AccountInfo<'_>, expected: &Pubkey) -> ProgramResult {
    expect_readonly(account)?;
    if account.key != expected || !account.executable {
        return Err(BoundaryError::InvalidProgram.into());
    }
    Ok(())
}

fn load_canonical_pool(
    program_id: &Pubkey,
    pool: &AccountInfo<'_>,
) -> Result<PoolState, ProgramError> {
    let state = load_canonical_pool_any_status(program_id, pool)?;
    if state.status != PoolStatus::Active as u8 {
        return Err(BoundaryError::InvalidPool.into());
    }
    Ok(state)
}

fn load_canonical_pool_for_close(
    program_id: &Pubkey,
    pool: &AccountInfo<'_>,
) -> Result<PoolState, ProgramError> {
    let state = load_canonical_pool_any_status(program_id, pool)?;
    if !matches!(
        PoolStatus::try_from(state.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    ) {
        return Err(BoundaryError::InvalidPool.into());
    }
    Ok(state)
}

fn load_canonical_pool_for_claim_service(
    program_id: &Pubkey,
    pool: &AccountInfo<'_>,
) -> Result<PoolState, ProgramError> {
    let state = load_canonical_pool_any_status(program_id, pool)?;
    if !matches!(
        PoolStatus::try_from(state.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly | PoolStatus::ClaimsOnly
    ) {
        return Err(BoundaryError::InvalidPool.into());
    }
    Ok(state)
}

fn load_canonical_pool_any_status(
    program_id: &Pubkey,
    pool: &AccountInfo<'_>,
) -> Result<PoolState, ProgramError> {
    if pool.owner != program_id
        || pool.executable
        || pool.data_len() != account_len::<PoolState>()
        || !Rent::get()?.is_exempt(pool.lamports(), pool.data_len())
    {
        return Err(BoundaryError::InvalidPool.into());
    }
    let data = pool.try_borrow_data()?;
    let header = decode_account_header::<PoolState>(&data)?;
    let state = decode_account::<PoolState>(&data)?;
    let admin = Pubkey::new_from_array(state.admin.0);
    let quote_mint = Pubkey::new_from_array(state.quote_mint.0);
    let (expected_pool, expected_bump) =
        derive_pool(program_id, &admin, &quote_mint, state.risk_pool_id.get());
    let (vault_authority, _) = derive_vault_authority(program_id, pool.key);
    let expected_vault = derive_canonical_vault(&vault_authority, &quote_mint);
    if pool.key != &expected_pool
        || header.bump != expected_bump
        || state.vault.0 != expected_vault.to_bytes()
    {
        return Err(BoundaryError::InvalidPool.into());
    }
    Ok(state)
}

fn load_canonical_market(
    program_id: &Pubkey,
    pool: &Pubkey,
    market_id: u16,
    market: &AccountInfo<'_>,
) -> Result<MarketRiskState, ProgramError> {
    if market.owner != program_id
        || market.executable
        || market.data_len() != account_len::<MarketRiskState>()
        || !Rent::get()?.is_exempt(market.lamports(), market.data_len())
    {
        return Err(BoundaryError::InvalidMarket.into());
    }
    let data = market.try_borrow_data()?;
    let header = decode_account_header::<MarketRiskState>(&data)?;
    let state = decode_account::<MarketRiskState>(&data)?;
    let (expected, bump) = derive_market(program_id, pool, market_id);
    if market.key != &expected
        || header.bump != bump
        || state.pool.0 != pool.to_bytes()
        || state.market_id.get() != market_id
    {
        return Err(BoundaryError::InvalidMarket.into());
    }
    Ok(state)
}

fn load_canonical_oracle_policy(
    program_id: &Pubkey,
    pool: &Pubkey,
    market: &Pubkey,
    policy: &AccountInfo<'_>,
) -> Result<OraclePolicyState, ProgramError> {
    if policy.owner != program_id
        || policy.executable
        || policy.data_len() != account_len::<OraclePolicyState>()
        || !Rent::get()?.is_exempt(policy.lamports(), policy.data_len())
    {
        return Err(BoundaryError::InvalidOraclePolicy.into());
    }
    let data = policy.try_borrow_data()?;
    let header = decode_account_header::<OraclePolicyState>(&data)?;
    let state = decode_account::<OraclePolicyState>(&data)?;
    let (expected, bump) = derive_oracle_policy(program_id, market);
    if policy.key != &expected
        || header.bump != bump
        || state.pool.0 != pool.to_bytes()
        || state.market.0 != market.to_bytes()
    {
        return Err(BoundaryError::InvalidOraclePolicy.into());
    }
    Ok(state)
}

fn load_canonical_oracle_relay(
    program_id: &Pubkey,
    relay: &AccountInfo<'_>,
) -> Result<OracleRelayState, ProgramError> {
    if relay.owner != program_id
        || relay.executable
        || relay.data_len() != account_len::<OracleRelayState>()
        || !Rent::get()?.is_exempt(relay.lamports(), relay.data_len())
    {
        return Err(BoundaryError::InvalidOracle.into());
    }
    let data = relay.try_borrow_data()?;
    let header = decode_account_header::<OracleRelayState>(&data)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    let state = decode_account::<OracleRelayState>(&data)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    let market = Pubkey::new_from_array(state.market.0);
    let (expected, bump) = derive_oracle_relay(program_id, &market, &state.feed_id.0);
    if relay.key != &expected || header.bump != bump {
        return Err(BoundaryError::InvalidOracle.into());
    }
    Ok(state)
}

fn load_canonical_controller(
    program_id: &Pubkey,
    pool: &Pubkey,
    authority: &Pubkey,
    controller: &AccountInfo<'_>,
) -> Result<ControllerRiskState, ProgramError> {
    if controller.owner != program_id
        || controller.executable
        || controller.data_len() != account_len::<ControllerRiskState>()
        || !Rent::get()?.is_exempt(controller.lamports(), controller.data_len())
    {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    let data = controller.try_borrow_data()?;
    let header = decode_account_header::<ControllerRiskState>(&data)?;
    let state = decode_account::<ControllerRiskState>(&data)?;
    let (expected, bump) = derive_controller(program_id, pool, authority);
    if controller.key != &expected
        || header.bump != bump
        || state.pool.0 != pool.to_bytes()
        || state.controller.0 != authority.to_bytes()
    {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    Ok(state)
}

fn load_canonical_trading_session(
    program_id: &Pubkey,
    pool: &Pubkey,
    owner: &Pubkey,
    session: &AccountInfo<'_>,
) -> Result<TradingSessionState, ProgramError> {
    if session.owner != program_id
        || session.executable
        || session.data_len() != account_len::<TradingSessionState>()
        || !Rent::get()?.is_exempt(session.lamports(), session.data_len())
    {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    let data = session.try_borrow_data()?;
    let header = decode_account_header::<TradingSessionState>(&data)?;
    let state = decode_account::<TradingSessionState>(&data)?;
    let (expected, bump) = derive_trading_session(program_id, pool, owner);
    if session.key != &expected
        || header.bump != bump
        || state.pool.0 != pool.to_bytes()
        || state.owner.0 != owner.to_bytes()
    {
        return Err(BoundaryError::InvalidAuthority.into());
    }
    Ok(state)
}

fn load_canonical_position(
    program_id: &Pubkey,
    pool: &Pubkey,
    controller: &Pubkey,
    position_id: u64,
    position: &AccountInfo<'_>,
) -> Result<PositionState, ProgramError> {
    if position.owner != program_id
        || position.executable
        || position.data_len() != account_len::<PositionState>()
        || !Rent::get()?.is_exempt(position.lamports(), position.data_len())
    {
        return Err(BoundaryError::InvalidPosition.into());
    }
    let data = position.try_borrow_data()?;
    let header = decode_account_header::<PositionState>(&data)?;
    let state = decode_account::<PositionState>(&data)?;
    let (expected, bump) = derive_position(program_id, pool, controller, position_id);
    if position.key != &expected
        || header.bump != bump
        || state.pool.0 != pool.to_bytes()
        || state.controller.0 != controller.to_bytes()
        || state.position_id.get() != position_id
    {
        return Err(BoundaryError::InvalidPosition.into());
    }
    Ok(state)
}

fn load_self_canonical_position(
    program_id: &Pubkey,
    position: &AccountInfo<'_>,
) -> Result<PositionState, ProgramError> {
    if position.owner != program_id
        || position.executable
        || position.data_len() != account_len::<PositionState>()
        || !Rent::get()?.is_exempt(position.lamports(), position.data_len())
    {
        return Err(BoundaryError::InvalidPosition.into());
    }
    let data = position.try_borrow_data()?;
    let header = decode_account_header::<PositionState>(&data)?;
    let state = decode_account::<PositionState>(&data)?;
    let pool = Pubkey::new_from_array(state.pool.0);
    let controller = Pubkey::new_from_array(state.controller.0);
    let (expected, bump) = derive_position(program_id, &pool, &controller, state.position_id.get());
    if position.key != &expected || header.bump != bump {
        return Err(BoundaryError::InvalidPosition.into());
    }
    Ok(state)
}

fn load_canonical_claim(
    program_id: &Pubkey,
    pool: &Pubkey,
    claim: &AccountInfo<'_>,
) -> Result<ClaimState, ProgramError> {
    if claim.owner != program_id
        || claim.executable
        || claim.data_len() != account_len::<ClaimState>()
        || !Rent::get()?.is_exempt(claim.lamports(), claim.data_len())
    {
        return Err(BoundaryError::InvalidClaim.into());
    }
    let data = claim.try_borrow_data()?;
    let header = decode_account_header::<ClaimState>(&data)?;
    let state = decode_account::<ClaimState>(&data)?;
    let controller = Pubkey::new_from_array(state.controller.0);
    let (position, _) = derive_position(
        program_id,
        pool,
        &controller,
        state.origin_position_id.get(),
    );
    let (expected, bump) = derive_claim(program_id, &position);
    if claim.key != &expected || header.bump != bump || state.pool.0 != pool.to_bytes() {
        return Err(BoundaryError::InvalidClaim.into());
    }
    Ok(state)
}

fn load_canonical_queue_page(
    program_id: &Pubkey,
    pool: &Pubkey,
    page_index: u64,
    page: &AccountInfo<'_>,
) -> Result<Box<QueuePageState>, ProgramError> {
    if page.owner != program_id
        || page.executable
        || page.data_len() != account_len::<QueuePageState>()
        || !Rent::get()?.is_exempt(page.lamports(), page.data_len())
    {
        return Err(BoundaryError::InvalidQueuePage.into());
    }
    let data = page.try_borrow_data()?;
    let header = decode_account_header::<QueuePageState>(&data)?;
    let state = decode_account_boxed::<QueuePageState>(&data)?;
    let (expected, bump) = derive_queue_page(program_id, pool, page_index);
    if page.key != &expected
        || header.bump != bump
        || state.pool.0 != pool.to_bytes()
        || state.page_index.get() != page_index
    {
        return Err(BoundaryError::InvalidQueuePage.into());
    }
    Ok(state)
}

fn validate_source_token_account(
    account: &AccountInfo<'_>,
    mint: &Pubkey,
    authority: &Pubkey,
) -> Result<TokenAccount, ProgramError> {
    let state = load_classic_token_account(account)?;
    if state.mint != *mint
        || state.owner != *authority
        || state.state != AccountState::Initialized
        || state.delegate != COption::None
        || state.close_authority != COption::None
        || state.is_native != COption::None
    {
        return Err(BoundaryError::InvalidTokenAccount.into());
    }
    Ok(state)
}

fn validate_vault_token_account(
    account: &AccountInfo<'_>,
    mint: &Pubkey,
    authority: &Pubkey,
) -> Result<TokenAccount, ProgramError> {
    let state = load_classic_token_account(account)?;
    if state.mint != *mint
        || state.owner != *authority
        || state.state != AccountState::Initialized
        || state.delegate != COption::None
        || state.close_authority != COption::None
        || state.is_native != COption::None
    {
        return Err(BoundaryError::InvalidVault.into());
    }
    Ok(state)
}

fn validate_destination_token_account(
    account: &AccountInfo<'_>,
    mint: &Pubkey,
    authority: &Pubkey,
) -> Result<TokenAccount, ProgramError> {
    let state = load_classic_token_account(account)?;
    if state.mint != *mint
        || state.owner != *authority
        || state.state != AccountState::Initialized
        || state.is_native != COption::None
    {
        return Err(BoundaryError::InvalidTokenAccount.into());
    }
    Ok(state)
}

fn load_classic_token_account(account: &AccountInfo<'_>) -> Result<TokenAccount, ProgramError> {
    if account.owner != &spl_token::ID
        || account.executable
        || account.data_len() != TokenAccount::LEN
        || !Rent::get()?.is_exempt(account.lamports(), account.data_len())
    {
        return Err(BoundaryError::InvalidTokenAccount.into());
    }
    TokenAccount::unpack(&account.try_borrow_data()?)
        .map_err(|_| BoundaryError::InvalidTokenAccount.into())
}

struct BoundaryAccounts<'a, 'info> {
    admin: &'a AccountInfo<'info>,
    quote_mint: &'a AccountInfo<'info>,
    pool: &'a AccountInfo<'info>,
    page_zero: &'a AccountInfo<'info>,
    system_program: &'a AccountInfo<'info>,
    pool_bump: u8,
    page_bump: u8,
}

fn validate_common<'a, 'info>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'info>],
    risk_pool_id: u64,
) -> Result<BoundaryAccounts<'a, 'info>, ProgramError> {
    if accounts.len() != POOL_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let admin = &accounts[0];
    let quote_mint = &accounts[1];
    let pool = &accounts[2];
    let page_zero = &accounts[3];
    let system_program_ai = &accounts[4];

    expect_signer(admin)?;
    expect_writable(admin)?;
    if admin.owner != &system_program::ID || admin.executable || admin.data_len() != 0 {
        return Err(BoundaryError::InvalidFreshAccount.into());
    }
    expect_readonly(quote_mint)?;
    validate_quote_mint(quote_mint)?;
    expect_writable(pool)?;
    expect_nonsigner(pool)?;
    expect_writable(page_zero)?;
    expect_nonsigner(page_zero)?;
    expect_readonly(system_program_ai)?;
    if system_program_ai.key != &system_program::ID || !system_program_ai.executable {
        return Err(BoundaryError::InvalidProgram.into());
    }

    let (expected_pool, pool_bump) =
        derive_pool(program_id, admin.key, quote_mint.key, risk_pool_id);
    if pool.key != &expected_pool {
        return Err(BoundaryError::InvalidPda.into());
    }
    let (expected_page, page_bump) = derive_queue_page_zero(program_id, pool.key);
    if page_zero.key != &expected_page {
        return Err(BoundaryError::InvalidPda.into());
    }

    Ok(BoundaryAccounts {
        admin,
        quote_mint,
        pool,
        page_zero,
        system_program: system_program_ai,
        pool_bump,
        page_bump,
    })
}

fn ensure_distinct(accounts: &[AccountInfo<'_>]) -> ProgramResult {
    for (index, account) in accounts.iter().enumerate() {
        if accounts[..index]
            .iter()
            .any(|earlier| earlier.key == account.key)
        {
            return Err(BoundaryError::DuplicateAccount.into());
        }
    }
    Ok(())
}

fn expect_signer(account: &AccountInfo<'_>) -> ProgramResult {
    if !account.is_signer {
        return Err(BoundaryError::ExpectedSigner.into());
    }
    Ok(())
}

fn expect_nonsigner(account: &AccountInfo<'_>) -> ProgramResult {
    if account.is_signer {
        return Err(BoundaryError::ExpectedReadonly.into());
    }
    Ok(())
}

fn expect_writable(account: &AccountInfo<'_>) -> ProgramResult {
    if !account.is_writable {
        return Err(BoundaryError::ExpectedWritable.into());
    }
    Ok(())
}

fn expect_readonly(account: &AccountInfo<'_>) -> ProgramResult {
    if account.is_signer || account.is_writable {
        return Err(BoundaryError::ExpectedReadonly.into());
    }
    Ok(())
}

fn validate_quote_mint(quote_mint: &AccountInfo<'_>) -> ProgramResult {
    if quote_mint.owner != &spl_token::ID
        || quote_mint.executable
        || quote_mint.data_len() != Mint::LEN
        || !Rent::get()?.is_exempt(quote_mint.lamports(), quote_mint.data_len())
    {
        return Err(BoundaryError::InvalidMint.into());
    }
    let mint = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?;
    if !mint.is_initialized {
        return Err(BoundaryError::InvalidMint.into());
    }
    if mint.decimals != V1_QUOTE_DECIMALS {
        return Err(BoundaryError::InvalidQuoteDecimals.into());
    }
    Ok(())
}

fn validate_fresh_target(target: &AccountInfo<'_>) -> ProgramResult {
    if target.owner != &system_program::ID || target.executable || target.data_len() != 0 {
        return Err(BoundaryError::InvalidFreshAccount.into());
    }
    Ok(())
}

fn validate_prepared_target(
    target: &AccountInfo<'_>,
    program_id: &Pubkey,
    expected_len: usize,
) -> ProgramResult {
    if target.owner != program_id
        || target.executable
        || target.data_len() != expected_len
        || !target.try_borrow_data()?.iter().all(|byte| *byte == 0)
        || !Rent::get()?.is_exempt(target.lamports(), expected_len)
    {
        return Err(BoundaryError::InvalidPreparedAccount.into());
    }
    Ok(())
}

fn allocate_pda<'a>(
    payer: &AccountInfo<'a>,
    target: &AccountInfo<'a>,
    system_program_ai: &AccountInfo<'a>,
    owner: &Pubkey,
    space: usize,
    signer_seeds: &[&[u8]],
) -> ProgramResult {
    let minimum_balance = Rent::get()?.minimum_balance(space);
    let shortfall = minimum_balance.saturating_sub(target.lamports());
    if shortfall != 0 {
        invoke(
            &system_instruction::transfer(payer.key, target.key, shortfall),
            &[payer.clone(), target.clone(), system_program_ai.clone()],
        )?;
    }
    let space = u64::try_from(space).map_err(|_| BoundaryError::ArithmeticOverflow)?;
    invoke_signed(
        &system_instruction::allocate(target.key, space),
        &[target.clone(), system_program_ai.clone()],
        &[signer_seeds],
    )?;
    invoke_signed(
        &system_instruction::assign(target.key, owner),
        &[target.clone(), system_program_ai.clone()],
        &[signer_seeds],
    )
}
