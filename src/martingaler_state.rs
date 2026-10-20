//! Fixed-width on-chain state contract for Ninja Martingaler V1.
//!
//! This module intentionally contains no instruction dispatch, account
//! allocation, oracle handling, or token CPI. It defines the wire format and
//! bounded FIFO primitives that executable instructions will consume later.

use alloc::boxed::Box;
use bytemuck::{Pod, Zeroable};
use core::mem::size_of;
use solana_program::program_error::ProgramError;

pub const MARTINGALER_MAGIC: [u8; 8] = *b"NINMART\0";
pub const MARTINGALER_VERSION: u16 = 1;
pub const ACCOUNT_HEADER_LEN: usize = 16;
pub const QUEUE_PAGE_CAPACITY: usize = 32;
pub const NO_PAGE_INDEX: u64 = u64::MAX;
pub const NO_CLAIM_SEQUENCE: u128 = u128::MAX;
pub const MARTINGALER_ERROR_BASE: u32 = 30_000;
pub const QUEUE_FLAG_HEAD_SETTLED: u8 = 1;
pub const MAX_ORACLE_STALENESS_SECS: u64 = 300;
pub const MAX_ORACLE_CONFIDENCE_BPS: u16 = 1_000;
pub const MAX_PHOENIX_SOURCE_STALENESS_SLOTS: u64 = 10_000;
pub const V1_QUOTE_ATOMS_PER_UNIT: u128 = 1_000_000;
pub const V1_MAX_LEVERAGE_X: u16 = 1_000;
pub const V1_MAX_POSITION_NOTIONAL_QUOTE: u128 = 2_000_000 * V1_QUOTE_ATOMS_PER_UNIT;
pub const V1_MAX_NET_PROFIT_PER_POSITION_QUOTE: u128 = 200_000 * V1_QUOTE_ATOMS_PER_UNIT;
pub const V1_MAX_RESERVED_PROFIT_PER_CONTROLLER_QUOTE: u128 = 500_000 * V1_QUOTE_ATOMS_PER_UNIT;
pub const V1_TOTAL_POTENTIAL_LIABILITY_QUOTE: u128 = 2_000_000 * V1_QUOTE_ATOMS_PER_UNIT;
pub const V1_MIN_COLLATERAL_QUOTE: u128 = 10 * 1_000_000;
pub const V1_PAYOUT_FACTOR_SCALE_BPS: u16 = 10_000;
pub const V1_MIN_PAYOUT_FACTOR_BPS: u16 = 3_000;
const LEGACY_FULL_RESERVE_ADMISSION: u16 = u16::MAX;

pub const KIND_POOL: u8 = 1;
pub const KIND_MARKET_RISK: u8 = 2;
pub const KIND_CONTROLLER_RISK: u8 = 3;
pub const KIND_POSITION: u8 = 4;
pub const KIND_CLAIM: u8 = 5;
pub const KIND_QUEUE_PAGE: u8 = 6;
pub const KIND_ORACLE_POLICY: u8 = 7;
pub const KIND_ORACLE_RELAY: u8 = 8;
pub const KIND_TRADING_SESSION: u8 = 9;
pub const TRADING_SESSION_DURATION_SECS: i64 = 3_600;
pub const TRADING_SESSION_MAX_ACTIONS: u64 = 100;
pub const TRADING_SESSION_TOTAL_NOTIONAL_CAP: u128 = 10_000_000 * 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum StateError {
    WrongLength = 1,
    InvalidMagic = 2,
    InvalidVersion = 3,
    InvalidKind = 4,
    AlreadyInitialized = 5,
    ReservedNonZero = 6,
    InvalidTag = 7,
    InvalidState = 8,
    ArithmeticOverflow = 9,
    QueueFull = 10,
    QueueEmpty = 11,
    SequenceMismatch = 12,
    BrokenPageLink = 13,
    HeadNotSettled = 14,
    ClaimMismatch = 15,
    InsufficientClaimServiceCash = 16,
    CustodyShortfall = 17,
    CustodyCapacityExceeded = 18,
    InsufficientCashCollateral = 19,
    RiskLimitExceeded = 20,
    LiabilityLimitExceeded = 21,
    FavorablePosition = 22,
    PositionNotBankrupt = 23,
    PositionNotProfitable = 24,
    SettlementShapeMismatch = 25,
    InvalidStatusTransition = 26,
}

impl From<StateError> for ProgramError {
    fn from(error: StateError) -> Self {
        ProgramError::Custom(MARTINGALER_ERROR_BASE + error as u32)
    }
}

pub type StateResult<T> = Result<T, StateError>;

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct U16Le([u8; 2]);

impl U16Le {
    pub const fn new(value: u16) -> Self {
        Self(value.to_le_bytes())
    }

