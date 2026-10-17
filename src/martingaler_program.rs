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
    initialize_queue_page_zero_account, open_cash_position_from_idle,
    open_claim_position_from_queue, overwrite_account, pop_fifo_head_with_claim,
    record_cash_deposit, resolve_bankrupt_claim_position, service_fifo_claim,
    transition_market_status, transition_pool_status, validate_queue_page_zero_account,
    withdraw_idle_cash, ClaimState, ControllerRiskState, HaircutConfig, Key32, MarketRiskState,
    OraclePolicyState, PoolState, PoolStatus, PositionSide, PositionState, QueuePageState,
    MAX_ORACLE_CONFIDENCE_BPS, MAX_ORACLE_STALENESS_SECS,
};
use alloc::{boxed::Box, vec::Vec};
use percolator_martingaler_engine::martingaler::{
    V1_MAX_LEVERAGE_X, V1_MAX_POSITION_NOTIONAL_QUOTE, V1_QUOTE_DECIMALS,
    V1_TOTAL_POTENTIAL_LIABILITY_QUOTE,
};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction as SolanaInstruction},
    program::{invoke, invoke_signed},
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
pub const MARTINGALER_PROGRAM_ERROR_BASE: u32 = 31_000;

const POOL_SEED: &[u8] = b"martingaler_pool";
const QUEUE_SEED: &[u8] = b"martingaler_queue";
const VAULT_SEED: &[u8] = b"martingaler_vault";
const MARKET_SEED: &[u8] = b"martingaler_market";
const CONTROLLER_SEED: &[u8] = b"martingaler_controller";
const ORACLE_POLICY_SEED: &[u8] = b"martingaler_oracle";
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
const POP_FIFO_HEAD_BASE_ACCOUNT_COUNT: usize = 3;
const OPEN_CLAIM_BASE_ACCOUNT_COUNT: usize = 10;
const CLAIM_CLOSE_BASE_ACCOUNT_COUNT: usize = 13;
const CLAIM_BANKRUPTCY_BASE_ACCOUNT_COUNT: usize = 8;
const SET_POOL_STATUS_ACCOUNT_COUNT: usize = 2;
const SET_MARKET_STATUS_ACCOUNT_COUNT: usize = 3;
const WITHDRAW_IDLE_CASH_ACCOUNT_COUNT: usize = 8;

const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

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
            Self::CloseNonPositiveCashPosition { .. } => (CLOSE_NONPOSITIVE_CASH_POSITION_TAG, 8),
            Self::ResolveBankruptCashPosition { .. } => (RESOLVE_BANKRUPT_CASH_POSITION_TAG, 8),
            Self::PrepareClaim { .. } => (PREPARE_CLAIM_TAG, 8),
            Self::PrepareQueuePage { .. } => (PREPARE_QUEUE_PAGE_TAG, 8),
            Self::CloseProfitableCashPosition { .. } => (CLOSE_PROFITABLE_CASH_POSITION_TAG, 9),
            Self::ServiceFifoHead => (SERVICE_FIFO_HEAD_TAG, 0),
            Self::PopFifoHead { .. } => (POP_FIFO_HEAD_TAG, 1),
            Self::OpenClaimPosition { .. } => (OPEN_CLAIM_POSITION_TAG, 29),
            Self::CloseNonPositiveClaimPosition { .. } => (CLOSE_NONPOSITIVE_CLAIM_POSITION_TAG, 9),
            Self::ResolveBankruptClaimPosition { .. } => (RESOLVE_BANKRUPT_CLAIM_POSITION_TAG, 8),
            Self::CloseProfitableClaimPosition { .. } => (CLOSE_PROFITABLE_CLAIM_POSITION_TAG, 9),
            Self::SetPoolStatus { .. } => (SET_POOL_STATUS_TAG, 1),
            Self::SetMarketStatus { .. } => (SET_MARKET_STATUS_TAG, 1),
            Self::WithdrawIdleCash { .. } => (WITHDRAW_IDLE_CASH_TAG, 8),
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
            Self::PrepareController
            | Self::InitializeController
            | Self::InitializeVault
            | Self::ServiceFifoHead => {}
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