    pub const fn get(self) -> u16 {
        u16::from_le_bytes(self.0)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct U32Le([u8; 4]);

impl U32Le {
    pub const fn new(value: u32) -> Self {
        Self(value.to_le_bytes())
    }

    pub const fn get(self) -> u32 {
        u32::from_le_bytes(self.0)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct U64Le([u8; 8]);

impl U64Le {
    pub const fn new(value: u64) -> Self {
        Self(value.to_le_bytes())
    }

    pub const fn get(self) -> u64 {
        u64::from_le_bytes(self.0)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct I64Le([u8; 8]);

impl I64Le {
    pub const fn new(value: i64) -> Self {
        Self(value.to_le_bytes())
    }

    pub const fn get(self) -> i64 {
        i64::from_le_bytes(self.0)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct U128Le([u8; 16]);

impl U128Le {
    pub const fn new(value: u128) -> Self {
        Self(value.to_le_bytes())
    }

    pub const fn get(self) -> u128 {
        u128::from_le_bytes(self.0)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct Key32(pub [u8; 32]);

impl Key32 {
    pub const ZERO: Self = Self([0; 32]);

    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct AccountHeader {
    pub magic: [u8; 8],
    pub version: U16Le,
    pub kind: u8,
    pub flags: u8,
    pub bump: u8,
    pub reserved: [u8; 3],
}

impl AccountHeader {
    pub const fn new(kind: u8, bump: u8) -> Self {
        Self {
            magic: MARTINGALER_MAGIC,
            version: U16Le::new(MARTINGALER_VERSION),
            kind,
            flags: 0,
            bump,
            reserved: [0; 3],
        }
    }

    fn validate(&self, expected_kind: u8) -> StateResult<()> {
        if self.magic != MARTINGALER_MAGIC {
            return Err(StateError::InvalidMagic);
        }
        if self.version.get() != MARTINGALER_VERSION {
            return Err(StateError::InvalidVersion);
        }
        if self.kind != expected_kind {
            return Err(StateError::InvalidKind);
        }
        if self.flags != 0 || !all_zero(&self.reserved) {
            return Err(StateError::ReservedNonZero);
        }
        Ok(())
    }
}

const _: [(); ACCOUNT_HEADER_LEN] = [(); size_of::<AccountHeader>()];

pub trait MartingalerAccount: Copy + Pod {
    const KIND: u8;

    fn validate(&self) -> StateResult<()>;
}

pub const fn account_len<T: MartingalerAccount>() -> usize {
    ACCOUNT_HEADER_LEN + size_of::<T>()
}

pub fn initialize_account<T: MartingalerAccount>(
    data: &mut [u8],
    bump: u8,
    body: &T,
) -> StateResult<()> {
    if data.len() != account_len::<T>() {
        return Err(StateError::WrongLength);
    }
    if !all_zero(data) {
        return Err(StateError::AlreadyInitialized);
    }
    body.validate()?;
    let header = AccountHeader::new(T::KIND, bump);
    data[..ACCOUNT_HEADER_LEN].copy_from_slice(bytemuck::bytes_of(&header));
    data[ACCOUNT_HEADER_LEN..].copy_from_slice(bytemuck::bytes_of(body));
    Ok(())
}

pub fn decode_account<T: MartingalerAccount>(data: &[u8]) -> StateResult<T> {
    if data.len() != account_len::<T>() {
        return Err(StateError::WrongLength);
    }
    decode_account_header::<T>(data)?;
    let body: T = bytemuck::pod_read_unaligned(&data[ACCOUNT_HEADER_LEN..]);
    body.validate()?;
    Ok(body)
}

pub fn decode_account_boxed<T: MartingalerAccount>(data: &[u8]) -> StateResult<Box<T>> {
    if data.len() != account_len::<T>() {
        return Err(StateError::WrongLength);
    }
    decode_account_header::<T>(data)?;
    let mut body = Box::<T>::new_uninit();
    // SAFETY: `T: Pod`, the source has exactly `size_of::<T>()` bytes after the
    // validated header, and the new allocation is exclusively owned.
    unsafe {
        core::ptr::copy_nonoverlapping(
            data[ACCOUNT_HEADER_LEN..].as_ptr(),
            body.as_mut_ptr().cast::<u8>(),
            size_of::<T>(),
        );
        let body = body.assume_init();
        body.validate()?;
        Ok(body)
    }
}

pub fn account_body_matches<T: MartingalerAccount>(data: &[u8], body: &T) -> StateResult<bool> {
    decode_account_header::<T>(data)?;
    body.validate()?;
    Ok(data[ACCOUNT_HEADER_LEN..] == *bytemuck::bytes_of(body))
}

pub fn decode_account_header<T: MartingalerAccount>(data: &[u8]) -> StateResult<AccountHeader> {
    if data.len() != account_len::<T>() {
        return Err(StateError::WrongLength);
    }
    let header: AccountHeader = bytemuck::pod_read_unaligned(&data[..ACCOUNT_HEADER_LEN]);
    header.validate(T::KIND)?;
    Ok(header)
}

pub fn overwrite_account<T: MartingalerAccount>(data: &mut [u8], body: &T) -> StateResult<()> {
    if data.len() != account_len::<T>() {
        return Err(StateError::WrongLength);
    }
    let header: AccountHeader = bytemuck::pod_read_unaligned(&data[..ACCOUNT_HEADER_LEN]);
    header.validate(T::KIND)?;
    body.validate()?;
    data[ACCOUNT_HEADER_LEN..].copy_from_slice(bytemuck::bytes_of(body));
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PoolStatus {
    Active = 0,
    CloseOnly = 1,
    ClaimsOnly = 2,
    Terminal = 3,
}

impl TryFrom<u8> for PoolStatus {
    type Error = StateError;

    fn try_from(value: u8) -> StateResult<Self> {
        match value {
            0 => Ok(Self::Active),
            1 => Ok(Self::CloseOnly),
            2 => Ok(Self::ClaimsOnly),
            3 => Ok(Self::Terminal),
            _ => Err(StateError::InvalidTag),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PositionSide {
    Long = 0,
    Short = 1,
}

impl TryFrom<u8> for PositionSide {
    type Error = StateError;

    fn try_from(value: u8) -> StateResult<Self> {
        match value {
            0 => Ok(Self::Long),
            1 => Ok(Self::Short),
            _ => Err(StateError::InvalidTag),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FundingKind {
    Cash = 0,
    Claim = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OracleRelaySource {
    MagicBlockDemo = 1,
    PhoenixHawkeye = 2,
    PhoenixMainnetRelay = 3,
}

impl TryFrom<u8> for OracleRelaySource {
    type Error = StateError;

    fn try_from(value: u8) -> StateResult<Self> {
        match value {
            1 => Ok(Self::MagicBlockDemo),
            2 => Ok(Self::PhoenixHawkeye),
            3 => Ok(Self::PhoenixMainnetRelay),
            _ => Err(StateError::InvalidTag),
        }
    }
}

impl TryFrom<u8> for FundingKind {
    type Error = StateError;

    fn try_from(value: u8) -> StateResult<Self> {
        match value {
            0 => Ok(Self::Cash),
            1 => Ok(Self::Claim),
            _ => Err(StateError::InvalidTag),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PositionStatus {
    Open = 0,
    Closed = 1,
}

impl TryFrom<u8> for PositionStatus {
    type Error = StateError;

    fn try_from(value: u8) -> StateResult<Self> {
        match value {
            0 => Ok(Self::Open),
            1 => Ok(Self::Closed),
            _ => Err(StateError::InvalidTag),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ClaimStatus {
    Queued = 0,
    PartiallyPaid = 1,
    FundingPosition = 2,
    Settled = 3,
    Extinguished = 4,
    QueuedAndFunding = 5,
}

impl TryFrom<u8> for ClaimStatus {
    type Error = StateError;

    fn try_from(value: u8) -> StateResult<Self> {
        match value {
            0 => Ok(Self::Queued),
            1 => Ok(Self::PartiallyPaid),
            2 => Ok(Self::FundingPosition),
            3 => Ok(Self::Settled),
            4 => Ok(Self::Extinguished),
            5 => Ok(Self::QueuedAndFunding),
            _ => Err(StateError::InvalidTag),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct PoolState {
    pub risk_pool_id: U64Le,
    pub quote_mint: Key32,
    pub vault: Key32,
    pub admin: Key32,
    pub status: u8,
    pub max_leverage_x: U16Le,
    pub queue_flags: u8,
    pub reserved_control: [u8; 4],
    pub max_position_notional: U128Le,
    pub max_net_profit_per_position: U128Le,
    pub max_reserved_profit_per_controller: U128Le,
    pub total_liability_open_gate: U128Le,
    pub protected_principal: U128Le,
    pub free_cash: U128Le,
    pub claim_service_cash: U128Le,
    pub total_queued: U128Le,
    pub open_claim_collateral: U128Le,
    pub reserved_open_profit: U128Le,
    pub residual_bad_debt: U128Le,
    pub fee_accrual: U128Le,
    pub next_claim_sequence: U128Le,
    pub head_claim_sequence: U128Le,
    pub tail_claim_sequence: U128Le,
    pub head_page_index: U64Le,
    pub tail_page_index: U64Le,
    pub next_page_index: U64Le,
    pub idle_cash_collateral: U128Le,
    pub unallocated_vault_surplus: U128Le,
}

impl PoolState {
    pub fn new(risk_pool_id: u64, quote_mint: Key32, vault: Key32, admin: Key32) -> Self {
        Self {
            risk_pool_id: U64Le::new(risk_pool_id),
            quote_mint,
            vault,
            admin,
            status: PoolStatus::Active as u8,
            max_leverage_x: U16Le::new(V1_MAX_LEVERAGE_X),
            queue_flags: 0,
            reserved_control: [0; 4],
            max_position_notional: U128Le::new(V1_MAX_POSITION_NOTIONAL_QUOTE),
            max_net_profit_per_position: U128Le::new(V1_MAX_NET_PROFIT_PER_POSITION_QUOTE),
            max_reserved_profit_per_controller: U128Le::new(
                V1_MAX_RESERVED_PROFIT_PER_CONTROLLER_QUOTE,
            ),
            total_liability_open_gate: U128Le::new(V1_TOTAL_POTENTIAL_LIABILITY_QUOTE),
            protected_principal: U128Le::new(0),
            free_cash: U128Le::new(0),
            claim_service_cash: U128Le::new(0),
            total_queued: U128Le::new(0),
            open_claim_collateral: U128Le::new(0),
            reserved_open_profit: U128Le::new(0),
            residual_bad_debt: U128Le::new(0),
            fee_accrual: U128Le::new(0),
            next_claim_sequence: U128Le::new(1),
            head_claim_sequence: U128Le::new(NO_CLAIM_SEQUENCE),
            tail_claim_sequence: U128Le::new(NO_CLAIM_SEQUENCE),
            head_page_index: U64Le::new(0),
            tail_page_index: U64Le::new(0),
            next_page_index: U64Le::new(1),
            idle_cash_collateral: U128Le::new(0),
            unallocated_vault_surplus: U128Le::new(0),
        }
    }

    pub fn represented_quote_custody(&self) -> StateResult<u128> {
        let represented = self
            .idle_cash_collateral
            .get()
            .checked_add(self.protected_principal.get())
            .and_then(|value| value.checked_add(self.free_cash.get()))
            .and_then(|value| value.checked_add(self.claim_service_cash.get()))
            .and_then(|value| value.checked_add(self.unallocated_vault_surplus.get()))
            .ok_or(StateError::ArithmeticOverflow)?;
        if represented > u128::from(u64::MAX) {
            return Err(StateError::CustodyCapacityExceeded);
        }
        Ok(represented)
    }
}

impl MartingalerAccount for PoolState {
    const KIND: u8 = KIND_POOL;

    fn validate(&self) -> StateResult<()> {
        PoolStatus::try_from(self.status)?;
        if self.risk_pool_id.get() == 0
            || self.quote_mint.is_zero()
            || self.vault.is_zero()
            || self.admin.is_zero()
            || self.max_leverage_x.get() == 0
            || self.max_leverage_x.get() > V1_MAX_LEVERAGE_X
            || self.max_position_notional.get() == 0
            || self.max_position_notional.get() > V1_MAX_POSITION_NOTIONAL_QUOTE
            || self.max_net_profit_per_position.get() == 0
            || self.max_net_profit_per_position.get() > V1_MAX_NET_PROFIT_PER_POSITION_QUOTE
            || self.max_net_profit_per_position.get() > self.max_position_notional.get()
            || self.max_reserved_profit_per_controller.get() == 0
            || self.max_reserved_profit_per_controller.get()
                > V1_MAX_RESERVED_PROFIT_PER_CONTROLLER_QUOTE
            || self.total_liability_open_gate.get() == 0
            || self.total_liability_open_gate.get() > V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
            || self.next_claim_sequence.get() == 0
            || self.next_claim_sequence.get() == NO_CLAIM_SEQUENCE
            || self.next_page_index.get() == 0
            || self.head_page_index.get() >= self.next_page_index.get()
            || self.tail_page_index.get() >= self.next_page_index.get()
            || self.queue_flags & !QUEUE_FLAG_HEAD_SETTLED != 0
            || !all_zero(&self.reserved_control)
        {
            return Err(StateError::InvalidState);
        }
        self.represented_quote_custody()?;

        let head = self.head_claim_sequence.get();
        let tail = self.tail_claim_sequence.get();
        let settled_head = self.queue_flags & QUEUE_FLAG_HEAD_SETTLED != 0;
        if self.total_queued.get() == 0 && !settled_head {
            if head != NO_CLAIM_SEQUENCE
                || tail != NO_CLAIM_SEQUENCE
                || self.claim_service_cash.get() != 0
            {
                return Err(StateError::InvalidState);
            }
        } else if head == 0
            || head == NO_CLAIM_SEQUENCE
            || tail == NO_CLAIM_SEQUENCE
            || head > tail
            || tail >= self.next_claim_sequence.get()
        {
            return Err(StateError::InvalidState);
        }
        if (self.total_queued.get() != 0 || settled_head) && self.free_cash.get() != 0 {
            return Err(StateError::InvalidState);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct HaircutConfig {
    pub base_rate_ppm: U32Le,
    pub deadband_move_ppm: U32Le,
    pub linear_coefficient_ppm: U32Le,
    pub quadratic_coefficient_ppm: U32Le,
    pub max_rate_ppm: U32Le,
    pub win_fee_ppm: U32Le,
}

impl HaircutConfig {
    pub const RATE_SCALE_PPM: u128 = 1_000_000;

    pub const fn new(
        base_rate_ppm: u32,
        deadband_move_ppm: u32,
        linear_coefficient_ppm: u32,
        quadratic_coefficient_ppm: u32,
        max_rate_ppm: u32,
        win_fee_ppm: u32,
    ) -> Self {
        Self {
            base_rate_ppm: U32Le::new(base_rate_ppm),
            deadband_move_ppm: U32Le::new(deadband_move_ppm),
            linear_coefficient_ppm: U32Le::new(linear_coefficient_ppm),
            quadratic_coefficient_ppm: U32Le::new(quadratic_coefficient_ppm),
            max_rate_ppm: U32Le::new(max_rate_ppm),
            win_fee_ppm: U32Le::new(win_fee_ppm),
        }
    }

    pub fn validate(&self) -> StateResult<()> {
        if self.base_rate_ppm.get() > self.max_rate_ppm.get()
            || u128::from(self.max_rate_ppm.get()) >= Self::RATE_SCALE_PPM
            || u128::from(self.win_fee_ppm.get()) > Self::RATE_SCALE_PPM
        {
            return Err(StateError::InvalidState);
        }
        Ok(())
    }
}

const _: [(); 24] = [(); size_of::<HaircutConfig>()];

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct MarketRiskState {
    pub pool: Key32,
    pub market_id: U16Le,
    pub status: u8,
    pub reserved_control: [u8; 1],
    pub max_leverage_x: U16Le,
    pub divergence_limit_bps: U16Le,
    pub max_position_notional: U128Le,
    pub max_net_profit_per_position: U128Le,
    pub max_long_open_interest: U128Le,
    pub max_short_open_interest: U128Le,
    pub long_open_interest: U128Le,
    pub short_open_interest: U128Le,
    pub reserved_profit: U128Le,
    pub haircut: HaircutConfig,
    pub reserved: [u8; 8],
}

impl MarketRiskState {
    pub fn new(
        pool: Key32,
        pool_state: &PoolState,
        market_id: u16,
        divergence_limit_bps: u16,
        max_long_open_interest: u128,
        max_short_open_interest: u128,
        haircut: HaircutConfig,
    ) -> StateResult<Self> {
        pool_state.validate()?;
        if max_long_open_interest > pool_state.total_liability_open_gate.get()
            || max_short_open_interest > pool_state.total_liability_open_gate.get()
        {
            return Err(StateError::InvalidState);
        }
        let state = Self {
            pool,
            market_id: U16Le::new(market_id),
            status: PoolStatus::Active as u8,
            reserved_control: [0; 1],
            max_leverage_x: pool_state.max_leverage_x,
            divergence_limit_bps: U16Le::new(divergence_limit_bps),
            max_position_notional: pool_state.max_position_notional,
            max_net_profit_per_position: pool_state.max_net_profit_per_position,
            max_long_open_interest: U128Le::new(max_long_open_interest),
            max_short_open_interest: U128Le::new(max_short_open_interest),
            long_open_interest: U128Le::new(0),
            short_open_interest: U128Le::new(0),
            reserved_profit: U128Le::new(0),
            haircut,
            reserved: [0; 8],
        };
        state.validate()?;
        Ok(state)
    }
}

impl MartingalerAccount for MarketRiskState {
    const KIND: u8 = KIND_MARKET_RISK;

    fn validate(&self) -> StateResult<()> {
        PoolStatus::try_from(self.status)?;
        self.haircut.validate()?;
        if self.pool.is_zero()
            || self.market_id.get() == 0
            || self.max_leverage_x.get() == 0
            || self.max_leverage_x.get() > V1_MAX_LEVERAGE_X
            || self.divergence_limit_bps.get() == 0
            || self.divergence_limit_bps.get() > 10_000
            || self.max_position_notional.get() == 0
            || self.max_position_notional.get() > V1_MAX_POSITION_NOTIONAL_QUOTE
            || self.max_net_profit_per_position.get() == 0
            || self.max_net_profit_per_position.get() > V1_MAX_NET_PROFIT_PER_POSITION_QUOTE
            || self.max_net_profit_per_position.get() > self.max_position_notional.get()
            || self.max_long_open_interest.get() == 0
            || self.max_long_open_interest.get() > V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
            || self.max_short_open_interest.get() == 0
            || self.max_short_open_interest.get() > V1_TOTAL_POTENTIAL_LIABILITY_QUOTE
            || self.long_open_interest.get() > self.max_long_open_interest.get()
            || self.short_open_interest.get() > self.max_short_open_interest.get()
            || !all_zero(&self.reserved_control)
            || !all_zero(&self.reserved)
        {
            return Err(StateError::InvalidState);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct OraclePolicyState {
    pub pool: Key32,
    pub market: Key32,
    pub primary_feed_id: Key32,
    pub secondary_feed_id: Key32,
    pub max_staleness_secs: U64Le,
    pub confidence_limit_bps: U16Le,
    pub oracle_count: u8,
    pub reserved_control: [u8; 5],
}

impl OraclePolicyState {
    pub fn new(
        pool: Key32,
        market: Key32,
        primary_feed_id: Key32,
        secondary_feed_id: Key32,
        max_staleness_secs: u64,
        confidence_limit_bps: u16,
    ) -> StateResult<Self> {
        let oracle_count = if secondary_feed_id.is_zero() { 1 } else { 2 };
        let state = Self {
            pool,
            market,
            primary_feed_id,
            secondary_feed_id,
            max_staleness_secs: U64Le::new(max_staleness_secs),
            confidence_limit_bps: U16Le::new(confidence_limit_bps),
            oracle_count,
            reserved_control: [0; 5],
        };
        state.validate()?;
        Ok(state)
    }
}

impl MartingalerAccount for OraclePolicyState {
    const KIND: u8 = KIND_ORACLE_POLICY;

    fn validate(&self) -> StateResult<()> {
        let secondary_configured = !self.secondary_feed_id.is_zero();
        if self.pool.is_zero()
            || self.market.is_zero()
            || self.pool == self.market
            || self.primary_feed_id.is_zero()
            || (secondary_configured && self.secondary_feed_id == self.primary_feed_id)
            || self.oracle_count != if secondary_configured { 2 } else { 1 }
            || self.max_staleness_secs.get() == 0
            || self.max_staleness_secs.get() > MAX_ORACLE_STALENESS_SECS
            || self.confidence_limit_bps.get() == 0
            || self.confidence_limit_bps.get() > MAX_ORACLE_CONFIDENCE_BPS
            || !all_zero(&self.reserved_control)
        {
            return Err(StateError::InvalidState);
        }
        Ok(())
    }
}

/// Program-owned L1 observation with an immutable source-specific trust path.
///
/// MagicBlock and devnet Phoenix-mainnet-mirror observations are written by
/// the configured singleton keeper; native Phoenix observations are refreshed
/// permissionlessly through a pinned Hawkeye CPI. The source tag and immutable
/// configuration keep those trust paths distinct from each other and from
/// direct Pyth/Switchboard accounts.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct OracleRelayState {
    pub pool: Key32,
    pub market: Key32,
    pub feed_id: Key32,
    pub authority: Key32,
    pub price_e6: U64Le,
    pub confidence_e6: U64Le,
    pub publish_time: I64Le,
    pub source_posted_slot: U64Le,
    pub update_slot: U64Le,
    pub sequence: U64Le,
    /// Exact E6 quote-price value represented by one Phoenix market tick.
    /// Zero for relay sources that already submit an E6 price.
    pub price_e6_per_tick: U64Le,
    /// Maximum age of both Phoenix mark and index updates at refresh time.
    /// Zero for non-Phoenix relay sources.
    pub max_source_staleness_slots: U64Le,
    /// Maximum Phoenix best-bid/best-ask spread, measured around the midpoint.
    /// Zero for non-Phoenix relay sources.
    pub max_spread_bps: U16Le,
    /// Maximum divergence between Phoenix's finalized mark and index price.
    /// Zero for non-Phoenix relay sources.
    pub max_mark_index_divergence_bps: U16Le,
    pub source: u8,
    pub reserved_control: [u8; 3],
}

impl OracleRelayState {
    pub fn new(
        pool: Key32,
        market: Key32,
        feed_id: Key32,
        authority: Key32,
        source: OracleRelaySource,
        price_e6_per_tick: u64,
        max_source_staleness_slots: u64,
        max_spread_bps: u16,
        max_mark_index_divergence_bps: u16,
    ) -> StateResult<Self> {
        let state = Self {
            pool,
            market,
            feed_id,
            authority,
            price_e6: U64Le::new(0),
            confidence_e6: U64Le::new(0),
            publish_time: I64Le::new(0),
            source_posted_slot: U64Le::new(0),
            update_slot: U64Le::new(0),
            sequence: U64Le::new(0),
            price_e6_per_tick: U64Le::new(price_e6_per_tick),
            max_source_staleness_slots: U64Le::new(max_source_staleness_slots),
            max_spread_bps: U16Le::new(max_spread_bps),
            max_mark_index_divergence_bps: U16Le::new(max_mark_index_divergence_bps),
            source: source as u8,
            reserved_control: [0; 3],
        };
        state.validate()?;
        Ok(state)
    }

    pub fn record_observation(
        &mut self,
        price_e6: u64,
        confidence_e6: u64,
        publish_time: i64,
        source_posted_slot: u64,
        update_slot: u64,
    ) -> StateResult<()> {
        if price_e6 == 0
            || confidence_e6 > price_e6
            || publish_time <= 0
            || source_posted_slot == 0
            || update_slot == 0
        {
            return Err(StateError::InvalidState);
        }
        let sequence = self.sequence.get();
        if sequence != 0
            && (publish_time < self.publish_time.get()
                || source_posted_slot <= self.source_posted_slot.get())
        {
            return Err(StateError::InvalidState);
        }
        self.price_e6 = U64Le::new(price_e6);
        self.confidence_e6 = U64Le::new(confidence_e6);
        self.publish_time = I64Le::new(publish_time);
        self.source_posted_slot = U64Le::new(source_posted_slot);
        self.update_slot = U64Le::new(update_slot);
        self.sequence = U64Le::new(
            sequence
                .checked_add(1)
                .ok_or(StateError::ArithmeticOverflow)?,
        );
        self.validate()
    }
}

impl MartingalerAccount for OracleRelayState {
    const KIND: u8 = KIND_ORACLE_RELAY;

    fn validate(&self) -> StateResult<()> {
        let source = OracleRelaySource::try_from(self.source)?;
        let source_config_valid = match source {
            OracleRelaySource::MagicBlockDemo | OracleRelaySource::PhoenixMainnetRelay => {
                self.price_e6_per_tick.get() == 0
                    && self.max_source_staleness_slots.get() == 0
                    && self.max_spread_bps.get() == 0
                    && self.max_mark_index_divergence_bps.get() == 0
            }
            OracleRelaySource::PhoenixHawkeye => {
                self.price_e6_per_tick.get() > 0
                    && self.max_source_staleness_slots.get() > 0
                    && self.max_source_staleness_slots.get() <= MAX_PHOENIX_SOURCE_STALENESS_SLOTS
                    && self.max_spread_bps.get() > 0
                    && self.max_spread_bps.get() <= 10_000
                    && self.max_mark_index_divergence_bps.get() > 0
                    && self.max_mark_index_divergence_bps.get() <= 10_000
            }
        };
        let empty_observation = self.sequence.get() == 0;
        let observation_is_zero = self.price_e6.get() == 0
            && self.confidence_e6.get() == 0
            && self.publish_time.get() == 0
            && self.source_posted_slot.get() == 0
            && self.update_slot.get() == 0;
        if self.pool.is_zero()
            || self.market.is_zero()
            || self.feed_id.is_zero()
            || self.authority.is_zero()
            || self.pool == self.market
            || !source_config_valid
            || !all_zero(&self.reserved_control)
            || (empty_observation != observation_is_zero)
            || (!empty_observation
                && (self.price_e6.get() == 0
                    || self.confidence_e6.get() > self.price_e6.get()
                    || self.publish_time.get() <= 0
                    || self.source_posted_slot.get() == 0
                    || self.update_slot.get() == 0))
        {
            return Err(StateError::InvalidState);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct ControllerRiskState {
    pub pool: Key32,
    pub controller: Key32,
    pub available_cash_collateral: U128Le,
    pub reserved_profit: U128Le,
    pub queued_claim_face: U128Le,
    pub claim_collateral_in_use: U128Le,
    pub open_positions: U32Le,
    pub active_claims: U32Le,
    pub reserved: [u8; 16],
}

impl ControllerRiskState {
    pub fn new(pool: Key32, controller: Key32) -> StateResult<Self> {
        let state = Self {
            pool,
            controller,
            available_cash_collateral: U128Le::new(0),
            reserved_profit: U128Le::new(0),
            queued_claim_face: U128Le::new(0),
            claim_collateral_in_use: U128Le::new(0),
            open_positions: U32Le::new(0),
            active_claims: U32Le::new(0),
            reserved: [0; 16],
        };
        state.validate()?;
        Ok(state)
    }
}

impl MartingalerAccount for ControllerRiskState {
    const KIND: u8 = KIND_CONTROLLER_RISK;

    fn validate(&self) -> StateResult<()> {
        if self.pool.is_zero()
            || self.controller.is_zero()
            || self.available_cash_collateral.get() > u128::from(u64::MAX)
            || self.reserved_profit.get() > V1_MAX_RESERVED_PROFIT_PER_CONTROLLER_QUOTE
            || !all_zero(&self.reserved)
        {
            return Err(StateError::InvalidState);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct TradingSessionState {
    pub pool: Key32,
    pub owner: Key32,
    pub session_key: Key32,
    pub epoch: U64Le,
    pub created_at: I64Le,
    pub expires_at: I64Le,
    pub next_nonce: U64Le,
    pub spent_notional: U128Le,
    pub revoked: u8,
    pub reserved: [u8; 15],
}

impl TradingSessionState {
    pub fn new(pool: Key32, owner: Key32, session_key: Key32, now: i64) -> StateResult<Self> {
        Self::new_epoch(pool, owner, session_key, 1, now)
    }

    fn new_epoch(
        pool: Key32,
        owner: Key32,
        session_key: Key32,
        epoch: u64,
        now: i64,
    ) -> StateResult<Self> {
        let state = Self {
            pool,
            owner,
            session_key,
            epoch: U64Le::new(epoch),
            created_at: I64Le::new(now),
            expires_at: I64Le::new(
                now.checked_add(TRADING_SESSION_DURATION_SECS)
                    .ok_or(StateError::ArithmeticOverflow)?,
            ),
            next_nonce: U64Le::new(0),
            spent_notional: U128Le::new(0),
            revoked: 0,
            reserved: [0; 15],
        };
        state.validate()?;
        Ok(state)
    }

    pub fn renew(
        &self,
        owner: Key32,
        session_key: Key32,
        expected_epoch: u64,
        now: i64,
    ) -> StateResult<Self> {
        self.validate()?;
        if owner != self.owner || expected_epoch != self.epoch.get() {
            return Err(StateError::InvalidState);
        }
        Self::new_epoch(
            self.pool,
            self.owner,
            session_key,
            expected_epoch
                .checked_add(1)
                .ok_or(StateError::ArithmeticOverflow)?,
            now,
        )
    }

    pub fn revoke(&self, owner: Key32, expected_epoch: u64) -> StateResult<Self> {
        self.validate()?;
        if owner != self.owner || expected_epoch != self.epoch.get() {
            return Err(StateError::InvalidState);
        }
        let mut next = *self;
        next.revoked = 1;
        next.validate()?;
        Ok(next)
    }

    pub fn authorize_action(
        &self,
        pool: Key32,
        owner: Key32,
        signer: Key32,
        epoch: u64,
        nonce: u64,
        now: i64,
        opened_notional: u128,
    ) -> StateResult<Self> {
        self.validate()?;
        if pool != self.pool
            || owner != self.owner
            || signer != self.session_key
            || epoch != self.epoch.get()
            || nonce != self.next_nonce.get()
            || self.revoked != 0
            || now < self.created_at.get()
            || now >= self.expires_at.get()
            || opened_notional > V1_MAX_POSITION_NOTIONAL_QUOTE
        {
            return Err(StateError::InvalidState);
        }
        let next_spent = self
            .spent_notional
            .get()
            .checked_add(opened_notional)
            .ok_or(StateError::ArithmeticOverflow)?;
        if next_spent > TRADING_SESSION_TOTAL_NOTIONAL_CAP {
            return Err(StateError::RiskLimitExceeded);
        }
        let mut next = *self;
        next.next_nonce = U64Le::new(nonce.checked_add(1).ok_or(StateError::ArithmeticOverflow)?);
        next.spent_notional = U128Le::new(next_spent);
        next.validate()?;
        Ok(next)
    }
}

impl MartingalerAccount for TradingSessionState {
    const KIND: u8 = KIND_TRADING_SESSION;

    fn validate(&self) -> StateResult<()> {
        let expected_expiry = self
            .created_at
            .get()
            .checked_add(TRADING_SESSION_DURATION_SECS)
            .ok_or(StateError::ArithmeticOverflow)?;
        if self.pool.is_zero()
            || self.owner.is_zero()
            || self.session_key.is_zero()
            || self.owner == self.session_key
            || self.epoch.get() == 0
            || self.created_at.get() < 0
            || self.expires_at.get() != expected_expiry
            || self.next_nonce.get() > TRADING_SESSION_MAX_ACTIONS
            || self.spent_notional.get() > TRADING_SESSION_TOTAL_NOTIONAL_CAP
            || self.revoked > 1
            || !all_zero(&self.reserved)
        {
            return Err(StateError::InvalidState);
        }
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct PositionState {
    pub pool: Key32,
    pub controller: Key32,
    pub position_id: U64Le,
    pub market_id: U16Le,
    pub side: u8,
    pub funding_kind: u8,
    pub status: u8,
    pub reserved_control: [u8; 1],
    pub leverage_x: U16Le,
    pub source_claim_sequence: U128Le,
    pub collateral: U128Le,
    pub notional: U128Le,
    pub max_payable_profit: U128Le,
    pub reserved_profit: U128Le,
    pub entry_price_e6: U64Le,
    pub entry_publish_time: I64Le,
    pub secondary_entry_price_e6: U64Le,
    pub secondary_publish_time: I64Le,
}

impl PositionState {
    #[allow(clippy::too_many_arguments)]
    pub fn new_cash(
        pool: Key32,
        controller: Key32,
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        leverage_x: u16,
        collateral: u128,
        notional: u128,
        max_payable_profit: u128,
        entry_price_e6: u64,
        entry_publish_time: i64,
        secondary_entry_price_e6: u64,
        secondary_publish_time: i64,
    ) -> StateResult<Self> {
        let state = Self {
            pool,
            controller,
            position_id: U64Le::new(position_id),
            market_id: U16Le::new(market_id),
            side: side as u8,
            funding_kind: FundingKind::Cash as u8,
            status: PositionStatus::Open as u8,
            reserved_control: [0; 1],
            leverage_x: U16Le::new(leverage_x),
            source_claim_sequence: U128Le::new(NO_CLAIM_SEQUENCE),
            collateral: U128Le::new(collateral),
            notional: U128Le::new(notional),
            max_payable_profit: U128Le::new(max_payable_profit),
            reserved_profit: U128Le::new(max_payable_profit),
            entry_price_e6: U64Le::new(entry_price_e6),
            entry_publish_time: I64Le::new(entry_publish_time),
            secondary_entry_price_e6: U64Le::new(secondary_entry_price_e6),
            secondary_publish_time: I64Le::new(secondary_publish_time),
        };
        state.validate()?;
        Ok(state)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_claim(
        pool: Key32,
        controller: Key32,
        position_id: u64,
        market_id: u16,
        side: PositionSide,
        leverage_x: u16,
        source_claim_sequence: u128,
        collateral: u128,
        notional: u128,
        max_payable_profit: u128,
        entry_price_e6: u64,
        entry_publish_time: i64,
        secondary_entry_price_e6: u64,
        secondary_publish_time: i64,
    ) -> StateResult<Self> {
        let state = Self {
            pool,
            controller,
            position_id: U64Le::new(position_id),
            market_id: U16Le::new(market_id),
            side: side as u8,
            funding_kind: FundingKind::Claim as u8,
            status: PositionStatus::Open as u8,
            reserved_control: [0; 1],
            leverage_x: U16Le::new(leverage_x),
            source_claim_sequence: U128Le::new(source_claim_sequence),
            collateral: U128Le::new(collateral),
            notional: U128Le::new(notional),
            max_payable_profit: U128Le::new(max_payable_profit),
            reserved_profit: U128Le::new(max_payable_profit),
            entry_price_e6: U64Le::new(entry_price_e6),
            entry_publish_time: I64Le::new(entry_publish_time),
            secondary_entry_price_e6: U64Le::new(secondary_entry_price_e6),
            secondary_publish_time: I64Le::new(secondary_publish_time),
        };
        state.validate()?;
        Ok(state)
    }
}

impl MartingalerAccount for PositionState {
    const KIND: u8 = KIND_POSITION;

    fn validate(&self) -> StateResult<()> {
        PositionSide::try_from(self.side)?;
        let funding = FundingKind::try_from(self.funding_kind)?;
        let status = PositionStatus::try_from(self.status)?;
        if self.pool.is_zero()
            || self.controller.is_zero()
            || self.position_id.get() == 0
            || self.market_id.get() == 0
            || self.leverage_x.get() == 0
            || self.leverage_x.get() > V1_MAX_LEVERAGE_X
            || self.collateral.get() == 0
            || self.notional.get() == 0
            || self.notional.get() > V1_MAX_POSITION_NOTIONAL_QUOTE
            || self.max_payable_profit.get() == 0
            || self.max_payable_profit.get() > self.notional.get()
            || self.max_payable_profit.get() > V1_MAX_NET_PROFIT_PER_POSITION_QUOTE
            || match status {
                PositionStatus::Open => self.reserved_profit.get() != self.max_payable_profit.get(),
                PositionStatus::Closed => self.reserved_profit.get() != 0,
            }
            || self.entry_price_e6.get() == 0
            || self.entry_publish_time.get() <= 0
            || (self.secondary_entry_price_e6.get() == 0)
                != (self.secondary_publish_time.get() == 0)
            || self.secondary_publish_time.get() < 0
            || !all_zero(&self.reserved_control)
        {
            return Err(StateError::InvalidState);
        }
        let exact_notional = self
            .collateral
            .get()
            .checked_mul(u128::from(self.leverage_x.get()))
            .ok_or(StateError::ArithmeticOverflow)?;
        if self.notional.get() != exact_notional {
            return Err(StateError::InvalidState);
        }
        match funding {
            FundingKind::Cash if self.source_claim_sequence.get() != NO_CLAIM_SEQUENCE => {
                Err(StateError::InvalidState)
            }
            FundingKind::Claim
                if self.source_claim_sequence.get() == 0
                    || self.source_claim_sequence.get() == NO_CLAIM_SEQUENCE =>
            {
                Err(StateError::InvalidState)
            }
            _ => Ok(()),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct ClaimState {
    pub pool: Key32,
    pub controller: Key32,
    pub origin_position_id: U64Le,
    pub origin_market_id: U16Le,
    pub status: u8,
    pub funding_kind: u8,
    pub queue_slot: U16Le,
    pub reserved_control: [u8; 2],
    pub queue_page_index: U64Le,
    pub sequence: U128Le,
    pub source_claim_sequence: U128Le,
    pub original_face: U128Le,
    pub remaining_face: U128Le,
    pub collateral_in_use: U128Le,
    pub reserved: [u8; 32],
}

impl ClaimState {
    #[allow(clippy::too_many_arguments)]
    pub fn new_cash(
        pool: Key32,
        controller: Key32,
        origin_position_id: u64,
        origin_market_id: u16,
        queue_page_index: u64,
        queue_slot: u16,
        sequence: u128,
        face: u128,
    ) -> StateResult<Self> {
        let state = Self {
            pool,
            controller,
            origin_position_id: U64Le::new(origin_position_id),
            origin_market_id: U16Le::new(origin_market_id),
            status: ClaimStatus::Queued as u8,
            funding_kind: FundingKind::Cash as u8,
            queue_slot: U16Le::new(queue_slot),
            reserved_control: [0; 2],
            queue_page_index: U64Le::new(queue_page_index),
            sequence: U128Le::new(sequence),
            source_claim_sequence: U128Le::new(NO_CLAIM_SEQUENCE),
            original_face: U128Le::new(face),
            remaining_face: U128Le::new(face),
            collateral_in_use: U128Le::new(0),
            reserved: [0; 32],
        };
        state.validate()?;
        Ok(state)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_from_claim(
        pool: Key32,
        controller: Key32,
        origin_position_id: u64,
        origin_market_id: u16,
        queue_page_index: u64,
        queue_slot: u16,
        sequence: u128,
        source_claim_sequence: u128,
        face: u128,
    ) -> StateResult<Self> {
        let state = Self {
            pool,
            controller,
            origin_position_id: U64Le::new(origin_position_id),
            origin_market_id: U16Le::new(origin_market_id),
            status: ClaimStatus::Queued as u8,
            funding_kind: FundingKind::Claim as u8,
            queue_slot: U16Le::new(queue_slot),
            reserved_control: [0; 2],
            queue_page_index: U64Le::new(queue_page_index),
            sequence: U128Le::new(sequence),
            source_claim_sequence: U128Le::new(source_claim_sequence),
            original_face: U128Le::new(face),
            remaining_face: U128Le::new(face),
            collateral_in_use: U128Le::new(0),
            reserved: [0; 32],
        };
        state.validate()?;
        Ok(state)
    }
}

impl MartingalerAccount for ClaimState {
    const KIND: u8 = KIND_CLAIM;

    fn validate(&self) -> StateResult<()> {
        let status = ClaimStatus::try_from(self.status)?;
        let funding = FundingKind::try_from(self.funding_kind)?;
        if self.pool.is_zero()
            || self.controller.is_zero()
            || self.origin_position_id.get() == 0
            || self.origin_market_id.get() == 0
            || self.sequence.get() == 0
            || self.sequence.get() == NO_CLAIM_SEQUENCE
            || self.original_face.get() == 0
            || self.remaining_face.get() > self.original_face.get()
            || self.queue_page_index.get() == NO_PAGE_INDEX
            || usize::from(self.queue_slot.get()) >= QUEUE_PAGE_CAPACITY
            || !all_zero(&self.reserved_control)
            || !all_zero(&self.reserved)
        {
            return Err(StateError::InvalidState);
        }
        let represented_face = self
            .remaining_face
            .get()
            .checked_add(self.collateral_in_use.get())
            .ok_or(StateError::ArithmeticOverflow)?;
        if represented_face > self.original_face.get() {
            return Err(StateError::InvalidState);
        }
        match status {
            ClaimStatus::Queued | ClaimStatus::PartiallyPaid
                if self.remaining_face.get() == 0 || self.collateral_in_use.get() != 0 =>
            {
                return Err(StateError::InvalidState);
            }
            ClaimStatus::FundingPosition
                if self.remaining_face.get() != 0 || self.collateral_in_use.get() == 0 =>
            {
                return Err(StateError::InvalidState);
            }
            ClaimStatus::QueuedAndFunding
                if self.remaining_face.get() == 0 || self.collateral_in_use.get() == 0 =>
            {
                return Err(StateError::InvalidState);
            }
            ClaimStatus::Settled | ClaimStatus::Extinguished if represented_face != 0 => {
                return Err(StateError::InvalidState);
            }
            ClaimStatus::Queued
            | ClaimStatus::PartiallyPaid
            | ClaimStatus::FundingPosition
            | ClaimStatus::QueuedAndFunding
            | ClaimStatus::Settled
            | ClaimStatus::Extinguished => {}
        }
        match funding {
            FundingKind::Cash if self.source_claim_sequence.get() != NO_CLAIM_SEQUENCE => {
                Err(StateError::InvalidState)
            }
            FundingKind::Claim
                if self.source_claim_sequence.get() == 0
                    || self.source_claim_sequence.get() == NO_CLAIM_SEQUENCE
                    || self.source_claim_sequence.get() >= self.sequence.get() =>
            {
                Err(StateError::InvalidState)
            }
            _ => Ok(()),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Pod, Zeroable)]
pub struct QueueEntry {
    pub claim: Key32,
    pub sequence: U128Le,
    pub remaining_face: U128Le,
}

impl QueueEntry {
    pub fn is_zero(self) -> bool {
        self == Self::default()
    }
}

const _: [(); 64] = [(); size_of::<QueueEntry>()];

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Pod, Zeroable)]
pub struct QueuePageState {
    pub pool: Key32,
    pub page_index: U64Le,
    pub previous_page_index: U64Le,
    pub next_page_index: U64Le,
    pub first_sequence: U128Le,
    pub next_append_sequence: U128Le,
    pub head_offset: U16Le,
    pub len: U16Le,
    pub reserved_control: [u8; 4],
    pub entries: [QueueEntry; QUEUE_PAGE_CAPACITY],
}

impl Default for QueuePageState {
    fn default() -> Self {
        Self::zeroed()
    }
}

impl QueuePageState {
    #[cfg(not(target_os = "solana"))]
    pub fn new(
        pool: Key32,
        page_index: u64,
        previous_page_index: u64,
        first_sequence: u128,
    ) -> StateResult<Self> {
        let page = Self {
            pool,
            page_index: U64Le::new(page_index),
            previous_page_index: U64Le::new(previous_page_index),
            next_page_index: U64Le::new(NO_PAGE_INDEX),
            first_sequence: U128Le::new(first_sequence),
            next_append_sequence: U128Le::new(first_sequence),
            head_offset: U16Le::new(0),
            len: U16Le::new(0),
            reserved_control: [0; 4],
            entries: [QueueEntry::default(); QUEUE_PAGE_CAPACITY],
        };
        page.validate()?;
        Ok(page)
    }

    pub fn new_boxed(
        pool: Key32,
        page_index: u64,
        previous_page_index: u64,
        first_sequence: u128,
    ) -> StateResult<Box<Self>> {
        let mut page = Box::<Self>::new_uninit();
        // SAFETY: `QueuePageState: Pod`, so all-zero is a valid initialized bit
        // pattern. Required nonzero/sentinel fields are assigned before validation.
        let mut page = unsafe {
            core::ptr::write_bytes(page.as_mut_ptr().cast::<u8>(), 0, size_of::<Self>());
            page.assume_init()
        };
        page.pool = pool;
        page.page_index = U64Le::new(page_index);
        page.previous_page_index = U64Le::new(previous_page_index);
        page.next_page_index = U64Le::new(NO_PAGE_INDEX);
        page.first_sequence = U128Le::new(first_sequence);
        page.next_append_sequence = U128Le::new(first_sequence);
        page.validate()?;
        Ok(page)
    }

    pub fn active_len(&self) -> usize {
        usize::from(self.len.get().saturating_sub(self.head_offset.get()))
    }

    pub fn is_full(&self) -> bool {
        usize::from(self.len.get()) == QUEUE_PAGE_CAPACITY
    }
}

pub fn initialize_queue_page_zero_account(
    data: &mut [u8],
    bump: u8,
    pool: Key32,
) -> StateResult<()> {
    if data.len() != account_len::<QueuePageState>() {
        return Err(StateError::WrongLength);
    }
    if !all_zero(data) {
        return Err(StateError::AlreadyInitialized);
    }
    if pool.is_zero() {
        return Err(StateError::InvalidState);
    }

    let header = AccountHeader::new(KIND_QUEUE_PAGE, bump);
    data[..ACCOUNT_HEADER_LEN].copy_from_slice(bytemuck::bytes_of(&header));
    let body = &mut data[ACCOUNT_HEADER_LEN..];
    body[..32].copy_from_slice(&pool.0);
    body[40..48].copy_from_slice(&NO_PAGE_INDEX.to_le_bytes());
    body[48..56].copy_from_slice(&NO_PAGE_INDEX.to_le_bytes());
    body[56..72].copy_from_slice(&1u128.to_le_bytes());
    body[72..88].copy_from_slice(&1u128.to_le_bytes());
    validate_queue_page_zero_account(data, bump, pool)
}

pub fn validate_queue_page_zero_account(
    data: &[u8],
    expected_bump: u8,
    expected_pool: Key32,
) -> StateResult<()> {
    let header = decode_account_header::<QueuePageState>(data)?;
    if header.bump != expected_bump {
        return Err(StateError::InvalidState);
    }
    let body = &data[ACCOUNT_HEADER_LEN..];
    if body[..32] != expected_pool.0
        || body[32..40] != 0u64.to_le_bytes()
        || body[40..48] != NO_PAGE_INDEX.to_le_bytes()
        || body[48..56] != NO_PAGE_INDEX.to_le_bytes()
        || body[56..72] != 1u128.to_le_bytes()
        || body[72..88] != 1u128.to_le_bytes()
        || !all_zero(&body[88..])
    {
        return Err(StateError::InvalidState);
    }
    Ok(())
}

impl MartingalerAccount for QueuePageState {
    const KIND: u8 = KIND_QUEUE_PAGE;

    fn validate(&self) -> StateResult<()> {
        let page_index = self.page_index.get();
        let first = self.first_sequence.get();
        let head = usize::from(self.head_offset.get());
        let len = usize::from(self.len.get());
        if self.pool.is_zero()
            || page_index == NO_PAGE_INDEX
            || first == 0
            || first == NO_CLAIM_SEQUENCE
            || head > len
            || len > QUEUE_PAGE_CAPACITY
            || !all_zero(&self.reserved_control)
        {
            return Err(StateError::InvalidState);
        }
        if page_index == 0 {
            if self.previous_page_index.get() != NO_PAGE_INDEX {
                return Err(StateError::InvalidState);
            }
        } else if self.previous_page_index.get() == NO_PAGE_INDEX
            || self.previous_page_index.get() >= page_index
        {
            return Err(StateError::InvalidState);
        }
        if self.next_page_index.get() != NO_PAGE_INDEX && self.next_page_index.get() <= page_index {
            return Err(StateError::InvalidState);
        }

        let expected_next = first
            .checked_add(len as u128)
            .ok_or(StateError::ArithmeticOverflow)?;
        if expected_next != self.next_append_sequence.get() || expected_next == NO_CLAIM_SEQUENCE {
            return Err(StateError::SequenceMismatch);
        }

        for (index, entry) in self.entries.iter().enumerate() {
            if index < head || index >= len {
                if !entry.is_zero() {
                    return Err(StateError::InvalidState);
                }
                continue;
            }
            let expected_sequence = first
                .checked_add(index as u128)
                .ok_or(StateError::ArithmeticOverflow)?;
            if entry.claim.is_zero() || entry.sequence.get() != expected_sequence {
                return Err(StateError::SequenceMismatch);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppendOutcome {
    pub sequence: u128,
    pub slot: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaymentOutcome {
    pub sequence: u128,
    pub paid: u128,
    pub remaining_face: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimCollateralOutcome {
    pub sequence: u128,
    pub consumed: u128,
    pub remaining_at_priority: u128,
    pub collateral_in_use: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CashDepositOutcome {
    pub amount: u64,
    pub surplus_recorded: u64,
    pub represented_custody_after: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IdleCashWithdrawalOutcome {
    pub amount: u64,
    pub surplus_recorded: u64,
    pub represented_custody_after: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayoutQuote {
    pub max_payable_profit: u128,
    pub payout_factor_bps: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CashOpenOutcome {
    pub position: PositionState,
    pub reserved_profit: u128,
    pub surplus_recorded: u64,
    pub represented_custody_after: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimOpenOutcome {
    pub position: PositionState,
    pub source: ClaimCollateralOutcome,
    pub reserved_profit: u128,
    pub surplus_recorded: u64,
    pub represented_custody_after: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OracleCashPnl {
    Profit(u128),
    Flat,
    Loss(u128),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CashCloseOutcome {
    pub principal_refund: u64,
    pub gross_loss: u128,
    pub cash_loss_collected: u128,
    pub residual_bad_debt_added: u128,
    pub surplus_recorded: u64,
    pub represented_custody_after: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProfitAdjustment {
    pub raw_profit: u128,
    pub favorable_move_ppm: u32,
    pub haircut_rate_ppm: u32,
    pub haircut_withheld: u128,
    pub post_haircut_profit: u128,
    pub win_fee: u128,
    pub trader_profit_before_cap: u128,
    pub capped_trader_profit: u128,
    pub profit_cap_withheld: u128,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfitableCashCloseOutcome {
    pub principal_refund: u64,
    pub immediate_profit_paid: u64,
    pub queued_profit: u128,
    pub claim: Option<ClaimState>,
    pub adjustment: ProfitAdjustment,
    pub surplus_recorded: u64,
    pub represented_custody_after: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClaimCloseOutcome {
    pub immediate_payout: u64,
    pub queued_payout: u128,
    pub child_claim: Option<ClaimState>,
    pub gross_loss: u128,
    pub claim_loss_extinguished: u128,
    pub residual_bad_debt_added: u128,
    pub adjustment: Option<ProfitAdjustment>,
    pub surplus_recorded: u64,
    pub represented_custody_after: u64,
}

pub fn oracle_cash_pnl(
    side: PositionSide,
    notional: u128,
    entry_price_e6: u64,
    exit_price_e6: u64,
) -> StateResult<OracleCashPnl> {
    if notional == 0 || entry_price_e6 == 0 || exit_price_e6 == 0 {
        return Err(StateError::InvalidState);
    }
    if entry_price_e6 == exit_price_e6 {
        return Ok(OracleCashPnl::Flat);
    }
    let favorable = match side {
        PositionSide::Long => exit_price_e6 > entry_price_e6,
        PositionSide::Short => exit_price_e6 < entry_price_e6,
    };
    let delta = u128::from(entry_price_e6.abs_diff(exit_price_e6));
    let denominator =
        percolator_martingaler_engine::martingaler::U256::from_u128(u128::from(entry_price_e6));
    let magnitude = if favorable {
        percolator_martingaler_engine::martingaler::U256::from_u128(notional)
            .checked_mul(percolator_martingaler_engine::martingaler::U256::from_u128(
                delta,
            ))
            .ok_or(StateError::ArithmeticOverflow)?
            / denominator
    } else {
        percolator_martingaler_engine::martingaler::mul_div_ceil_u256(
            percolator_martingaler_engine::martingaler::U256::from_u128(notional),
            percolator_martingaler_engine::martingaler::U256::from_u128(delta),
            denominator,
        )
    }
    .try_into_u128()
    .ok_or(StateError::ArithmeticOverflow)?;
    Ok(if favorable {
        OracleCashPnl::Profit(magnitude)
    } else {
        OracleCashPnl::Loss(magnitude)
    })
}

pub fn favorable_move_ppm(entry_price_e6: u64, exit_price_e6: u64) -> StateResult<u32> {
    if entry_price_e6 == 0 || exit_price_e6 == 0 {
        return Err(StateError::InvalidState);
    }
    let move_ppm = percolator_martingaler_engine::martingaler::wide_mul_div_floor_u128(
        u128::from(entry_price_e6.abs_diff(exit_price_e6)),
        HaircutConfig::RATE_SCALE_PPM,
        u128::from(entry_price_e6),
    );
    Ok(u32::try_from(move_ppm).unwrap_or(u32::MAX))
}

pub fn adjust_profitable_cash_pnl(
    raw_profit: u128,
    favorable_move_ppm: u32,
    haircut: HaircutConfig,
    max_payable_profit: u128,
) -> StateResult<ProfitAdjustment> {
    haircut.validate()?;
    let scale = HaircutConfig::RATE_SCALE_PPM;
    let excess = favorable_move_ppm.saturating_sub(haircut.deadband_move_ppm.get());
    let linear = percolator_martingaler_engine::martingaler::wide_mul_div_floor_u128(
        u128::from(excess),
        u128::from(haircut.linear_coefficient_ppm.get()),
        scale,
    );
    let squared_move = percolator_martingaler_engine::martingaler::wide_mul_div_floor_u128(
        u128::from(excess),
        u128::from(excess),
        scale,
    );
    let quadratic = percolator_martingaler_engine::martingaler::wide_mul_div_floor_u128(
        squared_move,
        u128::from(haircut.quadratic_coefficient_ppm.get()),
        scale,
    );
    let denominator = scale
        .checked_add(linear)
        .and_then(|value| value.checked_add(quadratic))
        .ok_or(StateError::ArithmeticOverflow)?;
    let spread = u128::from(
        haircut
            .max_rate_ppm
            .get()
            .checked_sub(haircut.base_rate_ppm.get())
            .ok_or(StateError::InvalidState)?,
    );
    let decaying_spread = percolator_martingaler_engine::martingaler::wide_mul_div_floor_u128(
        spread,
        scale,
        denominator,
    );
    let haircut_rate = u128::from(haircut.base_rate_ppm.get())
        .checked_add(decaying_spread)
        .ok_or(StateError::ArithmeticOverflow)?;
    let haircut_rate_ppm =
        u32::try_from(haircut_rate).map_err(|_| StateError::ArithmeticOverflow)?;
    let post_haircut_profit = percolator_martingaler_engine::martingaler::wide_mul_div_floor_u128(
        raw_profit,
        scale
            .checked_sub(haircut_rate)
            .ok_or(StateError::ArithmeticOverflow)?,
        scale,
    );
    let trader_profit_before_cap =
        percolator_martingaler_engine::martingaler::wide_mul_div_floor_u128(
            post_haircut_profit,
            scale
                .checked_sub(u128::from(haircut.win_fee_ppm.get()))
                .ok_or(StateError::ArithmeticOverflow)?,
            scale,
        );
    let haircut_withheld = raw_profit
        .checked_sub(post_haircut_profit)
        .ok_or(StateError::ArithmeticOverflow)?;
    let win_fee = post_haircut_profit
        .checked_sub(trader_profit_before_cap)
        .ok_or(StateError::ArithmeticOverflow)?;
    let capped_trader_profit = core::cmp::min(trader_profit_before_cap, max_payable_profit);
    let profit_cap_withheld = trader_profit_before_cap
        .checked_sub(capped_trader_profit)
        .ok_or(StateError::ArithmeticOverflow)?;
    Ok(ProfitAdjustment {
        raw_profit,
        favorable_move_ppm,
        haircut_rate_ppm,
        haircut_withheld,
        post_haircut_profit,
        win_fee,
        trader_profit_before_cap,
        capped_trader_profit,
        profit_cap_withheld,
    })
}

pub fn record_cash_deposit(
    pool_key: Key32,
    pool: &mut PoolState,
    controller: &mut ControllerRiskState,
    observed_vault_before: u64,
    amount: u64,
) -> StateResult<CashDepositOutcome> {
    pool.validate()?;
    controller.validate()?;
    if amount == 0 || pool.status != PoolStatus::Active as u8 || controller.pool != pool_key {
        return Err(StateError::InvalidState);
    }

    let represented_before = pool.represented_quote_custody()?;
    let observed_vault_before = u128::from(observed_vault_before);
    if observed_vault_before < represented_before {
        return Err(StateError::CustodyShortfall);
    }
    let surplus_recorded = observed_vault_before - represented_before;
    let amount_u128 = u128::from(amount);

    let mut next_pool = *pool;
    let mut next_controller = *controller;
    next_pool.unallocated_vault_surplus = U128Le::new(
        next_pool
            .unallocated_vault_surplus
            .get()
            .checked_add(surplus_recorded)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.idle_cash_collateral = U128Le::new(
        next_pool
            .idle_cash_collateral
            .get()
            .checked_add(amount_u128)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.available_cash_collateral = U128Le::new(
        next_controller
            .available_cash_collateral
            .get()
            .checked_add(amount_u128)
            .ok_or(StateError::ArithmeticOverflow)?,
    );

    next_pool.validate()?;
    next_controller.validate()?;
    let represented_after = next_pool.represented_quote_custody()?;
    let expected_after = observed_vault_before
        .checked_add(amount_u128)
        .ok_or(StateError::ArithmeticOverflow)?;
    if represented_after != expected_after {
        return Err(StateError::InvalidState);
    }

    *pool = next_pool;
    *controller = next_controller;
    Ok(CashDepositOutcome {
        amount,
        surplus_recorded: u64::try_from(surplus_recorded)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        represented_custody_after: u64::try_from(represented_after)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
    })
}

pub fn transition_pool_status(pool: &mut PoolState, target: PoolStatus) -> StateResult<()> {
    pool.validate()?;
    let current = PoolStatus::try_from(pool.status)?;
    let permitted = match (current, target) {
        (PoolStatus::Active, PoolStatus::CloseOnly) => true,
        (PoolStatus::CloseOnly, PoolStatus::ClaimsOnly) => {
            pool.protected_principal.get() == 0
                && pool.open_claim_collateral.get() == 0
                && pool.reserved_open_profit.get() == 0
        }
        (PoolStatus::ClaimsOnly, PoolStatus::Terminal) => {
            pool.protected_principal.get() == 0
                && pool.open_claim_collateral.get() == 0
                && pool.reserved_open_profit.get() == 0
                && pool.total_queued.get() == 0
                && pool.claim_service_cash.get() == 0
                && pool.queue_flags == 0
                && pool.head_claim_sequence.get() == NO_CLAIM_SEQUENCE
                && pool.tail_claim_sequence.get() == NO_CLAIM_SEQUENCE
        }
        _ => false,
    };
    if !permitted {
        return Err(StateError::InvalidStatusTransition);
    }
    let mut next = *pool;
    next.status = target as u8;
    next.validate()?;
    *pool = next;
    Ok(())
}

pub fn transition_market_status(
    pool_key: Key32,
    market: &mut MarketRiskState,
    target: PoolStatus,
) -> StateResult<()> {
    market.validate()?;
    if market.pool != pool_key {
        return Err(StateError::InvalidState);
    }
    let current = PoolStatus::try_from(market.status)?;
    let permitted = match (current, target) {
        (PoolStatus::Active, PoolStatus::CloseOnly) => true,
        (PoolStatus::CloseOnly, PoolStatus::Terminal) => {
            market.long_open_interest.get() == 0
                && market.short_open_interest.get() == 0
                && market.reserved_profit.get() == 0
        }
        _ => false,
    };
    if !permitted {
        return Err(StateError::InvalidStatusTransition);
    }
    let mut next = *market;
    next.status = target as u8;
    next.validate()?;
    *market = next;
    Ok(())
}

pub fn withdraw_idle_cash(
    pool_key: Key32,
    controller_key: Key32,
    pool: &mut PoolState,
    controller: &mut ControllerRiskState,
    observed_vault_before: u64,
    amount: u64,
) -> StateResult<IdleCashWithdrawalOutcome> {
    pool.validate()?;
    controller.validate()?;
    if amount == 0
        || controller.pool != pool_key
        || controller.controller != controller_key
        || controller.available_cash_collateral.get() < u128::from(amount)
        || pool.idle_cash_collateral.get() < u128::from(amount)
    {
        return Err(StateError::InsufficientCashCollateral);
    }
    let represented_before = pool.represented_quote_custody()?;
    let observed_vault_before = u128::from(observed_vault_before);
    if observed_vault_before < represented_before {
        return Err(StateError::CustodyShortfall);
    }
    let surplus_recorded = observed_vault_before - represented_before;
    let amount_u128 = u128::from(amount);

    let mut next_pool = *pool;
    let mut next_controller = *controller;
    next_pool.unallocated_vault_surplus = U128Le::new(
        next_pool
            .unallocated_vault_surplus
            .get()
            .checked_add(surplus_recorded)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.idle_cash_collateral = U128Le::new(
        next_pool
            .idle_cash_collateral
            .get()
            .checked_sub(amount_u128)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.available_cash_collateral = U128Le::new(
        next_controller
            .available_cash_collateral
            .get()
            .checked_sub(amount_u128)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.validate()?;
    next_controller.validate()?;
    let represented_after = next_pool.represented_quote_custody()?;
    if represented_after
        != observed_vault_before
            .checked_sub(amount_u128)
            .ok_or(StateError::ArithmeticOverflow)?
    {
        return Err(StateError::InvalidState);
    }

    *pool = next_pool;
    *controller = next_controller;
    Ok(IdleCashWithdrawalOutcome {
        amount,
        surplus_recorded: u64::try_from(surplus_recorded)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        represented_custody_after: u64::try_from(represented_after)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn quote_payout_factor(
    pool: &PoolState,
    market: &MarketRiskState,
    controller: &ControllerRiskState,
    notional: u128,
    min_payout_factor_bps: u16,
) -> StateResult<PayoutQuote> {
    pool.validate()?;
    market.validate()?;
    controller.validate()?;
    if min_payout_factor_bps == LEGACY_FULL_RESERVE_ADMISSION {
        let reserved_profit = core::cmp::min(notional, market.max_net_profit_per_position.get());
        let controller_reserve = controller
            .reserved_profit
            .get()
            .checked_add(reserved_profit)
            .ok_or(StateError::ArithmeticOverflow)?;
        let total_liability = pool
            .total_queued
            .get()
            .checked_add(pool.open_claim_collateral.get())
            .and_then(|value| value.checked_add(pool.residual_bad_debt.get()))
            .and_then(|value| value.checked_add(pool.reserved_open_profit.get()))
            .and_then(|value| value.checked_add(reserved_profit))
            .ok_or(StateError::ArithmeticOverflow)?;
        if controller_reserve > pool.max_reserved_profit_per_controller.get()
            || total_liability > pool.total_liability_open_gate.get()
        {
            return Err(StateError::LiabilityLimitExceeded);
        }
        return Ok(PayoutQuote {
            max_payable_profit: reserved_profit,
            payout_factor_bps: V1_PAYOUT_FACTOR_SCALE_BPS,
        });
    }
    if notional == 0
        || (min_payout_factor_bps != 0
            && !(V1_MIN_PAYOUT_FACTOR_BPS..=V1_PAYOUT_FACTOR_SCALE_BPS)
                .contains(&min_payout_factor_bps))
    {
        return Err(StateError::RiskLimitExceeded);
    }
    let nominal_profit = core::cmp::min(notional, market.max_net_profit_per_position.get());
    let controller_depth = pool
        .max_reserved_profit_per_controller
        .get()
        .saturating_sub(controller.reserved_profit.get());
    let current_liability = pool
        .total_queued
        .get()
        .checked_add(pool.open_claim_collateral.get())
        .and_then(|value| value.checked_add(pool.residual_bad_debt.get()))
        .and_then(|value| value.checked_add(pool.reserved_open_profit.get()))
        .ok_or(StateError::ArithmeticOverflow)?;
    let total_depth = pool
        .total_liability_open_gate
        .get()
        .saturating_sub(current_liability);
    let available = core::cmp::min(
        nominal_profit,
        core::cmp::min(controller_depth, total_depth),
    );
    let payout_factor_bps = available
        .checked_mul(u128::from(V1_PAYOUT_FACTOR_SCALE_BPS))
        .ok_or(StateError::ArithmeticOverflow)?
        / nominal_profit;
    let payout_factor_bps =
        u16::try_from(payout_factor_bps).map_err(|_| StateError::ArithmeticOverflow)?;
    if payout_factor_bps < V1_MIN_PAYOUT_FACTOR_BPS
        || (min_payout_factor_bps != 0 && payout_factor_bps < min_payout_factor_bps)
    {
        return Err(StateError::LiabilityLimitExceeded);
    }
    let max_payable_profit = nominal_profit
        .checked_mul(u128::from(payout_factor_bps))
        .ok_or(StateError::ArithmeticOverflow)?
        / u128::from(V1_PAYOUT_FACTOR_SCALE_BPS);
    Ok(PayoutQuote {
        max_payable_profit,
        payout_factor_bps,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn open_cash_position_from_idle(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    observed_vault_balance: u64,
    position_id: u64,
    side: PositionSide,
    collateral: u128,
    notional: u128,
    leverage_x: u16,
    entry_price_e6: u64,
    entry_publish_time: i64,
    secondary_entry_price_e6: u64,
    secondary_publish_time: i64,
) -> StateResult<CashOpenOutcome> {
    open_cash_position_from_idle_guarded(
        pool_key,
        market_key,
        controller_key,
        pool,
        market,
        controller,
        observed_vault_balance,
        position_id,
        side,
        collateral,
        notional,
        leverage_x,
        entry_price_e6,
        entry_publish_time,
        secondary_entry_price_e6,
        secondary_publish_time,
        LEGACY_FULL_RESERVE_ADMISSION,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn open_cash_position_from_idle_guarded(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    observed_vault_balance: u64,
    position_id: u64,
    side: PositionSide,
    collateral: u128,
    notional: u128,
    leverage_x: u16,
    entry_price_e6: u64,
    entry_publish_time: i64,
    secondary_entry_price_e6: u64,
    secondary_publish_time: i64,
    min_payout_factor_bps: u16,
) -> StateResult<CashOpenOutcome> {
    pool.validate()?;
    market.validate()?;
    controller.validate()?;
    if pool.status != PoolStatus::Active as u8
        || market.status != PoolStatus::Active as u8
        || market.pool != pool_key
        || controller.pool != pool_key
        || controller.controller != controller_key
        || market_key.is_zero()
        || market.market_id.get() == 0
        || position_id == 0
        || (min_payout_factor_bps != LEGACY_FULL_RESERVE_ADMISSION
            && collateral < V1_MIN_COLLATERAL_QUOTE)
        || notional == 0
        || leverage_x == 0
        || leverage_x > market.max_leverage_x.get()
        || market.max_leverage_x.get() > pool.max_leverage_x.get()
        || market.max_position_notional.get() > pool.max_position_notional.get()
        || market.max_net_profit_per_position.get() > pool.max_net_profit_per_position.get()
        || notional > market.max_position_notional.get()
    {
        return Err(StateError::RiskLimitExceeded);
    }

    let exact_notional = collateral
        .checked_mul(u128::from(leverage_x))
        .ok_or(StateError::ArithmeticOverflow)?;
    if notional != exact_notional {
        return Err(StateError::RiskLimitExceeded);
    }

    let represented_before = pool.represented_quote_custody()?;
    let observed_vault_balance = u128::from(observed_vault_balance);
    if observed_vault_balance < represented_before {
        return Err(StateError::CustodyShortfall);
    }
    let surplus_recorded = observed_vault_balance - represented_before;
    if controller.available_cash_collateral.get() < collateral
        || pool.idle_cash_collateral.get() < collateral
    {
        return Err(StateError::InsufficientCashCollateral);
    }

    let payout = quote_payout_factor(pool, market, controller, notional, min_payout_factor_bps)?;
    let reserved_profit = payout.max_payable_profit;
    let next_controller_reserve = controller
        .reserved_profit
        .get()
        .checked_add(reserved_profit)
        .ok_or(StateError::ArithmeticOverflow)?;
    if next_controller_reserve > pool.max_reserved_profit_per_controller.get() {
        return Err(StateError::LiabilityLimitExceeded);
    }
    let next_pool_reserve = pool
        .reserved_open_profit
        .get()
        .checked_add(reserved_profit)
        .ok_or(StateError::ArithmeticOverflow)?;
    let total_potential_liability = pool
        .total_queued
        .get()
        .checked_add(pool.open_claim_collateral.get())
        .and_then(|value| value.checked_add(pool.residual_bad_debt.get()))
        .and_then(|value| value.checked_add(next_pool_reserve))
        .ok_or(StateError::ArithmeticOverflow)?;
    if total_potential_liability > pool.total_liability_open_gate.get() {
        return Err(StateError::LiabilityLimitExceeded);
    }

    let mut next_pool = *pool;
    let mut next_market = *market;
    let mut next_controller = *controller;
    let target_oi = match side {
        PositionSide::Long => &mut next_market.long_open_interest,
        PositionSide::Short => &mut next_market.short_open_interest,
    };
    let next_oi = target_oi
        .get()
        .checked_add(notional)
        .ok_or(StateError::ArithmeticOverflow)?;
    let max_oi = match side {
        PositionSide::Long => market.max_long_open_interest.get(),
        PositionSide::Short => market.max_short_open_interest.get(),
    };
    if next_oi > max_oi {
        return Err(StateError::RiskLimitExceeded);
    }
    *target_oi = U128Le::new(next_oi);

    next_pool.unallocated_vault_surplus = U128Le::new(
        next_pool
            .unallocated_vault_surplus
            .get()
            .checked_add(surplus_recorded)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.idle_cash_collateral = U128Le::new(
        next_pool
            .idle_cash_collateral
            .get()
            .checked_sub(collateral)
            .ok_or(StateError::InsufficientCashCollateral)?,
    );
    next_pool.protected_principal = U128Le::new(
        next_pool
            .protected_principal
            .get()
            .checked_add(collateral)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.reserved_open_profit = U128Le::new(next_pool_reserve);
    next_market.reserved_profit = U128Le::new(
        next_market
            .reserved_profit
            .get()
            .checked_add(reserved_profit)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.available_cash_collateral = U128Le::new(
        next_controller
            .available_cash_collateral
            .get()
            .checked_sub(collateral)
            .ok_or(StateError::InsufficientCashCollateral)?,
    );
    next_controller.reserved_profit = U128Le::new(next_controller_reserve);
    next_controller.open_positions = U32Le::new(
        next_controller
            .open_positions
            .get()
            .checked_add(1)
            .ok_or(StateError::ArithmeticOverflow)?,
    );

    let position = PositionState::new_cash(
        pool_key,
        controller_key,
        position_id,
        market.market_id.get(),
        side,
        leverage_x,
        collateral,
        notional,
        reserved_profit,
        entry_price_e6,
        entry_publish_time,
        secondary_entry_price_e6,
        secondary_publish_time,
    )?;

    next_pool.validate()?;
    next_market.validate()?;
    next_controller.validate()?;
    let represented_after = next_pool.represented_quote_custody()?;
    if represented_after != observed_vault_balance {
        return Err(StateError::InvalidState);
    }

    *pool = next_pool;
    *market = next_market;
    *controller = next_controller;
    Ok(CashOpenOutcome {
        position,
        reserved_profit,
        surplus_recorded: u64::try_from(surplus_recorded)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        represented_custody_after: u64::try_from(represented_after)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn open_claim_position_from_queue(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    source_claim_key: Key32,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    source_page: &mut QueuePageState,
    source_claim: &mut ClaimState,
    observed_vault_balance: u64,
    position_id: u64,
    side: PositionSide,
    claim_amount: u128,
    notional: u128,
    leverage_x: u16,
    entry_price_e6: u64,
    entry_publish_time: i64,
    secondary_entry_price_e6: u64,
    secondary_publish_time: i64,
) -> StateResult<ClaimOpenOutcome> {
    open_claim_position_from_queue_guarded(
        pool_key,
        market_key,
        controller_key,
        source_claim_key,
        pool,
        market,
        controller,
        source_page,
        source_claim,
        observed_vault_balance,
        position_id,
        side,
        claim_amount,
        notional,
        leverage_x,
        entry_price_e6,
        entry_publish_time,
        secondary_entry_price_e6,
        secondary_publish_time,
        LEGACY_FULL_RESERVE_ADMISSION,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn open_claim_position_from_queue_guarded(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    source_claim_key: Key32,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    source_page: &mut QueuePageState,
    source_claim: &mut ClaimState,
    observed_vault_balance: u64,
    position_id: u64,
    side: PositionSide,
    claim_amount: u128,
    notional: u128,
    leverage_x: u16,
    entry_price_e6: u64,
    entry_publish_time: i64,
    secondary_entry_price_e6: u64,
    secondary_publish_time: i64,
    min_payout_factor_bps: u16,
) -> StateResult<ClaimOpenOutcome> {
    pool.validate()?;
    market.validate()?;
    controller.validate()?;
    source_page.validate()?;
    source_claim.validate()?;
    if pool.status != PoolStatus::Active as u8
        || market.status != PoolStatus::Active as u8
        || market.pool != pool_key
        || controller.pool != pool_key
        || controller.controller != controller_key
        || source_claim.pool != pool_key
        || source_claim.controller != controller_key
        || source_claim.sequence.get() == 0
        || source_claim.sequence.get() == NO_CLAIM_SEQUENCE
        || market_key.is_zero()
        || market.market_id.get() == 0
        || position_id == 0
        || (min_payout_factor_bps != LEGACY_FULL_RESERVE_ADMISSION
            && claim_amount < V1_MIN_COLLATERAL_QUOTE)
        || notional == 0
        || leverage_x == 0
        || leverage_x > market.max_leverage_x.get()
        || market.max_leverage_x.get() > pool.max_leverage_x.get()
        || market.max_position_notional.get() > pool.max_position_notional.get()
        || market.max_net_profit_per_position.get() > pool.max_net_profit_per_position.get()
        || notional > market.max_position_notional.get()
    {
        return Err(StateError::RiskLimitExceeded);
    }

    let exact_notional = claim_amount
        .checked_mul(u128::from(leverage_x))
        .ok_or(StateError::ArithmeticOverflow)?;
    if notional != exact_notional {
        return Err(StateError::RiskLimitExceeded);
    }

    let represented_before = pool.represented_quote_custody()?;
    let observed_vault_balance = u128::from(observed_vault_balance);
    if observed_vault_balance < represented_before {
        return Err(StateError::CustodyShortfall);
    }
    let surplus_recorded = observed_vault_balance - represented_before;
    let payout = quote_payout_factor(pool, market, controller, notional, min_payout_factor_bps)?;
    let reserved_profit = payout.max_payable_profit;
    let next_controller_reserve = controller
        .reserved_profit
        .get()
        .checked_add(reserved_profit)
        .ok_or(StateError::ArithmeticOverflow)?;
    if next_controller_reserve > pool.max_reserved_profit_per_controller.get() {
        return Err(StateError::LiabilityLimitExceeded);
    }
    let next_pool_reserve = pool
        .reserved_open_profit
        .get()
        .checked_add(reserved_profit)
        .ok_or(StateError::ArithmeticOverflow)?;
    let total_potential_liability = pool
        .total_queued
        .get()
        .checked_add(pool.open_claim_collateral.get())
        .and_then(|value| value.checked_add(pool.residual_bad_debt.get()))
        .and_then(|value| value.checked_add(next_pool_reserve))
        .ok_or(StateError::ArithmeticOverflow)?;
    if total_potential_liability > pool.total_liability_open_gate.get() {
        return Err(StateError::LiabilityLimitExceeded);
    }

    let mut next_pool = *pool;
    let mut next_market = *market;
    let mut next_controller = *controller;
    let mut next_source_page = boxed_queue_page_copy(source_page);
    let mut next_source_claim = *source_claim;
    next_pool.unallocated_vault_surplus = U128Le::new(
        next_pool
            .unallocated_vault_surplus
            .get()
            .checked_add(surplus_recorded)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    let source = consume_claim_for_position(
        pool_key,
        &mut next_pool,
        &mut next_controller,
        next_source_page.as_mut(),
        source_claim_key,
        &mut next_source_claim,
        claim_amount,
    )?;

    let target_oi = match side {
        PositionSide::Long => &mut next_market.long_open_interest,
        PositionSide::Short => &mut next_market.short_open_interest,
    };
    let next_oi = target_oi
        .get()
        .checked_add(notional)
        .ok_or(StateError::ArithmeticOverflow)?;
    let max_oi = match side {
        PositionSide::Long => market.max_long_open_interest.get(),
        PositionSide::Short => market.max_short_open_interest.get(),
    };
    if next_oi > max_oi {
        return Err(StateError::RiskLimitExceeded);
    }
    *target_oi = U128Le::new(next_oi);
    next_pool.reserved_open_profit = U128Le::new(next_pool_reserve);
    next_market.reserved_profit = U128Le::new(
        next_market
            .reserved_profit
            .get()
            .checked_add(reserved_profit)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.reserved_profit = U128Le::new(next_controller_reserve);
    next_controller.open_positions = U32Le::new(
        next_controller
            .open_positions
            .get()
            .checked_add(1)
            .ok_or(StateError::ArithmeticOverflow)?,
    );

    let position = PositionState::new_claim(
        pool_key,
        controller_key,
        position_id,
        market.market_id.get(),
        side,
        leverage_x,
        source_claim.sequence.get(),
        claim_amount,
        notional,
        reserved_profit,
        entry_price_e6,
        entry_publish_time,
        secondary_entry_price_e6,
        secondary_publish_time,
    )?;

    next_pool.validate()?;
    next_market.validate()?;
    next_controller.validate()?;
    next_source_page.validate()?;
    next_source_claim.validate()?;
    position.validate()?;
    let represented_after = next_pool.represented_quote_custody()?;
    if represented_after != observed_vault_balance {
        return Err(StateError::InvalidState);
    }

    *pool = next_pool;
    *market = next_market;
    *controller = next_controller;
    *source_page = *next_source_page;
    *source_claim = next_source_claim;
    Ok(ClaimOpenOutcome {
        position,
        source,
        reserved_profit,
        surplus_recorded: u64::try_from(surplus_recorded)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        represented_custody_after: u64::try_from(represented_after)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn close_cash_position(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    position: &mut PositionState,
    observed_vault_balance: u64,
    primary_exit_price_e6: u64,
    secondary_exit_price_e6: Option<u64>,
    require_bankruptcy: bool,
) -> StateResult<CashCloseOutcome> {
    pool.validate()?;
    market.validate()?;
    controller.validate()?;
    position.validate()?;
    let pool_closable = matches!(
        PoolStatus::try_from(pool.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    );
    let market_closable = matches!(
        PoolStatus::try_from(market.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    );
    if !pool_closable
        || !market_closable
        || market.pool != pool_key
        || controller.pool != pool_key
        || controller.controller != controller_key
        || position.pool != pool_key
        || position.controller != controller_key
        || position.market_id != market.market_id
        || market_key.is_zero()
        || position.status != PositionStatus::Open as u8
        || position.funding_kind != FundingKind::Cash as u8
    {
        return Err(StateError::InvalidState);
    }
    let side = PositionSide::try_from(position.side)?;
    let primary_pnl = oracle_cash_pnl(
        side,
        position.notional.get(),
        position.entry_price_e6.get(),
        primary_exit_price_e6,
    )?;
    let primary_loss = match primary_pnl {
        OracleCashPnl::Profit(_) => return Err(StateError::FavorablePosition),
        OracleCashPnl::Flat => 0,
        OracleCashPnl::Loss(loss) => loss,
    };
    let collateral = position.collateral.get();
    let has_secondary = position.secondary_entry_price_e6.get() != 0;
    if require_bankruptcy {
        if primary_loss < collateral || has_secondary != secondary_exit_price_e6.is_some() {
            return Err(StateError::PositionNotBankrupt);
        }
        if let Some(exit_price) = secondary_exit_price_e6 {
            let secondary_loss = match oracle_cash_pnl(
                side,
                position.notional.get(),
                position.secondary_entry_price_e6.get(),
                exit_price,
            )? {
                OracleCashPnl::Loss(loss) => loss,
                OracleCashPnl::Flat | OracleCashPnl::Profit(_) => {
                    return Err(StateError::PositionNotBankrupt)
                }
            };
            if secondary_loss < collateral {
                return Err(StateError::PositionNotBankrupt);
            }
        }
    } else if has_secondary != secondary_exit_price_e6.is_some() {
        return Err(StateError::InvalidState);
    }

    let represented_before = pool.represented_quote_custody()?;
    let observed_vault_balance = u128::from(observed_vault_balance);
    if observed_vault_balance < represented_before {
        return Err(StateError::CustodyShortfall);
    }
    let surplus_recorded = observed_vault_balance - represented_before;
    let collected = core::cmp::min(primary_loss, collateral);
    let refund = collateral
        .checked_sub(collected)
        .ok_or(StateError::ArithmeticOverflow)?;
    let residual = primary_loss
        .checked_sub(collected)
        .ok_or(StateError::ArithmeticOverflow)?;

    let mut next_pool = *pool;
    let mut next_market = *market;
    let mut next_controller = *controller;
    let mut next_position = *position;
    next_pool.unallocated_vault_surplus = U128Le::new(
        next_pool
            .unallocated_vault_surplus
            .get()
            .checked_add(surplus_recorded)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.protected_principal = U128Le::new(
        next_pool
            .protected_principal
            .get()
            .checked_sub(collateral)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.idle_cash_collateral = U128Le::new(
        next_pool
            .idle_cash_collateral
            .get()
            .checked_add(refund)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.reserved_open_profit = U128Le::new(
        next_pool
            .reserved_open_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    let target_oi = match side {
        PositionSide::Long => &mut next_market.long_open_interest,
        PositionSide::Short => &mut next_market.short_open_interest,
    };
    *target_oi = U128Le::new(
        target_oi
            .get()
            .checked_sub(position.notional.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_market.reserved_profit = U128Le::new(
        next_market
            .reserved_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.reserved_profit = U128Le::new(
        next_controller
            .reserved_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.open_positions = U32Le::new(
        next_controller
            .open_positions
            .get()
            .checked_sub(1)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.available_cash_collateral = U128Le::new(
        next_controller
            .available_cash_collateral
            .get()
            .checked_add(refund)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    if collected != 0 {
        if next_pool.total_queued.get() != 0 || next_pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED != 0
        {
            next_pool.claim_service_cash = U128Le::new(
                next_pool
                    .claim_service_cash
                    .get()
                    .checked_add(collected)
                    .ok_or(StateError::ArithmeticOverflow)?,
            );
        } else {
            next_pool.free_cash = U128Le::new(
                next_pool
                    .free_cash
                    .get()
                    .checked_add(collected)
                    .ok_or(StateError::ArithmeticOverflow)?,
            );
        }
    }
    next_pool.residual_bad_debt = U128Le::new(
        next_pool
            .residual_bad_debt
            .get()
            .checked_add(residual)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_position.status = PositionStatus::Closed as u8;
    next_position.reserved_profit = U128Le::new(0);

    next_pool.validate()?;
    next_market.validate()?;
    next_controller.validate()?;
    next_position.validate()?;
    let represented_after = next_pool.represented_quote_custody()?;
    if represented_after != observed_vault_balance {
        return Err(StateError::InvalidState);
    }

    *pool = next_pool;
    *market = next_market;
    *controller = next_controller;
    *position = next_position;
    Ok(CashCloseOutcome {
        principal_refund: u64::try_from(refund).map_err(|_| StateError::CustodyCapacityExceeded)?,
        gross_loss: primary_loss,
        cash_loss_collected: collected,
        residual_bad_debt_added: residual,
        surplus_recorded: u64::try_from(surplus_recorded)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        represented_custody_after: u64::try_from(represented_after)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
    })
}

fn boxed_queue_page_copy(page: &QueuePageState) -> Box<QueuePageState> {
    let mut copy = Box::<QueuePageState>::new_uninit();
    // SAFETY: both pointers are valid for one `QueuePageState`, do not overlap,
    // and `QueuePageState: Copy + Pod`.
    unsafe {
        core::ptr::copy_nonoverlapping(page, copy.as_mut_ptr(), 1);
        copy.assume_init()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn close_profitable_cash_position(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    claim_key: Option<Key32>,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    position: &mut PositionState,
    current_tail: Option<&mut QueuePageState>,
    rollover_tail: Option<&mut QueuePageState>,
    observed_vault_balance: u64,
    primary_exit_price_e6: u64,
    secondary_exit_price_e6: Option<u64>,
) -> StateResult<ProfitableCashCloseOutcome> {
    pool.validate()?;
    market.validate()?;
    controller.validate()?;
    position.validate()?;
    let pool_closable = matches!(
        PoolStatus::try_from(pool.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    );
    let market_closable = matches!(
        PoolStatus::try_from(market.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    );
    if !pool_closable
        || !market_closable
        || market.pool != pool_key
        || controller.pool != pool_key
        || controller.controller != controller_key
        || position.pool != pool_key
        || position.controller != controller_key
        || position.market_id != market.market_id
        || market_key.is_zero()
        || position.status != PositionStatus::Open as u8
        || position.funding_kind != FundingKind::Cash as u8
        || (position.secondary_entry_price_e6.get() != 0) != secondary_exit_price_e6.is_some()
    {
        return Err(StateError::InvalidState);
    }

    let side = PositionSide::try_from(position.side)?;
    let raw_profit = match oracle_cash_pnl(
        side,
        position.notional.get(),
        position.entry_price_e6.get(),
        primary_exit_price_e6,
    )? {
        OracleCashPnl::Profit(profit) => profit,
        OracleCashPnl::Flat | OracleCashPnl::Loss(_) => {
            return Err(StateError::PositionNotProfitable)
        }
    };
    let move_ppm = favorable_move_ppm(position.entry_price_e6.get(), primary_exit_price_e6)?;
    let adjustment = adjust_profitable_cash_pnl(
        raw_profit,
        move_ppm,
        market.haircut,
        position.max_payable_profit.get(),
    )?;

    let represented_before = pool.represented_quote_custody()?;
    let observed = u128::from(observed_vault_balance);
    if observed < represented_before {
        return Err(StateError::CustodyShortfall);
    }
    let surplus_recorded = observed - represented_before;
    let predecessor_blocks =
        pool.total_queued.get() != 0 || pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED != 0;
    let immediate_profit = if predecessor_blocks {
        0
    } else {
        core::cmp::min(pool.free_cash.get(), adjustment.capped_trader_profit)
    };
    let queued_profit = adjustment
        .capped_trader_profit
        .checked_sub(immediate_profit)
        .ok_or(StateError::ArithmeticOverflow)?;

    if queued_profit == 0 {
        if claim_key.is_some() || current_tail.is_some() || rollover_tail.is_some() {
            return Err(StateError::SettlementShapeMismatch);
        }
    } else if claim_key.is_none() || current_tail.is_none() {
        return Err(StateError::SettlementShapeMismatch);
    }

    let mut next_pool = *pool;
    let mut next_market = *market;
    let mut next_controller = *controller;
    let mut next_position = *position;
    let mut next_current_tail = current_tail
        .as_ref()
        .map(|page| boxed_queue_page_copy(page));
    let mut next_rollover_tail = rollover_tail
        .as_ref()
        .map(|page| boxed_queue_page_copy(page));

    next_pool.unallocated_vault_surplus = U128Le::new(
        next_pool
            .unallocated_vault_surplus
            .get()
            .checked_add(surplus_recorded)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.protected_principal = U128Le::new(
        next_pool
            .protected_principal
            .get()
            .checked_sub(position.collateral.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    let internal_credit = position
        .collateral
        .get()
        .checked_add(immediate_profit)
        .ok_or(StateError::ArithmeticOverflow)?;
    next_pool.idle_cash_collateral = U128Le::new(
        next_pool
            .idle_cash_collateral
            .get()
            .checked_add(internal_credit)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.free_cash = U128Le::new(
        next_pool
            .free_cash
            .get()
            .checked_sub(immediate_profit)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.reserved_open_profit = U128Le::new(
        next_pool
            .reserved_open_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.fee_accrual = U128Le::new(
        next_pool
            .fee_accrual
            .get()
            .saturating_add(adjustment.win_fee),
    );
    let market_open_interest = match side {
        PositionSide::Long => &mut next_market.long_open_interest,
        PositionSide::Short => &mut next_market.short_open_interest,
    };
    *market_open_interest = U128Le::new(
        market_open_interest
            .get()
            .checked_sub(position.notional.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_market.reserved_profit = U128Le::new(
        next_market
            .reserved_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.reserved_profit = U128Le::new(
        next_controller
            .reserved_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.open_positions = U32Le::new(
        next_controller
            .open_positions
            .get()
            .checked_sub(1)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.available_cash_collateral = U128Le::new(
        next_controller
            .available_cash_collateral
            .get()
            .checked_add(internal_credit)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_position.status = PositionStatus::Closed as u8;
    next_position.reserved_profit = U128Le::new(0);

    let mut claim = None;
    if queued_profit != 0 {
        let claim_key = claim_key.ok_or(StateError::SettlementShapeMismatch)?;
        let current = next_current_tail
            .as_deref_mut()
            .ok_or(StateError::SettlementShapeMismatch)?;
        let (append, page_index) = if current.is_full() {
            let rollover = next_rollover_tail
                .as_deref_mut()
                .ok_or(StateError::SettlementShapeMismatch)?;
            link_next_queue_page(pool_key, &mut next_pool, current, rollover)?;
            let outcome =
                append_claim(pool_key, &mut next_pool, rollover, claim_key, queued_profit)?;
            (outcome, rollover.page_index.get())
        } else {
            if next_rollover_tail.is_some() {
                return Err(StateError::SettlementShapeMismatch);
            }
            let page_index = current.page_index.get();
            let outcome =
                append_claim(pool_key, &mut next_pool, current, claim_key, queued_profit)?;
            (outcome, page_index)
        };
        let next_claim = ClaimState::new_cash(
            pool_key,
            controller_key,
            position.position_id.get(),
            position.market_id.get(),
            page_index,
            append.slot,
            append.sequence,
            queued_profit,
        )?;
        next_controller.queued_claim_face = U128Le::new(
            next_controller
                .queued_claim_face
                .get()
                .checked_add(queued_profit)
                .ok_or(StateError::ArithmeticOverflow)?,
        );
        next_controller.active_claims = U32Le::new(
            next_controller
                .active_claims
                .get()
                .checked_add(1)
                .ok_or(StateError::ArithmeticOverflow)?,
        );
        claim = Some(next_claim);
    } else if next_rollover_tail.is_some() {
        return Err(StateError::SettlementShapeMismatch);
    }

    next_pool.validate()?;
    next_market.validate()?;
    next_controller.validate()?;
    next_position.validate()?;
    if let Some(page) = next_current_tail.as_deref() {
        page.validate()?;
    }
    if let Some(page) = next_rollover_tail.as_deref() {
        page.validate()?;
    }
    if let Some(claim) = claim {
        claim.validate()?;
    }
    let represented_after = next_pool.represented_quote_custody()?;
    if represented_after != observed {
        return Err(StateError::InvalidState);
    }

    *pool = next_pool;
    *market = next_market;
    *controller = next_controller;
    *position = next_position;
    if let (Some(destination), Some(value)) = (current_tail, next_current_tail) {
        *destination = *value;
    }
    if let (Some(destination), Some(value)) = (rollover_tail, next_rollover_tail) {
        *destination = *value;
    }
    Ok(ProfitableCashCloseOutcome {
        principal_refund: u64::try_from(position.collateral.get())
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        immediate_profit_paid: u64::try_from(immediate_profit)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        queued_profit,
        claim,
        adjustment,
        surplus_recorded: u64::try_from(surplus_recorded)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        represented_custody_after: u64::try_from(represented_after)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClaimCloseMode {
    NonPositive,
    Profitable,
    Bankrupt,
}

#[allow(clippy::too_many_arguments)]
pub fn close_nonpositive_claim_position(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    child_claim_key: Option<Key32>,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    source_claim: &mut ClaimState,
    position: &mut PositionState,
    current_tail: Option<&mut QueuePageState>,
    rollover_tail: Option<&mut QueuePageState>,
    observed_vault_balance: u64,
    primary_exit_price_e6: u64,
    secondary_exit_price_e6: Option<u64>,
) -> StateResult<ClaimCloseOutcome> {
    close_claim_position_inner(
        pool_key,
        market_key,
        controller_key,
        child_claim_key,
        pool,
        market,
        controller,
        source_claim,
        position,
        current_tail,
        rollover_tail,
        observed_vault_balance,
        primary_exit_price_e6,
        secondary_exit_price_e6,
        ClaimCloseMode::NonPositive,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn close_profitable_claim_position(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    child_claim_key: Option<Key32>,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    source_claim: &mut ClaimState,
    position: &mut PositionState,
    current_tail: Option<&mut QueuePageState>,
    rollover_tail: Option<&mut QueuePageState>,
    observed_vault_balance: u64,
    primary_exit_price_e6: u64,
    secondary_exit_price_e6: Option<u64>,
) -> StateResult<ClaimCloseOutcome> {
    close_claim_position_inner(
        pool_key,
        market_key,
        controller_key,
        child_claim_key,
        pool,
        market,
        controller,
        source_claim,
        position,
        current_tail,
        rollover_tail,
        observed_vault_balance,
        primary_exit_price_e6,
        secondary_exit_price_e6,
        ClaimCloseMode::Profitable,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn resolve_bankrupt_claim_position(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    source_claim: &mut ClaimState,
    position: &mut PositionState,
    observed_vault_balance: u64,
    primary_exit_price_e6: u64,
    secondary_exit_price_e6: Option<u64>,
) -> StateResult<ClaimCloseOutcome> {
    close_claim_position_inner(
        pool_key,
        market_key,
        controller_key,
        None,
        pool,
        market,
        controller,
        source_claim,
        position,
        None,
        None,
        observed_vault_balance,
        primary_exit_price_e6,
        secondary_exit_price_e6,
        ClaimCloseMode::Bankrupt,
    )
}

#[allow(clippy::too_many_arguments)]
fn close_claim_position_inner(
    pool_key: Key32,
    market_key: Key32,
    controller_key: Key32,
    child_claim_key: Option<Key32>,
    pool: &mut PoolState,
    market: &mut MarketRiskState,
    controller: &mut ControllerRiskState,
    source_claim: &mut ClaimState,
    position: &mut PositionState,
    current_tail: Option<&mut QueuePageState>,
    rollover_tail: Option<&mut QueuePageState>,
    observed_vault_balance: u64,
    primary_exit_price_e6: u64,
    secondary_exit_price_e6: Option<u64>,
    mode: ClaimCloseMode,
) -> StateResult<ClaimCloseOutcome> {
    pool.validate()?;
    market.validate()?;
    controller.validate()?;
    source_claim.validate()?;
    position.validate()?;
    let pool_closable = matches!(
        PoolStatus::try_from(pool.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    );
    let market_closable = matches!(
        PoolStatus::try_from(market.status)?,
        PoolStatus::Active | PoolStatus::CloseOnly
    );
    let collateral = position.collateral.get();
    if !pool_closable
        || !market_closable
        || market.pool != pool_key
        || controller.pool != pool_key
        || controller.controller != controller_key
        || source_claim.pool != pool_key
        || source_claim.controller != controller_key
        || source_claim.sequence != position.source_claim_sequence
        || source_claim.collateral_in_use.get() < collateral
        || pool.open_claim_collateral.get() < collateral
        || controller.claim_collateral_in_use.get() < collateral
        || position.pool != pool_key
        || position.controller != controller_key
        || position.market_id != market.market_id
        || market_key.is_zero()
        || position.status != PositionStatus::Open as u8
        || position.funding_kind != FundingKind::Claim as u8
        || controller.active_claims.get() == 0
    {
        return Err(StateError::InvalidState);
    }

    let side = PositionSide::try_from(position.side)?;
    let has_secondary = position.secondary_entry_price_e6.get() != 0;
    if has_secondary != secondary_exit_price_e6.is_some() {
        return Err(match mode {
            ClaimCloseMode::Bankrupt => StateError::PositionNotBankrupt,
            ClaimCloseMode::NonPositive | ClaimCloseMode::Profitable => StateError::InvalidState,
        });
    }
    let primary_pnl = oracle_cash_pnl(
        side,
        position.notional.get(),
        position.entry_price_e6.get(),
        primary_exit_price_e6,
    )?;
    let (gross_loss, adjustment, payable_before_cash) = match mode {
        ClaimCloseMode::NonPositive => match primary_pnl {
            OracleCashPnl::Profit(_) => return Err(StateError::FavorablePosition),
            OracleCashPnl::Flat => (0, None, collateral),
            OracleCashPnl::Loss(loss) => (loss, None, collateral.saturating_sub(loss)),
        },
        ClaimCloseMode::Profitable => {
            let raw_profit = match primary_pnl {
                OracleCashPnl::Profit(profit) => profit,
                OracleCashPnl::Flat | OracleCashPnl::Loss(_) => {
                    return Err(StateError::PositionNotProfitable)
                }
            };
            let move_ppm =
                favorable_move_ppm(position.entry_price_e6.get(), primary_exit_price_e6)?;
            let adjustment = adjust_profitable_cash_pnl(
                raw_profit,
                move_ppm,
                market.haircut,
                position.max_payable_profit.get(),
            )?;
            let payable = collateral
                .checked_add(adjustment.capped_trader_profit)
                .ok_or(StateError::ArithmeticOverflow)?;
            (0, Some(adjustment), payable)
        }
        ClaimCloseMode::Bankrupt => {
            let primary_loss = match primary_pnl {
                OracleCashPnl::Loss(loss) if loss >= collateral => loss,
                OracleCashPnl::Loss(_) | OracleCashPnl::Flat | OracleCashPnl::Profit(_) => {
                    return Err(StateError::PositionNotBankrupt)
                }
            };
            if let Some(exit_price) = secondary_exit_price_e6 {
                let secondary_loss = match oracle_cash_pnl(
                    side,
                    position.notional.get(),
                    position.secondary_entry_price_e6.get(),
                    exit_price,
                )? {
                    OracleCashPnl::Loss(loss) => loss,
                    OracleCashPnl::Flat | OracleCashPnl::Profit(_) => {
                        return Err(StateError::PositionNotBankrupt)
                    }
                };
                if secondary_loss < collateral {
                    return Err(StateError::PositionNotBankrupt);
                }
            }
            (primary_loss, None, 0)
        }
    };

    let represented_before = pool.represented_quote_custody()?;
    let observed = u128::from(observed_vault_balance);
    if observed < represented_before {
        return Err(StateError::CustodyShortfall);
    }
    let surplus_recorded = observed - represented_before;
    let predecessor_blocks =
        pool.total_queued.get() != 0 || pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED != 0;
    let immediate_payout = if predecessor_blocks {
        0
    } else {
        core::cmp::min(pool.free_cash.get(), payable_before_cash)
    };
    let queued_payout = payable_before_cash
        .checked_sub(immediate_payout)
        .ok_or(StateError::ArithmeticOverflow)?;
    if queued_payout == 0 {
        if child_claim_key.is_some() || current_tail.is_some() || rollover_tail.is_some() {
            return Err(StateError::SettlementShapeMismatch);
        }
    } else if child_claim_key.is_none() || current_tail.is_none() {
        return Err(StateError::SettlementShapeMismatch);
    }

    let claim_loss_extinguished = core::cmp::min(gross_loss, collateral);
    let residual_bad_debt_added = gross_loss.saturating_sub(collateral);
    let mut next_pool = *pool;
    let mut next_market = *market;
    let mut next_controller = *controller;
    let mut next_source_claim = *source_claim;
    let mut next_position = *position;
    let mut next_current_tail = current_tail
        .as_ref()
        .map(|page| boxed_queue_page_copy(page));
    let mut next_rollover_tail = rollover_tail
        .as_ref()
        .map(|page| boxed_queue_page_copy(page));

    next_pool.unallocated_vault_surplus = U128Le::new(
        next_pool
            .unallocated_vault_surplus
            .get()
            .checked_add(surplus_recorded)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.open_claim_collateral = U128Le::new(
        next_pool
            .open_claim_collateral
            .get()
            .checked_sub(collateral)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.reserved_open_profit = U128Le::new(
        next_pool
            .reserved_open_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.free_cash = U128Le::new(
        next_pool
            .free_cash
            .get()
            .checked_sub(immediate_payout)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.idle_cash_collateral = U128Le::new(
        next_pool
            .idle_cash_collateral
            .get()
            .checked_add(immediate_payout)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.residual_bad_debt = U128Le::new(
        next_pool
            .residual_bad_debt
            .get()
            .checked_add(residual_bad_debt_added)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    if let Some(adjustment) = adjustment {
        next_pool.fee_accrual = U128Le::new(
            next_pool
                .fee_accrual
                .get()
                .saturating_add(adjustment.win_fee),
        );
    }

    let market_open_interest = match side {
        PositionSide::Long => &mut next_market.long_open_interest,
        PositionSide::Short => &mut next_market.short_open_interest,
    };
    *market_open_interest = U128Le::new(
        market_open_interest
            .get()
            .checked_sub(position.notional.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_market.reserved_profit = U128Le::new(
        next_market
            .reserved_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.reserved_profit = U128Le::new(
        next_controller
            .reserved_profit
            .get()
            .checked_sub(position.reserved_profit.get())
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.claim_collateral_in_use = U128Le::new(
        next_controller
            .claim_collateral_in_use
            .get()
            .checked_sub(collateral)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.open_positions = U32Le::new(
        next_controller
            .open_positions
            .get()
            .checked_sub(1)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.available_cash_collateral = U128Le::new(
        next_controller
            .available_cash_collateral
            .get()
            .checked_add(immediate_payout)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_source_claim.collateral_in_use = U128Le::new(
        next_source_claim
            .collateral_in_use
            .get()
            .checked_sub(collateral)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    let source_still_active = next_source_claim.remaining_face.get() != 0
        || next_source_claim.collateral_in_use.get() != 0;
    next_source_claim.status = match (
        next_source_claim.remaining_face.get(),
        next_source_claim.collateral_in_use.get(),
    ) {
        (0, 0) if payable_before_cash == 0 => ClaimStatus::Extinguished as u8,
        (0, 0) => ClaimStatus::Settled as u8,
        (0, _) => ClaimStatus::FundingPosition as u8,
        (_, 0) => ClaimStatus::PartiallyPaid as u8,
        (_, _) => ClaimStatus::QueuedAndFunding as u8,
    };
    if !source_still_active {
        next_controller.active_claims = U32Le::new(
            next_controller
                .active_claims
                .get()
                .checked_sub(1)
                .ok_or(StateError::ArithmeticOverflow)?,
        );
    }
    next_position.status = PositionStatus::Closed as u8;
    next_position.reserved_profit = U128Le::new(0);

    let mut child_claim = None;
    if queued_payout != 0 {
        let child_claim_key = child_claim_key.ok_or(StateError::SettlementShapeMismatch)?;
        let current = next_current_tail
            .as_deref_mut()
            .ok_or(StateError::SettlementShapeMismatch)?;
        let (append, page_index) = if current.is_full() {
            let rollover = next_rollover_tail
                .as_deref_mut()
                .ok_or(StateError::SettlementShapeMismatch)?;
            link_next_queue_page(pool_key, &mut next_pool, current, rollover)?;
            let outcome = append_claim(
                pool_key,
                &mut next_pool,
                rollover,
                child_claim_key,
                queued_payout,
            )?;
            (outcome, rollover.page_index.get())
        } else {
            if next_rollover_tail.is_some() {
                return Err(StateError::SettlementShapeMismatch);
            }
            let page_index = current.page_index.get();
            let outcome = append_claim(
                pool_key,
                &mut next_pool,
                current,
                child_claim_key,
                queued_payout,
            )?;
            (outcome, page_index)
        };
        let next_claim = ClaimState::new_from_claim(
            pool_key,
            controller_key,
            position.position_id.get(),
            position.market_id.get(),
            page_index,
            append.slot,
            append.sequence,
            source_claim.sequence.get(),
            queued_payout,
        )?;
        next_controller.queued_claim_face = U128Le::new(
            next_controller
                .queued_claim_face
                .get()
                .checked_add(queued_payout)
                .ok_or(StateError::ArithmeticOverflow)?,
        );
        next_controller.active_claims = U32Le::new(
            next_controller
                .active_claims
                .get()
                .checked_add(1)
                .ok_or(StateError::ArithmeticOverflow)?,
        );
        child_claim = Some(next_claim);
    } else if next_rollover_tail.is_some() {
        return Err(StateError::SettlementShapeMismatch);
    }

    next_pool.validate()?;
    next_market.validate()?;
    next_controller.validate()?;
    next_source_claim.validate()?;
    next_position.validate()?;
    if let Some(page) = next_current_tail.as_deref() {
        page.validate()?;
    }
    if let Some(page) = next_rollover_tail.as_deref() {
        page.validate()?;
    }
    if let Some(claim) = child_claim {
        claim.validate()?;
    }
    let represented_after = next_pool.represented_quote_custody()?;
    if represented_after != observed {
        return Err(StateError::InvalidState);
    }

    *pool = next_pool;
    *market = next_market;
    *controller = next_controller;
    *source_claim = next_source_claim;
    *position = next_position;
    if let (Some(destination), Some(value)) = (current_tail, next_current_tail) {
        *destination = *value;
    }
    if let (Some(destination), Some(value)) = (rollover_tail, next_rollover_tail) {
        *destination = *value;
    }
    Ok(ClaimCloseOutcome {
        immediate_payout: u64::try_from(immediate_payout)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        queued_payout,
        child_claim,
        gross_loss,
        claim_loss_extinguished,
        residual_bad_debt_added,
        adjustment,
        surplus_recorded: u64::try_from(surplus_recorded)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
        represented_custody_after: u64::try_from(represented_after)
            .map_err(|_| StateError::CustodyCapacityExceeded)?,
    })
}

pub fn append_claim(
    pool_key: Key32,
    pool: &mut PoolState,
    tail_page: &mut QueuePageState,
    claim_key: Key32,
    face: u128,
) -> StateResult<AppendOutcome> {
    pool.validate()?;
    tail_page.validate()?;
    if claim_key.is_zero() || face == 0 {
        return Err(StateError::InvalidState);
    }
    if tail_page.pool != pool_key || tail_page.page_index.get() != pool.tail_page_index.get() {
        return Err(StateError::BrokenPageLink);
    }
    if pool.free_cash.get() != 0 {
        return Err(StateError::InvalidState);
    }
    if tail_page.next_page_index.get() != NO_PAGE_INDEX {
        return Err(StateError::BrokenPageLink);
    }
    if tail_page.is_full() {
        return Err(StateError::QueueFull);
    }

    let sequence = pool.next_claim_sequence.get();
    if tail_page.next_append_sequence.get() != sequence {
        return Err(StateError::SequenceMismatch);
    }
    let slot = usize::from(tail_page.len.get());
    if !tail_page.entries[slot].is_zero() {
        return Err(StateError::InvalidState);
    }

    let mut next_pool = *pool;
    let mut next_page = *tail_page;
    next_page.entries[slot] = QueueEntry {
        claim: claim_key,
        sequence: U128Le::new(sequence),
        remaining_face: U128Le::new(face),
    };
    next_page.len = U16Le::new(
        next_page
            .len
            .get()
            .checked_add(1)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_page.next_append_sequence = U128Le::new(
        sequence
            .checked_add(1)
            .filter(|value| *value != NO_CLAIM_SEQUENCE)
            .ok_or(StateError::ArithmeticOverflow)?,
    );

    next_pool.total_queued = U128Le::new(
        next_pool
            .total_queued
            .get()
            .checked_add(face)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    if pool.total_queued.get() == 0 && pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED == 0 {
        next_pool.head_claim_sequence = U128Le::new(sequence);
        next_pool.head_page_index = tail_page.page_index;
    }
    next_pool.tail_claim_sequence = U128Le::new(sequence);
    next_pool.next_claim_sequence = next_page.next_append_sequence;

    next_pool.validate()?;
    next_page.validate()?;
    *pool = next_pool;
    *tail_page = next_page;
    Ok(AppendOutcome {
        sequence,
        slot: slot as u16,
    })
}

pub fn link_next_queue_page(
    pool_key: Key32,
    pool: &mut PoolState,
    current_tail: &mut QueuePageState,
    new_tail: &QueuePageState,
) -> StateResult<()> {
    pool.validate()?;
    current_tail.validate()?;
    new_tail.validate()?;
    if current_tail.pool != pool_key
        || new_tail.pool != pool_key
        || current_tail.page_index.get() != pool.tail_page_index.get()
        || !current_tail.is_full()
        || current_tail.next_page_index.get() != NO_PAGE_INDEX
        || new_tail.page_index.get() != pool.next_page_index.get()
        || new_tail.previous_page_index.get() != current_tail.page_index.get()
        || new_tail.next_page_index.get() != NO_PAGE_INDEX
        || new_tail.head_offset.get() != 0
        || new_tail.len.get() != 0
        || new_tail.first_sequence.get() != pool.next_claim_sequence.get()
        || new_tail.next_append_sequence.get() != pool.next_claim_sequence.get()
    {
        return Err(StateError::BrokenPageLink);
    }

    let mut next_pool = *pool;
    let mut next_current = *current_tail;
    next_current.next_page_index = new_tail.page_index;
    next_pool.tail_page_index = new_tail.page_index;
    next_pool.next_page_index = U64Le::new(
        new_tail
            .page_index
            .get()
            .checked_add(1)
            .filter(|index| *index != NO_PAGE_INDEX)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.validate()?;
    next_current.validate()?;
    *pool = next_pool;
    *current_tail = next_current;
    Ok(())
}

pub fn pay_fifo_head(
    pool_key: Key32,
    pool: &mut PoolState,
    head_page: &mut QueuePageState,
    max_payment: u128,
) -> StateResult<PaymentOutcome> {
    pool.validate()?;
    head_page.validate()?;
    if max_payment == 0 {
        return Err(StateError::InvalidState);
    }
    if pool.total_queued.get() == 0 {
        return Err(StateError::QueueEmpty);
    }
    if pool.claim_service_cash.get() == 0 {
        return Err(StateError::InsufficientClaimServiceCash);
    }
    if head_page.pool != pool_key || head_page.page_index.get() != pool.head_page_index.get() {
        return Err(StateError::BrokenPageLink);
    }
    let slot = usize::from(head_page.head_offset.get());
    if slot >= usize::from(head_page.len.get()) {
        return Err(StateError::QueueEmpty);
    }
    let entry = head_page.entries[slot];
    if entry.sequence.get() != pool.head_claim_sequence.get() {
        return Err(StateError::SequenceMismatch);
    }
    let face = entry.remaining_face.get();
    if face == 0 {
        return Err(StateError::HeadNotSettled);
    }
    let paid = face.min(max_payment).min(pool.claim_service_cash.get());
    let remaining = face - paid;

    let mut next_pool = *pool;
    let mut next_page = *head_page;
    next_page.entries[slot].remaining_face = U128Le::new(remaining);
    next_pool.total_queued = U128Le::new(
        next_pool
            .total_queued
            .get()
            .checked_sub(paid)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.claim_service_cash = U128Le::new(
        next_pool
            .claim_service_cash
            .get()
            .checked_sub(paid)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    if remaining == 0 {
        next_pool.queue_flags |= QUEUE_FLAG_HEAD_SETTLED;
    }
    next_pool.validate()?;
    next_page.validate()?;
    *pool = next_pool;
    *head_page = next_page;
    Ok(PaymentOutcome {
        sequence: entry.sequence.get(),
        paid,
        remaining_face: remaining,
    })
}

pub fn service_fifo_claim(
    pool_key: Key32,
    pool: &mut PoolState,
    controller: &mut ControllerRiskState,
    head_page: &mut QueuePageState,
    claim_key: Key32,
    claim: &mut ClaimState,
) -> StateResult<PaymentOutcome> {
    pool.validate()?;
    controller.validate()?;
    head_page.validate()?;
    claim.validate()?;
    let status = ClaimStatus::try_from(claim.status)?;
    if !matches!(
        status,
        ClaimStatus::Queued | ClaimStatus::PartiallyPaid | ClaimStatus::QueuedAndFunding
    ) || claim.remaining_face.get() == 0
    {
        return Err(StateError::InvalidState);
    }
    if controller.pool != pool_key
        || controller.controller != claim.controller
        || claim.pool != pool_key
        || head_page.pool != pool_key
        || head_page.page_index.get() != pool.head_page_index.get()
        || claim.queue_page_index.get() != head_page.page_index.get()
        || claim.sequence != pool.head_claim_sequence
    {
        return Err(StateError::ClaimMismatch);
    }
    let slot = usize::from(claim.queue_slot.get());
    if slot != usize::from(head_page.head_offset.get()) || slot >= usize::from(head_page.len.get())
    {
        return Err(StateError::ClaimMismatch);
    }
    let entry = head_page.entries[slot];
    if entry.claim != claim_key
        || entry.sequence != claim.sequence
        || entry.remaining_face != claim.remaining_face
    {
        return Err(StateError::ClaimMismatch);
    }
    let claim_face = claim.remaining_face.get();
    let claim_collateral = claim.collateral_in_use.get();
    if pool.total_queued.get() < claim_face
        || controller.queued_claim_face.get() < claim_face
        || controller.active_claims.get() == 0
        || pool.open_claim_collateral.get() < claim_collateral
        || controller.claim_collateral_in_use.get() < claim_collateral
    {
        return Err(StateError::ClaimMismatch);
    }

    let mut next_pool = *pool;
    let mut next_controller = *controller;
    let mut next_page = *head_page;
    let mut next_claim = *claim;
    let outcome = pay_fifo_head(pool_key, &mut next_pool, &mut next_page, u128::MAX)?;
    next_controller.queued_claim_face = U128Le::new(
        next_controller
            .queued_claim_face
            .get()
            .checked_sub(outcome.paid)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.idle_cash_collateral = U128Le::new(
        next_pool
            .idle_cash_collateral
            .get()
            .checked_add(outcome.paid)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.available_cash_collateral = U128Le::new(
        next_controller
            .available_cash_collateral
            .get()
            .checked_add(outcome.paid)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_claim.remaining_face = U128Le::new(outcome.remaining_face);
    next_claim.status = match (outcome.remaining_face, next_claim.collateral_in_use.get()) {
        (0, 0) => {
            next_controller.active_claims = U32Le::new(
                next_controller
                    .active_claims
                    .get()
                    .checked_sub(1)
                    .ok_or(StateError::ArithmeticOverflow)?,
            );
            ClaimStatus::Settled as u8
        }
        (0, _) => ClaimStatus::FundingPosition as u8,
        (_, 0) => ClaimStatus::PartiallyPaid as u8,
        (_, _) => ClaimStatus::QueuedAndFunding as u8,
    };

    next_pool.validate()?;
    next_controller.validate()?;
    next_page.validate()?;
    next_claim.validate()?;
    *pool = next_pool;
    *controller = next_controller;
    *head_page = next_page;
    *claim = next_claim;
    Ok(outcome)
}

pub fn pop_settled_fifo_head(
    pool_key: Key32,
    pool: &mut PoolState,
    head_page: &mut QueuePageState,
    next_page: Option<&QueuePageState>,
) -> StateResult<u128> {
    pool.validate()?;
    head_page.validate()?;
    if head_page.pool != pool_key || head_page.page_index.get() != pool.head_page_index.get() {
        return Err(StateError::BrokenPageLink);
    }
    let slot = usize::from(head_page.head_offset.get());
    if slot >= usize::from(head_page.len.get()) {
        return Err(StateError::QueueEmpty);
    }
    let entry = head_page.entries[slot];
    if entry.sequence.get() != pool.head_claim_sequence.get() {
        return Err(StateError::SequenceMismatch);
    }
    if entry.remaining_face.get() != 0 {
        return Err(StateError::HeadNotSettled);
    }
    if pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED == 0 {
        return Err(StateError::InvalidState);
    }

    let mut next_pool = *pool;
    let mut next_head = *head_page;
    next_head.entries[slot] = QueueEntry::default();
    next_head.head_offset = U16Le::new(
        next_head
            .head_offset
            .get()
            .checked_add(1)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.queue_flags &= !QUEUE_FLAG_HEAD_SETTLED;

    if next_head.head_offset.get() < next_head.len.get() {
        let following_entry = next_head.entries[usize::from(next_head.head_offset.get())];
        next_pool.head_claim_sequence = following_entry.sequence;
        if following_entry.remaining_face.get() == 0 {
            next_pool.queue_flags |= QUEUE_FLAG_HEAD_SETTLED;
        }
    } else if next_head.next_page_index.get() != NO_PAGE_INDEX {
        let following = next_page.ok_or(StateError::BrokenPageLink)?;
        validate_following_page(pool_key, &next_head, following)?;
        next_pool.head_page_index = following.page_index;
        if following.active_len() == 0 {
            if next_pool.total_queued.get() != 0 {
                return Err(StateError::QueueEmpty);
            }
            next_pool.head_claim_sequence = U128Le::new(NO_CLAIM_SEQUENCE);
            next_pool.tail_claim_sequence = U128Le::new(NO_CLAIM_SEQUENCE);
        } else {
            let following_entry = following.entries[usize::from(following.head_offset.get())];
            if following_entry.sequence.get()
                != entry
                    .sequence
                    .get()
                    .checked_add(1)
                    .ok_or(StateError::ArithmeticOverflow)?
            {
                return Err(StateError::SequenceMismatch);
            }
            next_pool.head_claim_sequence = following_entry.sequence;
            if following_entry.remaining_face.get() == 0 {
                next_pool.queue_flags |= QUEUE_FLAG_HEAD_SETTLED;
            }
        }
    } else if next_pool.total_queued.get() == 0 {
        next_pool.head_claim_sequence = U128Le::new(NO_CLAIM_SEQUENCE);
        next_pool.tail_claim_sequence = U128Le::new(NO_CLAIM_SEQUENCE);
    } else {
        return Err(StateError::BrokenPageLink);
    }

    if next_pool.total_queued.get() == 0 && next_pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED == 0 {
        next_pool.free_cash = U128Le::new(
            next_pool
                .free_cash
                .get()
                .checked_add(next_pool.claim_service_cash.get())
                .ok_or(StateError::ArithmeticOverflow)?,
        );
        next_pool.claim_service_cash = U128Le::new(0);
    }

    next_pool.validate()?;
    next_head.validate()?;
    *pool = next_pool;
    *head_page = next_head;
    Ok(entry.sequence.get())
}

pub fn pop_fifo_head_with_claim(
    pool_key: Key32,
    pool: &mut PoolState,
    head_page: &mut QueuePageState,
    claim_key: Key32,
    claim: &ClaimState,
    next_page: Option<&QueuePageState>,
) -> StateResult<u128> {
    pool.validate()?;
    head_page.validate()?;
    claim.validate()?;
    if claim.pool != pool_key
        || head_page.pool != pool_key
        || head_page.page_index.get() != pool.head_page_index.get()
        || claim.queue_page_index.get() != head_page.page_index.get()
        || claim.sequence != pool.head_claim_sequence
        || claim.remaining_face.get() != 0
        || !matches!(
            ClaimStatus::try_from(claim.status)?,
            ClaimStatus::Settled | ClaimStatus::FundingPosition | ClaimStatus::Extinguished
        )
    {
        return Err(StateError::ClaimMismatch);
    }
    let slot = usize::from(claim.queue_slot.get());
    if slot != usize::from(head_page.head_offset.get()) || slot >= usize::from(head_page.len.get())
    {
        return Err(StateError::ClaimMismatch);
    }
    let entry = head_page.entries[slot];
    if entry.claim != claim_key
        || entry.sequence != claim.sequence
        || entry.remaining_face.get() != 0
    {
        return Err(StateError::ClaimMismatch);
    }

    let mut next_pool = *pool;
    let mut next_head = *head_page;
    let sequence = pop_settled_fifo_head(pool_key, &mut next_pool, &mut next_head, next_page)?;
    if sequence != claim.sequence.get() {
        return Err(StateError::SequenceMismatch);
    }
    next_pool.validate()?;
    next_head.validate()?;
    *pool = next_pool;
    *head_page = next_head;
    Ok(sequence)
}

#[allow(clippy::too_many_arguments)]
pub fn consume_claim_for_position(
    pool_key: Key32,
    pool: &mut PoolState,
    controller: &mut ControllerRiskState,
    page: &mut QueuePageState,
    claim_key: Key32,
    claim: &mut ClaimState,
    amount: u128,
) -> StateResult<ClaimCollateralOutcome> {
    pool.validate()?;
    controller.validate()?;
    page.validate()?;
    claim.validate()?;
    if amount == 0 || amount > claim.remaining_face.get() {
        return Err(StateError::InvalidState);
    }
    if page.pool != pool_key
        || claim.pool != pool_key
        || controller.pool != pool_key
        || controller.controller != claim.controller
        || claim.queue_page_index.get() != page.page_index.get()
    {
        return Err(StateError::ClaimMismatch);
    }
    if !matches!(
        ClaimStatus::try_from(claim.status)?,
        ClaimStatus::Queued | ClaimStatus::PartiallyPaid | ClaimStatus::QueuedAndFunding
    ) {
        return Err(StateError::InvalidState);
    }
    let slot = usize::from(claim.queue_slot.get());
    if slot < usize::from(page.head_offset.get()) || slot >= usize::from(page.len.get()) {
        return Err(StateError::ClaimMismatch);
    }
    let entry = page.entries[slot];
    if entry.claim != claim_key
        || entry.sequence != claim.sequence
        || entry.remaining_face != claim.remaining_face
    {
        return Err(StateError::ClaimMismatch);
    }

    let remaining = claim
        .remaining_face
        .get()
        .checked_sub(amount)
        .ok_or(StateError::ArithmeticOverflow)?;
    let collateral_in_use = claim
        .collateral_in_use
        .get()
        .checked_add(amount)
        .ok_or(StateError::ArithmeticOverflow)?;
    let mut next_pool = *pool;
    let mut next_controller = *controller;
    let mut next_page = *page;
    let mut next_claim = *claim;
    next_pool.total_queued = U128Le::new(
        next_pool
            .total_queued
            .get()
            .checked_sub(amount)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_pool.open_claim_collateral = U128Le::new(
        next_pool
            .open_claim_collateral
            .get()
            .checked_add(amount)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.queued_claim_face = U128Le::new(
        next_controller
            .queued_claim_face
            .get()
            .checked_sub(amount)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_controller.claim_collateral_in_use = U128Le::new(
        next_controller
            .claim_collateral_in_use
            .get()
            .checked_add(amount)
            .ok_or(StateError::ArithmeticOverflow)?,
    );
    next_page.entries[slot].remaining_face = U128Le::new(remaining);
    next_claim.remaining_face = U128Le::new(remaining);
    next_claim.collateral_in_use = U128Le::new(collateral_in_use);
    next_claim.status = if remaining == 0 {
        ClaimStatus::FundingPosition as u8
    } else {
        ClaimStatus::QueuedAndFunding as u8
    };
    if entry.sequence == next_pool.head_claim_sequence && remaining == 0 {
        next_pool.queue_flags |= QUEUE_FLAG_HEAD_SETTLED;
    }

    next_pool.validate()?;
    next_controller.validate()?;
    next_page.validate()?;
    next_claim.validate()?;
    *pool = next_pool;
    *controller = next_controller;
    *page = next_page;
    *claim = next_claim;
    Ok(ClaimCollateralOutcome {
        sequence: entry.sequence.get(),
        consumed: amount,
        remaining_at_priority: remaining,
        collateral_in_use,
    })
}

fn validate_following_page(
    pool_key: Key32,
    current: &QueuePageState,
    following: &QueuePageState,
) -> StateResult<()> {
    following.validate()?;
    if current.next_page_index.get() != following.page_index.get()
        || following.previous_page_index.get() != current.page_index.get()
        || following.pool != pool_key
    {
        return Err(StateError::BrokenPageLink);
    }
    Ok(())
}

fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| *byte == 0)
}

pub const POOL_ACCOUNT_LEN: usize = account_len::<PoolState>();
pub const MARKET_RISK_ACCOUNT_LEN: usize = account_len::<MarketRiskState>();
pub const ORACLE_POLICY_ACCOUNT_LEN: usize = account_len::<OraclePolicyState>();
pub const ORACLE_RELAY_ACCOUNT_LEN: usize = account_len::<OracleRelayState>();
pub const CONTROLLER_RISK_ACCOUNT_LEN: usize = account_len::<ControllerRiskState>();
pub const TRADING_SESSION_ACCOUNT_LEN: usize = account_len::<TradingSessionState>();
pub const POSITION_ACCOUNT_LEN: usize = account_len::<PositionState>();
pub const CLAIM_ACCOUNT_LEN: usize = account_len::<ClaimState>();
pub const QUEUE_PAGE_ACCOUNT_LEN: usize = account_len::<QueuePageState>();