pub fn derive_oracle_policy(program_id: &Pubkey, market: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ORACLE_POLICY_SEED, market.as_ref()], program_id)
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
            process_prepare_position(program_id, accounts, position_id)
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
        ),
        Instruction::CloseNonPositiveCashPosition { position_id } => {
            process_close_nonpositive_cash_position(program_id, accounts, position_id)
        }
        Instruction::ResolveBankruptCashPosition { position_id } => {
            process_resolve_bankrupt_cash_position(program_id, accounts, position_id)
        }
        Instruction::PrepareClaim { position_id } => {
            process_prepare_claim(program_id, accounts, position_id)
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
        ),
        Instruction::ServiceFifoHead => process_service_fifo_head(program_id, accounts),
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
    }
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
) -> ProgramResult {
    let boundary = validate_position_preparation(program_id, accounts, position_id)?;
    validate_fresh_target(boundary.target)?;

    let position_id_bytes = position_id.to_le_bytes();
    let target_bump = [boundary.target_bump];
    let signer_seeds: &[&[u8]] = &[
        POSITION_SEED,
        boundary.pool.key.as_ref(),
        boundary.authority.key.as_ref(),
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

    validate_payer_authority(authority)?;
    // The bundled open instruction mutates pool/controller, so their
    // transaction-wide privileges remain writable during preparation.
    expect_nonsigner(pool)?;
    load_canonical_pool(program_id, pool)?;
    expect_nonsigner(controller)?;
    load_canonical_controller(program_id, pool.key, authority.key, controller)?;
    expect_writable(target)?;
    expect_nonsigner(target)?;
    validate_external_program(system_program_ai, &system_program::ID)?;
    let (expected_target, target_bump) =
        derive_position(program_id, pool.key, authority.key, position_id);
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
) -> ProgramResult {
    if accounts.len() != PREPARE_CLAIM_ACCOUNT_COUNT {
        return Err(BoundaryError::InvalidAccountCount.into());
    }
    ensure_distinct(accounts)?;
    let authority = &accounts[0];
    let position = &accounts[1];
    let claim = &accounts[2];
    let system_program_ai = &accounts[3];
    validate_payer_authority(authority)?;
    expect_writable(position)?;
    expect_nonsigner(position)?;
    let position_state = load_self_canonical_position(program_id, position)?;
    if position_state.position_id.get() != position_id
        || position_state.controller.0 != authority.key.to_bytes()
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
        load_canonical_controller(program_id, pool.key, authority.key, controller)?;
    expect_writable(position)?;
    expect_nonsigner(position)?;
    let (expected_position, position_bump) =
        derive_position(program_id, pool.key, authority.key, position_id);
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

    let primary_provider = validate_oracle_account(primary_oracle)?;
    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (entry_price_e6, entry_publish_time) = crate::oracle_v16::read_oracle_price_e6(
        primary_oracle,
        &policy_state.primary_feed_id.0,
        now_unix_ts,
        policy_state.max_staleness_secs.get(),
        policy_state.confidence_limit_bps.get(),
    )
    .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;

    let (secondary_entry_price_e6, secondary_publish_time) = if policy_state.oracle_count == 2 {
        let secondary_oracle = &accounts[8];
        let secondary_provider = validate_oracle_account(secondary_oracle)?;
        if secondary_provider == primary_provider {
            return Err(BoundaryError::InvalidOracle.into());
        }
        let observation = crate::oracle_v16::read_oracle_price_e6(
            secondary_oracle,
            &policy_state.secondary_feed_id.0,
            now_unix_ts,
            policy_state.max_staleness_secs.get(),
            policy_state.confidence_limit_bps.get(),
        )
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
        let divergence = price_divergence_bps_ceil(entry_price_e6, observation.0)
            .ok_or(BoundaryError::ArithmeticOverflow)?;
        if divergence > u64::from(market_state.divergence_limit_bps.get()) {
            return Err(BoundaryError::OracleDivergence.into());
        }
        observation
    } else {
        (0, 0)
    };

    let outcome = open_cash_position_from_idle(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(authority.key.to_bytes()),
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
        load_canonical_controller(program_id, pool.key, authority.key, controller)?;
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
        derive_position(program_id, pool.key, authority.key, position_id);
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

    let primary_provider = validate_oracle_account(primary_oracle)?;
    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (entry_price_e6, entry_publish_time) = crate::oracle_v16::read_oracle_price_e6(
        primary_oracle,
        &policy_state.primary_feed_id.0,
        now_unix_ts,
        policy_state.max_staleness_secs.get(),
        policy_state.confidence_limit_bps.get(),
    )
    .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    let (secondary_entry_price_e6, secondary_publish_time) = if policy_state.oracle_count == 2 {
        let secondary_oracle = &accounts[10];
        let secondary_provider = validate_oracle_account(secondary_oracle)?;
        if secondary_provider == primary_provider {
            return Err(BoundaryError::InvalidOracle.into());
        }
        let observation = crate::oracle_v16::read_oracle_price_e6(
            secondary_oracle,
            &policy_state.secondary_feed_id.0,
            now_unix_ts,
            policy_state.max_staleness_secs.get(),
            policy_state.confidence_limit_bps.get(),
        )
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
        let divergence = price_divergence_bps_ceil(entry_price_e6, observation.0)
            .ok_or(BoundaryError::ArithmeticOverflow)?;
        if divergence > u64::from(market_state.divergence_limit_bps.get()) {
            return Err(BoundaryError::OracleDivergence.into());
        }
        observation
    } else {
        (0, 0)
    };

    let outcome = open_claim_position_from_queue(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(authority.key.to_bytes()),
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
    Ok(())
}

#[inline(never)]
fn process_close_nonpositive_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
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
        load_canonical_position(program_id, pool.key, authority.key, position_id, position)?;
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
        load_canonical_controller(program_id, pool.key, authority.key, controller)?;
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

    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (exit_price_e6, secondary_exit_price_e6) = read_policy_exit_prices(
        &policy_state,
        &market_state,
        primary_oracle,
        (policy_state.oracle_count == 2).then(|| &accounts[12]),
        now_unix_ts,
    )?;
    let outcome = close_cash_position(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(authority.key.to_bytes()),
        &mut pool_state,
        &mut market_state,
        &mut controller_state,
        &mut position_state,
        vault_before.amount,
        exit_price_e6,
        secondary_exit_price_e6,
        false,
    )?;

    if outcome.principal_refund != 0 {
        let transfer = spl_token::instruction::transfer_checked(
            &spl_token::ID,
            vault.key,
            quote_mint.key,
            destination.key,
            vault_authority.key,
            &[],
            outcome.principal_refund,
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
    }

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
            .checked_sub(outcome.principal_refund)
            .ok_or(BoundaryError::InvalidTokenDelta)?
        || destination_after.amount
            != destination_before
                .amount
                .checked_add(outcome.principal_refund)
                .ok_or(BoundaryError::InvalidTokenDelta)?
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
    Ok(())
}

#[inline(never)]
fn process_close_profitable_cash_position<'a>(
    program_id: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    position_id: u64,
    settlement_shape: ProfitSettlementShape,
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
        load_canonical_position(program_id, pool.key, authority.key, position_id, position)?;
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
        load_canonical_controller(program_id, pool.key, authority.key, controller)?;
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

    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (exit_price_e6, secondary_exit_price_e6) = read_policy_exit_prices(
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

    let outcome = close_profitable_cash_position(
        Key32::new(pool.key.to_bytes()),
        Key32::new(market.key.to_bytes()),
        Key32::new(authority.key.to_bytes()),
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

    let transfer_amount = outcome
        .principal_refund
        .checked_add(outcome.immediate_profit_paid)
        .ok_or(BoundaryError::ArithmeticOverflow)?;
    if transfer_amount != 0 {
        let transfer = spl_token::instruction::transfer_checked(
            &spl_token::ID,
            vault.key,
            quote_mint.key,
            destination.key,
            vault_authority.key,
            &[],
            transfer_amount,
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
    }

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
            .checked_sub(transfer_amount)
            .ok_or(BoundaryError::InvalidTokenDelta)?
        || destination_after.amount
            != destination_before
                .amount
                .checked_add(transfer_amount)
                .ok_or(BoundaryError::InvalidTokenDelta)?
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
        load_canonical_position(program_id, pool.key, authority.key, position_id, position)?;
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
        load_canonical_controller(program_id, pool.key, authority.key, controller)?;
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

    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (exit_price_e6, secondary_exit_price_e6) = read_policy_exit_prices(
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

    let outcome = match close_kind {
        ClaimVoluntaryCloseKind::NonPositive => close_nonpositive_claim_position(
            Key32::new(pool.key.to_bytes()),
            Key32::new(market.key.to_bytes()),
            Key32::new(authority.key.to_bytes()),
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
            Key32::new(authority.key.to_bytes()),
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

    if outcome.immediate_payout != 0 {
        let transfer = spl_token::instruction::transfer_checked(
            &spl_token::ID,
            vault.key,
            quote_mint.key,
            destination.key,
            vault_authority.key,
            &[],
            outcome.immediate_payout,
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
    }

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
            .checked_sub(outcome.immediate_payout)
            .ok_or(BoundaryError::InvalidTokenDelta)?
        || destination_after.amount
            != destination_before
                .amount
                .checked_add(outcome.immediate_payout)
                .ok_or(BoundaryError::InvalidTokenDelta)?
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

    let outcome = service_fifo_claim(
        Key32::new(pool.key.to_bytes()),
        &mut pool_state,
        &mut controller_state,
        head_page_state.as_mut(),
        Key32::new(claim.key.to_bytes()),
        &mut claim_state,
    )?;
    let payment = u64::try_from(outcome.paid).map_err(|_| BoundaryError::ArithmeticOverflow)?;
    let transfer = spl_token::instruction::transfer_checked(
        &spl_token::ID,
        vault.key,
        quote_mint.key,
        destination.key,
        vault_authority.key,
        &[],
        payment,
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
        validate_destination_token_account(destination, quote_mint.key, &controller_key)?;
    let mint_supply_after = Mint::unpack(&quote_mint.try_borrow_data()?)
        .map_err(|_| ProgramError::from(BoundaryError::InvalidMint))?
        .supply;
    if vault_after.amount
        != vault_before
            .amount
            .checked_sub(payment)
            .ok_or(BoundaryError::InvalidTokenDelta)?
        || destination_after.amount
            != destination_before
                .amount
                .checked_add(payment)
                .ok_or(BoundaryError::InvalidTokenDelta)?
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
    let primary_provider = validate_oracle_account(primary_oracle)?;
    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (primary_exit_price_e6, _) = crate::oracle_v16::read_oracle_price_e6(
        primary_oracle,
        &policy_state.primary_feed_id.0,
        now_unix_ts,
        policy_state.max_staleness_secs.get(),
        policy_state.confidence_limit_bps.get(),
    )
    .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    let secondary_exit_price_e6 = if policy_state.oracle_count == 2 {
        let secondary_oracle = &accounts[7];
        let secondary_provider = validate_oracle_account(secondary_oracle)?;
        if secondary_provider == primary_provider {
            return Err(BoundaryError::InvalidOracle.into());
        }
        let (secondary_price, _) = crate::oracle_v16::read_oracle_price_e6(
            secondary_oracle,
            &policy_state.secondary_feed_id.0,
            now_unix_ts,
            policy_state.max_staleness_secs.get(),
            policy_state.confidence_limit_bps.get(),
        )
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
        let divergence = price_divergence_bps_ceil(primary_exit_price_e6, secondary_price)
            .ok_or(BoundaryError::ArithmeticOverflow)?;
        if divergence > u64::from(market_state.divergence_limit_bps.get()) {
            return Err(BoundaryError::OracleDivergence.into());
        }
        Some(secondary_price)
    } else {
        None
    };

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
    let primary_provider = validate_oracle_account(primary_oracle)?;
    let now_unix_ts = Clock::get()?.unix_timestamp;
    let (primary_exit_price_e6, _) = crate::oracle_v16::read_oracle_price_e6(
        primary_oracle,
        &policy_state.primary_feed_id.0,
        now_unix_ts,
        policy_state.max_staleness_secs.get(),
        policy_state.confidence_limit_bps.get(),
    )
    .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
    let secondary_exit_price_e6 = if policy_state.oracle_count == 2 {
        let secondary_oracle = &accounts[8];
        let secondary_provider = validate_oracle_account(secondary_oracle)?;
        if secondary_provider == primary_provider {
            return Err(BoundaryError::InvalidOracle.into());
        }
        let (secondary_price, _) = crate::oracle_v16::read_oracle_price_e6(
            secondary_oracle,
            &policy_state.secondary_feed_id.0,
            now_unix_ts,
            policy_state.max_staleness_secs.get(),
            policy_state.confidence_limit_bps.get(),
        )
        .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
        let divergence = price_divergence_bps_ceil(primary_exit_price_e6, secondary_price)
            .ok_or(BoundaryError::ArithmeticOverflow)?;
        if divergence > u64::from(market_state.divergence_limit_bps.get()) {
            return Err(BoundaryError::OracleDivergence.into());
        }
        Some(secondary_price)
    } else {
        None
    };

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
}

fn validate_oracle_account(
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

fn read_policy_exit_prices(
    policy: &OraclePolicyState,
    market: &MarketRiskState,
    primary_oracle: &AccountInfo<'_>,
    secondary_oracle: Option<&AccountInfo<'_>>,
    now_unix_ts: i64,
) -> Result<(u64, Option<u64>), ProgramError> {
    let primary_provider = validate_oracle_account(primary_oracle)?;
    let (primary_price, _) = crate::oracle_v16::read_oracle_price_e6(
        primary_oracle,
        &policy.primary_feed_id.0,
        now_unix_ts,
        policy.max_staleness_secs.get(),
        policy.confidence_limit_bps.get(),
    )
    .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;

    match (policy.oracle_count, secondary_oracle) {
        (1, None) => Ok((primary_price, None)),
        (2, Some(secondary_oracle)) => {
            let secondary_provider = validate_oracle_account(secondary_oracle)?;
            if secondary_provider == primary_provider {
                return Err(BoundaryError::InvalidOracle.into());
            }
            let (secondary_price, _) = crate::oracle_v16::read_oracle_price_e6(
                secondary_oracle,
                &policy.secondary_feed_id.0,
                now_unix_ts,
                policy.max_staleness_secs.get(),
                policy.confidence_limit_bps.get(),
            )
            .map_err(|_| ProgramError::from(BoundaryError::InvalidOracle))?;
            let divergence = price_divergence_bps_ceil(primary_price, secondary_price)
                .ok_or(BoundaryError::ArithmeticOverflow)?;
            if divergence > u64::from(market.divergence_limit_bps.get()) {
                return Err(BoundaryError::OracleDivergence.into());
            }
            Ok((primary_price, Some(secondary_price)))
        }
        _ => Err(BoundaryError::InvalidAccountCount.into()),
    }
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
    if authority.is_writable {
        return Err(BoundaryError::ExpectedReadonly.into());
    }
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
