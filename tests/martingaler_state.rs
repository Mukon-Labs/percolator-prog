use core::mem::size_of;
use percolator_prog::martingaler_state::{
    account_len, append_claim, close_cash_position, close_nonpositive_claim_position,
    close_profitable_cash_position, close_profitable_claim_position, consume_claim_for_position,
    decode_account, decode_account_header, initialize_account, initialize_queue_page_zero_account,
    link_next_queue_page, open_cash_position_from_idle, open_claim_position_from_queue,
    oracle_cash_pnl, overwrite_account, pay_fifo_head, pop_fifo_head_with_claim,
    pop_settled_fifo_head, record_cash_deposit, resolve_bankrupt_claim_position,
    service_fifo_claim, transition_market_status, transition_pool_status,
    validate_queue_page_zero_account, withdraw_idle_cash, ClaimState, ClaimStatus,
    ControllerRiskState, FundingKind, HaircutConfig, I64Le, Key32, MarketRiskState,
    MartingalerAccount, OracleCashPnl, OraclePolicyState, PoolState, PoolStatus, PositionSide,
    PositionState, PositionStatus, QueueEntry, QueuePageState, StateError, U128Le, U16Le, U32Le,
    U64Le, ACCOUNT_HEADER_LEN, CLAIM_ACCOUNT_LEN, CONTROLLER_RISK_ACCOUNT_LEN, KIND_POOL,
    MARKET_RISK_ACCOUNT_LEN, MARTINGALER_MAGIC, MARTINGALER_VERSION, NO_CLAIM_SEQUENCE,
    NO_PAGE_INDEX, ORACLE_POLICY_ACCOUNT_LEN, POOL_ACCOUNT_LEN, POSITION_ACCOUNT_LEN,
    QUEUE_FLAG_HEAD_SETTLED, QUEUE_PAGE_ACCOUNT_LEN, QUEUE_PAGE_CAPACITY,
};

fn key(byte: u8) -> Key32 {
    Key32::new([byte; 32])
}

fn pool() -> PoolState {
    PoolState::new(77, key(1), key(2), key(3))
}

fn haircut() -> HaircutConfig {
    HaircutConfig::new(100_000, 20, 2_000_000, 4_000_000, 100_000, 20_000)
}

fn market(pool_key: Key32) -> MarketRiskState {
    MarketRiskState {
        pool: pool_key,
        market_id: U16Le::new(1),
        status: PoolStatus::Active as u8,
        reserved_control: [0; 1],
        max_leverage_x: U16Le::new(100),
        divergence_limit_bps: U16Le::new(500),
        max_position_notional: U128Le::new(1_000_000),
        max_net_profit_per_position: U128Le::new(100_000),
        max_long_open_interest: U128Le::new(2_000_000),
        max_short_open_interest: U128Le::new(2_000_000),
        long_open_interest: U128Le::new(0),
        short_open_interest: U128Le::new(0),
        reserved_profit: U128Le::new(0),
        haircut: haircut(),
        reserved: [0; 8],
    }
}

fn controller(pool_key: Key32) -> ControllerRiskState {
    ControllerRiskState {
        pool: pool_key,
        controller: key(4),
        available_cash_collateral: U128Le::new(0),
        reserved_profit: U128Le::new(100_000),
        queued_claim_face: U128Le::new(0),
        claim_collateral_in_use: U128Le::new(0),
        open_positions: U32Le::new(1),
        active_claims: U32Le::new(0),
        reserved: [0; 16],
    }
}

fn oracle_policy(pool_key: Key32) -> OraclePolicyState {
    OraclePolicyState::new(pool_key, key(8), key(10), key(11), 30, 100).unwrap()
}

fn cash_position(pool_key: Key32) -> PositionState {
    PositionState {
        pool: pool_key,
        controller: key(4),
        position_id: U64Le::new(9),
        market_id: U16Le::new(1),
        side: PositionSide::Long as u8,
        funding_kind: FundingKind::Cash as u8,
        status: PositionStatus::Open as u8,
        reserved_control: [0; 1],
        leverage_x: U16Le::new(100),
        source_claim_sequence: U128Le::new(NO_CLAIM_SEQUENCE),
        collateral: U128Le::new(10_000),
        notional: U128Le::new(1_000_000),
        max_payable_profit: U128Le::new(100_000),
        reserved_profit: U128Le::new(100_000),
        entry_price_e6: U64Le::new(100_000_000),
        entry_publish_time: I64Le::new(1_000),
        secondary_entry_price_e6: U64Le::new(100_100_000),
        secondary_publish_time: I64Le::new(1_000),
    }
}

fn cash_claim(pool_key: Key32) -> ClaimState {
    ClaimState {
        pool: pool_key,
        controller: key(4),
        origin_position_id: U64Le::new(9),
        origin_market_id: U16Le::new(1),
        status: ClaimStatus::Queued as u8,
        funding_kind: FundingKind::Cash as u8,
        queue_slot: U16Le::new(0),
        reserved_control: [0; 2],
        queue_page_index: U64Le::new(0),
        sequence: U128Le::new(1),
        source_claim_sequence: U128Le::new(NO_CLAIM_SEQUENCE),
        original_face: U128Le::new(50_000),
        remaining_face: U128Le::new(50_000),
        collateral_in_use: U128Le::new(0),
        reserved: [0; 32],
    }
}

fn round_trip<T>(body: T)
where
    T: MartingalerAccount + core::fmt::Debug + Eq,
{
    let mut data = vec![0u8; account_len::<T>()];
    initialize_account(&mut data, 254, &body).unwrap();
    assert_eq!(&data[..8], &MARTINGALER_MAGIC);
    assert_eq!(
        u16::from_le_bytes(data[8..10].try_into().unwrap()),
        MARTINGALER_VERSION
    );
    assert_eq!(decode_account::<T>(&data).unwrap(), body);
    overwrite_account(&mut data, &body).unwrap();
    assert_eq!(decode_account::<T>(&data).unwrap(), body);
    assert_eq!(
        initialize_account(&mut data, 254, &body),
        Err(StateError::AlreadyInitialized)
    );
}

#[test]
fn every_account_kind_has_an_exact_round_trip_layout() {
    let pool_key = key(9);
    let pool = pool();
    let market = market(pool_key);
    let oracle_policy = oracle_policy(pool_key);
    let controller = controller(pool_key);
    let position = cash_position(pool_key);
    let claim = cash_claim(pool_key);
    let page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();

    assert_eq!(ACCOUNT_HEADER_LEN, 16);
    assert_eq!(size_of::<QueueEntry>(), 64);
    assert_eq!(POOL_ACCOUNT_LEN, 424);
    assert_eq!(MARKET_RISK_ACCOUNT_LEN, 200);
    assert_eq!(ORACLE_POLICY_ACCOUNT_LEN, 160);
    assert_eq!(CONTROLLER_RISK_ACCOUNT_LEN, 168);
    assert_eq!(POSITION_ACCOUNT_LEN, 208);
    assert_eq!(CLAIM_ACCOUNT_LEN, 216);
    assert_eq!(QUEUE_PAGE_ACCOUNT_LEN, 2_160);
    assert_eq!(POOL_ACCOUNT_LEN, account_len::<PoolState>());
    assert_eq!(MARKET_RISK_ACCOUNT_LEN, account_len::<MarketRiskState>());
    assert_eq!(
        ORACLE_POLICY_ACCOUNT_LEN,
        account_len::<OraclePolicyState>()
    );
    assert_eq!(
        CONTROLLER_RISK_ACCOUNT_LEN,
        account_len::<ControllerRiskState>()
    );
    assert_eq!(POSITION_ACCOUNT_LEN, account_len::<PositionState>());
    assert_eq!(CLAIM_ACCOUNT_LEN, account_len::<ClaimState>());
    assert_eq!(QUEUE_PAGE_ACCOUNT_LEN, account_len::<QueuePageState>());

    round_trip(pool);
    round_trip(market);
    round_trip(oracle_policy);
    round_trip(controller);
    round_trip(position);
    round_trip(claim);
    round_trip(page);
}

#[test]
fn in_place_page_zero_initialization_preserves_layout_and_checks_bump() {
    let pool_key = key(9);
    let mut data = vec![0u8; QUEUE_PAGE_ACCOUNT_LEN];
    initialize_queue_page_zero_account(&mut data, 203, pool_key).unwrap();
    validate_queue_page_zero_account(&data, 203, pool_key).unwrap();
    assert_eq!(
        decode_account_header::<QueuePageState>(&data).unwrap().bump,
        203
    );
    assert_eq!(
        decode_account::<QueuePageState>(&data).unwrap(),
        QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap()
    );
    assert_eq!(
        validate_queue_page_zero_account(&data, 202, pool_key),
        Err(StateError::InvalidState)
    );
    assert_eq!(
        initialize_queue_page_zero_account(&mut data, 203, pool_key),
        Err(StateError::AlreadyInitialized)
    );
}

#[test]
fn registration_roots_copy_pool_limits_and_start_with_zero_accounting() {
    let pool_key = key(9);
    let pool = pool();
    let market = MarketRiskState::new(
        pool_key,
        &pool,
        7,
        500,
        pool.total_liability_open_gate.get(),
        pool.total_liability_open_gate.get(),
        haircut(),
    )
    .unwrap();
    assert_eq!(market.pool, pool_key);
    assert_eq!(market.market_id.get(), 7);
    assert_eq!(market.status, PoolStatus::Active as u8);
    assert_eq!(market.max_leverage_x, pool.max_leverage_x);
    assert_eq!(market.max_position_notional, pool.max_position_notional);
    assert_eq!(
        market.max_net_profit_per_position,
        pool.max_net_profit_per_position
    );
    assert_eq!(market.divergence_limit_bps.get(), 500);
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.short_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    market.validate().unwrap();

    let controller_key = key(4);
    let controller = ControllerRiskState::new(pool_key, controller_key).unwrap();
    assert_eq!(controller.pool, pool_key);
    assert_eq!(controller.controller, controller_key);
    assert_eq!(controller.available_cash_collateral.get(), 0);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.queued_claim_face.get(), 0);
    assert_eq!(controller.claim_collateral_in_use.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);
    controller.validate().unwrap();
    assert_eq!(pool.idle_cash_collateral.get(), 0);
    assert_eq!(pool.unallocated_vault_surplus.get(), 0);
    assert_eq!(pool.represented_quote_custody().unwrap(), 0);

    assert_eq!(
        MarketRiskState::new(pool_key, &pool, 0, 500, 1, 1, haircut()),
        Err(StateError::InvalidState)
    );
    assert_eq!(
        MarketRiskState::new(pool_key, &pool, 7, 0, 1, 1, haircut()),
        Err(StateError::InvalidState)
    );
    assert_eq!(
        MarketRiskState::new(
            pool_key,
            &pool,
            7,
            500,
            pool.total_liability_open_gate.get() + 1,
            1,
            haircut(),
        ),
        Err(StateError::InvalidState)
    );
    assert_eq!(
        MarketRiskState::new(pool_key, &pool, 7, 500, 1, 0, haircut()),
        Err(StateError::InvalidState)
    );
    assert_eq!(
        ControllerRiskState::new(Key32::ZERO, controller_key),
        Err(StateError::InvalidState)
    );
    assert_eq!(
        ControllerRiskState::new(pool_key, Key32::ZERO),
        Err(StateError::InvalidState)
    );
}

#[test]
fn malformed_header_length_and_reserved_bytes_fail_closed() {
    let body = pool();
    let mut data = vec![0u8; POOL_ACCOUNT_LEN];
    initialize_account(&mut data, 1, &body).unwrap();

    let mut wrong_magic = data.clone();
    wrong_magic[0] ^= 1;
    assert_eq!(
        decode_account::<PoolState>(&wrong_magic),
        Err(StateError::InvalidMagic)
    );

    let mut wrong_version = data.clone();
    wrong_version[8..10].copy_from_slice(&(MARTINGALER_VERSION + 1).to_le_bytes());
    assert_eq!(
        decode_account::<PoolState>(&wrong_version),
        Err(StateError::InvalidVersion)
    );

    let mut wrong_kind = data.clone();
    wrong_kind[10] = KIND_POOL + 1;
    assert_eq!(
        decode_account::<PoolState>(&wrong_kind),
        Err(StateError::InvalidKind)
    );

    let mut header_reserved = data.clone();
    header_reserved[11] = 1;
    assert_eq!(
        decode_account::<PoolState>(&header_reserved),
        Err(StateError::ReservedNonZero)
    );
    assert_eq!(
        decode_account::<PoolState>(&data[..data.len() - 1]),
        Err(StateError::WrongLength)
    );

    let mut body_reserved = body;
    body_reserved.reserved_control[3] = 1;
    assert_eq!(body_reserved.validate(), Err(StateError::InvalidState));
}

#[test]
fn cash_deposits_preserve_controller_ownership_and_isolate_vault_surplus() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut controller = ControllerRiskState::new(pool_key, key(4)).unwrap();

    let first = record_cash_deposit(pool_key, &mut pool, &mut controller, 0, 100).unwrap();
    assert_eq!(first.amount, 100);
    assert_eq!(first.surplus_recorded, 0);
    assert_eq!(first.represented_custody_after, 100);
    assert_eq!(pool.idle_cash_collateral.get(), 100);
    assert_eq!(pool.unallocated_vault_surplus.get(), 0);
    assert_eq!(controller.available_cash_collateral.get(), 100);
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.protected_principal.get(), 0);

    let second = record_cash_deposit(pool_key, &mut pool, &mut controller, 125, 50).unwrap();
    assert_eq!(second.amount, 50);
    assert_eq!(second.surplus_recorded, 25);
    assert_eq!(second.represented_custody_after, 175);
    assert_eq!(pool.idle_cash_collateral.get(), 150);
    assert_eq!(pool.unallocated_vault_surplus.get(), 25);
    assert_eq!(controller.available_cash_collateral.get(), 150);
    assert_eq!(pool.represented_quote_custody().unwrap(), 175);
    pool.validate().unwrap();
    controller.validate().unwrap();
}

#[test]
fn cash_deposit_rejections_are_atomic_and_classic_spl_bounded() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut controller = ControllerRiskState::new(pool_key, key(4)).unwrap();
    let pristine_pool = pool;
    let pristine_controller = controller;

    assert_eq!(
        record_cash_deposit(pool_key, &mut pool, &mut controller, 0, 0),
        Err(StateError::InvalidState)
    );
    assert_eq!(pool, pristine_pool);
    assert_eq!(controller, pristine_controller);

    controller.pool = key(8);
    let mismatched_controller = controller;
    assert_eq!(
        record_cash_deposit(pool_key, &mut pool, &mut controller, 0, 1),
        Err(StateError::InvalidState)
    );
    assert_eq!(pool, pristine_pool);
    assert_eq!(controller, mismatched_controller);

    controller = pristine_controller;
    record_cash_deposit(pool_key, &mut pool, &mut controller, 0, 100).unwrap();
    let funded_pool = pool;
    let funded_controller = controller;
    assert_eq!(
        record_cash_deposit(pool_key, &mut pool, &mut controller, 99, 1),
        Err(StateError::CustodyShortfall)
    );
    assert_eq!(pool, funded_pool);
    assert_eq!(controller, funded_controller);

    pool = pristine_pool;
    controller = pristine_controller;
    pool.idle_cash_collateral = U128Le::new(u128::from(u64::MAX));
    controller.available_cash_collateral = U128Le::new(u128::from(u64::MAX));
    pool.validate().unwrap();
    controller.validate().unwrap();
    let full_pool = pool;
    let full_controller = controller;
    assert_eq!(
        record_cash_deposit(pool_key, &mut pool, &mut controller, u64::MAX, 1,),
        Err(StateError::CustodyCapacityExceeded)
    );
    assert_eq!(pool, full_pool);
    assert_eq!(controller, full_controller);
}

#[test]
fn oracle_policy_and_cash_open_use_fixed_layout_and_exact_reclassification() {
    let pool_key = key(9);
    let market_key = key(8);
    let controller_key = key(4);
    let policy = OraclePolicyState::new(pool_key, market_key, key(10), key(11), 30, 100)
        .expect("valid dual oracle policy");
    assert_eq!(policy.oracle_count, 2);
    policy.validate().unwrap();

    let one_feed = OraclePolicyState::new(pool_key, market_key, key(10), Key32::ZERO, 30, 100)
        .expect("valid single oracle policy");
    assert_eq!(one_feed.oracle_count, 1);
    assert_eq!(one_feed.secondary_feed_id, Key32::ZERO);

    let mut pool = pool();
    let mut market = market(pool_key);
    let mut controller = ControllerRiskState::new(pool_key, controller_key).unwrap();
    record_cash_deposit(pool_key, &mut pool, &mut controller, 0, 500).unwrap();
    let represented_before = pool.represented_quote_custody().unwrap();

    let opened = open_cash_position_from_idle(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        525,
        77,
        PositionSide::Long,
        100,
        10_000,
        100,
        100_000_000,
        1_000,
        100_100_000,
        1_001,
    )
    .unwrap();

    assert_eq!(opened.surplus_recorded, 25);
    assert_eq!(opened.represented_custody_after, 525);
    assert_eq!(opened.reserved_profit, 10_000);
    assert_eq!(pool.idle_cash_collateral.get(), 400);
    assert_eq!(pool.protected_principal.get(), 100);
    assert_eq!(pool.unallocated_vault_surplus.get(), 25);
    assert_eq!(pool.represented_quote_custody().unwrap(), 525);
    assert_eq!(represented_before, 500);
    assert_eq!(controller.available_cash_collateral.get(), 400);
    assert_eq!(controller.reserved_profit.get(), 10_000);
    assert_eq!(controller.open_positions.get(), 1);
    assert_eq!(market.long_open_interest.get(), 10_000);
    assert_eq!(market.short_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 10_000);
    assert_eq!(pool.reserved_open_profit.get(), 10_000);
    assert_eq!(opened.position.position_id.get(), 77);
    assert_eq!(opened.position.entry_price_e6.get(), 100_000_000);
    assert_eq!(opened.position.entry_publish_time.get(), 1_000);
    assert_eq!(opened.position.secondary_entry_price_e6.get(), 100_100_000);
    assert_eq!(opened.position.secondary_publish_time.get(), 1_001);
    opened.position.validate().unwrap();
}

#[test]
fn cash_open_risk_cash_and_custody_failures_are_atomic() {
    let pool_key = key(9);
    let market_key = key(8);
    let controller_key = key(4);
    let mut pool = pool();
    let mut market = market(pool_key);
    let mut controller = ControllerRiskState::new(pool_key, controller_key).unwrap();
    record_cash_deposit(pool_key, &mut pool, &mut controller, 0, 100).unwrap();

    for (observed, collateral, notional, leverage, expected) in [
        (100, 101, 100, 1, StateError::InsufficientCashCollateral),
        (99, 1, 1, 1, StateError::CustodyShortfall),
        (100, 1, 101, 100, StateError::RiskLimitExceeded),
        (100, 1, 2_000_001, 100, StateError::RiskLimitExceeded),
    ] {
        let before_pool = pool;
        let before_market = market;
        let before_controller = controller;
        assert_eq!(
            open_cash_position_from_idle(
                pool_key,
                market_key,
                controller_key,
                &mut pool,
                &mut market,
                &mut controller,
                observed,
                77,
                PositionSide::Short,
                collateral,
                notional,
                leverage,
                100_000_000,
                1_000,
                0,
                0,
            ),
            Err(expected)
        );
        assert_eq!(pool, before_pool);
        assert_eq!(market, before_market);
        assert_eq!(controller, before_controller);
    }

    assert!(OraclePolicyState::new(pool_key, market_key, key(10), key(10), 30, 100).is_err());
    assert!(OraclePolicyState::new(pool_key, market_key, key(10), Key32::ZERO, 0, 100).is_err());
    assert!(OraclePolicyState::new(pool_key, market_key, key(10), Key32::ZERO, 30, 0).is_err());
}

#[test]
fn cash_open_aggregate_risk_and_liability_gates_are_atomic() {
    let pool_key = key(9);
    let market_key = key(8);
    let controller_key = key(4);
    for case in 0u8..3 {
        let mut pool = pool();
        let mut market = market(pool_key);
        let mut controller = ControllerRiskState::new(pool_key, controller_key).unwrap();
        record_cash_deposit(pool_key, &mut pool, &mut controller, 0, 100).unwrap();
        let expected = match case {
            0 => {
                market.short_open_interest = market.max_short_open_interest;
                StateError::RiskLimitExceeded
            }
            1 => {
                controller.reserved_profit = pool.max_reserved_profit_per_controller;
                StateError::LiabilityLimitExceeded
            }
            2 => {
                pool.residual_bad_debt = pool.total_liability_open_gate;
                StateError::LiabilityLimitExceeded
            }
            _ => unreachable!(),
        };
        let before_pool = pool;
        let before_market = market;
        let before_controller = controller;
        assert_eq!(
            open_cash_position_from_idle(
                pool_key,
                market_key,
                controller_key,
                &mut pool,
                &mut market,
                &mut controller,
                100,
                78 + u64::from(case),
                PositionSide::Short,
                1,
                1,
                1,
                100_000_000,
                1_000,
                0,
                0,
            ),
            Err(expected)
        );
        assert_eq!(pool, before_pool);
        assert_eq!(market, before_market);
        assert_eq!(controller, before_controller);
    }
}

fn opened_cash_close_fixture(
    side: PositionSide,
    secondary: bool,
) -> (
    Key32,
    Key32,
    Key32,
    PoolState,
    MarketRiskState,
    ControllerRiskState,
    PositionState,
) {
    let pool_key = key(9);
    let market_key = key(8);
    let controller_key = key(4);
    let mut pool = pool();
    let mut market = market(pool_key);
    let mut controller = ControllerRiskState::new(pool_key, controller_key).unwrap();
    record_cash_deposit(pool_key, &mut pool, &mut controller, 0, 500).unwrap();
    let opened = open_cash_position_from_idle(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        500,
        77,
        side,
        100,
        10_000,
        100,
        100_000_000,
        1_000,
        if secondary { 100_000_000 } else { 0 },
        if secondary { 1_000 } else { 0 },
    )
    .unwrap();
    (
        pool_key,
        market_key,
        controller_key,
        pool,
        market,
        controller,
        opened.position,
    )
}

#[test]
fn cash_close_releases_exact_accounting_refunds_principal_and_tombstones() {
    assert_eq!(
        oracle_cash_pnl(PositionSide::Long, 10_000, 100_000_000, 99_500_000),
        Ok(OracleCashPnl::Loss(50))
    );
    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Long, false);
    pool.status = PoolStatus::CloseOnly as u8;
    market.status = PoolStatus::CloseOnly as u8;
    let outcome = close_cash_position(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        505,
        99_500_000,
        None,
        false,
    )
    .unwrap();
    assert_eq!(outcome.principal_refund, 50);
    assert_eq!(outcome.gross_loss, 50);
    assert_eq!(outcome.cash_loss_collected, 50);
    assert_eq!(outcome.residual_bad_debt_added, 0);
    assert_eq!(outcome.surplus_recorded, 5);
    assert_eq!(outcome.represented_custody_after, 455);
    assert_eq!(pool.idle_cash_collateral.get(), 400);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.free_cash.get(), 50);
    assert_eq!(pool.unallocated_vault_surplus.get(), 5);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.available_cash_collateral.get(), 400);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(position.reserved_profit.get(), 0);
    assert_eq!(position.entry_price_e6.get(), 100_000_000);
    position.validate().unwrap();

    let before = (pool, market, controller, position);
    assert!(close_cash_position(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        455,
        99_500_000,
        None,
        false,
    )
    .is_err());
    assert_eq!((pool, market, controller, position), before);
}

#[test]
fn flat_cash_close_refunds_all_principal_and_releases_every_live_cap() {
    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Long, false);
    let outcome = close_cash_position(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        500,
        100_000_000,
        None,
        false,
    )
    .unwrap();
    assert_eq!(outcome.principal_refund, 100);
    assert_eq!(outcome.gross_loss, 0);
    assert_eq!(outcome.cash_loss_collected, 0);
    assert_eq!(outcome.residual_bad_debt_added, 0);
    assert_eq!(outcome.surplus_recorded, 0);
    assert_eq!(outcome.represented_custody_after, 400);
    assert_eq!(pool.idle_cash_collateral.get(), 400);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.residual_bad_debt.get(), 0);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(pool.represented_quote_custody().unwrap(), 400);
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.available_cash_collateral.get(), 400);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(position.reserved_profit.get(), 0);
    position.validate().unwrap();

    let before = (pool, market, controller, position);
    assert!(close_cash_position(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        400,
        100_000_000,
        None,
        false,
    )
    .is_err());
    assert_eq!((pool, market, controller, position), before);
}

#[test]
fn primary_only_exact_exhaustion_bankruptcy_has_zero_residual_debt() {
    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Long, false);
    let outcome = close_cash_position(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        500,
        99_000_000,
        None,
        true,
    )
    .unwrap();
    assert_eq!(outcome.principal_refund, 0);
    assert_eq!(outcome.gross_loss, 100);
    assert_eq!(outcome.cash_loss_collected, 100);
    assert_eq!(outcome.residual_bad_debt_added, 0);
    assert_eq!(outcome.surplus_recorded, 0);
    assert_eq!(outcome.represented_custody_after, 500);
    assert_eq!(pool.idle_cash_collateral.get(), 400);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.free_cash.get(), 100);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.residual_bad_debt.get(), 0);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(pool.represented_quote_custody().unwrap(), 500);
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.available_cash_collateral.get(), 400);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(position.reserved_profit.get(), 0);
    position.validate().unwrap();

    let before = (pool, market, controller, position);
    assert!(close_cash_position(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        500,
        99_000_000,
        None,
        true,
    )
    .is_err());
    assert_eq!((pool, market, controller, position), before);
}

#[test]
fn bankruptcy_requires_every_configured_feed_and_uses_primary_loss() {
    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Short, true);
    let before = (pool, market, controller, position);
    assert_eq!(
        close_cash_position(
            pool_key,
            market_key,
            controller_key,
            &mut pool,
            &mut market,
            &mut controller,
            &mut position,
            500,
            101_000_000,
            Some(100_500_000),
            true,
        ),
        Err(StateError::PositionNotBankrupt)
    );
    assert_eq!((pool, market, controller, position), before);

    let outcome = close_cash_position(
        pool_key,
        market_key,
        controller_key,
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        500,
        102_500_000,
        Some(101_000_000),
        true,
    )
    .unwrap();
    assert_eq!(outcome.principal_refund, 0);
    assert_eq!(outcome.gross_loss, 250);
    assert_eq!(outcome.cash_loss_collected, 100);
    assert_eq!(outcome.residual_bad_debt_added, 150);
    assert_eq!(pool.free_cash.get(), 100);
    assert_eq!(pool.residual_bad_debt.get(), 150);
    assert_eq!(pool.represented_quote_custody().unwrap(), 500);
}

#[test]
fn cash_close_routes_behind_queue_and_all_failures_are_atomic() {
    for settled_head_only in [false, true] {
        let (
            pool_key,
            market_key,
            controller_key,
            mut pool,
            mut market,
            mut controller,
            mut position,
        ) = opened_cash_close_fixture(PositionSide::Long, false);
        pool.total_queued = U128Le::new(u128::from(!settled_head_only));
        pool.queue_flags = u8::from(settled_head_only) * QUEUE_FLAG_HEAD_SETTLED;
        pool.next_claim_sequence = U128Le::new(2);
        pool.head_claim_sequence = U128Le::new(1);
        pool.tail_claim_sequence = U128Le::new(1);
        pool.validate().unwrap();
        let outcome = close_cash_position(
            pool_key,
            market_key,
            controller_key,
            &mut pool,
            &mut market,
            &mut controller,
            &mut position,
            500,
            99_000_000,
            None,
            false,
        )
        .unwrap();
        assert_eq!(outcome.principal_refund, 0);
        assert_eq!(pool.free_cash.get(), 0);
        assert_eq!(pool.claim_service_cash.get(), 100);
    }

    for case in 0u8..5 {
        let (
            pool_key,
            market_key,
            controller_key,
            mut pool,
            mut market,
            mut controller,
            mut position,
        ) = opened_cash_close_fixture(PositionSide::Long, false);
        let (observed, exit) = match case {
            0 => (499, 99_000_000),
            1 => (500, 101_000_000),
            2 => {
                pool.residual_bad_debt = U128Le::new(u128::MAX);
                (500, 98_000_000)
            }
            3 => {
                pool.status = PoolStatus::ClaimsOnly as u8;
                (500, 99_000_000)
            }
            4 => {
                market.status = PoolStatus::Terminal as u8;
                (500, 99_000_000)
            }
            _ => unreachable!(),
        };
        let before = (pool, market, controller, position);
        assert!(close_cash_position(
            pool_key,
            market_key,
            controller_key,
            &mut pool,
            &mut market,
            &mut controller,
            &mut position,
            observed,
            exit,
            None,
            false,
        )
        .is_err());
        assert_eq!((pool, market, controller, position), before);
    }
}

#[test]
fn profitable_cash_close_pays_available_cash_then_queues_exact_remainder() {
    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Long, false);
    pool.idle_cash_collateral = U128Le::new(370);
    pool.free_cash = U128Le::new(30);
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();

    let outcome = close_profitable_cash_position(
        pool_key,
        market_key,
        controller_key,
        Some(key(33)),
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        Some(&mut page),
        None,
        500,
        101_000_000,
        None,
    )
    .unwrap();
    assert_eq!(outcome.adjustment.raw_profit, 100);
    assert_eq!(outcome.adjustment.haircut_withheld, 10);
    assert_eq!(outcome.adjustment.win_fee, 2);
    assert_eq!(outcome.adjustment.capped_trader_profit, 88);
    assert_eq!(outcome.principal_refund, 100);
    assert_eq!(outcome.immediate_profit_paid, 30);
    assert_eq!(outcome.queued_profit, 58);
    let claim = outcome.claim.unwrap();
    assert_eq!(claim.sequence.get(), 1);
    assert_eq!(claim.queue_page_index.get(), 0);
    assert_eq!(claim.queue_slot.get(), 0);
    assert_eq!(claim.original_face.get(), 58);
    assert_eq!(claim.remaining_face.get(), 58);
    assert_eq!(pool.total_queued.get(), 58);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.reserved_open_profit.get(), 0);
    assert_eq!(pool.fee_accrual.get(), 2);
    assert_eq!(pool.represented_quote_custody().unwrap(), 370);
    assert_eq!(market.long_open_interest.get(), 0);
    assert_eq!(market.reserved_profit.get(), 0);
    assert_eq!(controller.reserved_profit.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(controller.queued_claim_face.get(), 58);
    assert_eq!(controller.active_claims.get(), 1);
    assert_eq!(position.status, PositionStatus::Closed as u8);
    assert_eq!(page.entries[0].claim, key(33));
    assert_eq!(page.entries[0].remaining_face.get(), 58);
}

#[test]
fn profitable_cash_close_respects_fifo_settled_head_and_rolls_full_tail() {
    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Long, false);
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    append_claim(pool_key, &mut pool, &mut page, key(40), 1).unwrap();
    pool.claim_service_cash = U128Le::new(1);
    pay_fifo_head(pool_key, &mut pool, &mut page, 1).unwrap();
    assert_eq!(pool.total_queued.get(), 0);
    assert_ne!(pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED, 0);

    let settled = close_profitable_cash_position(
        pool_key,
        market_key,
        controller_key,
        Some(key(41)),
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        Some(&mut page),
        None,
        500,
        101_000_000,
        None,
    )
    .unwrap();
    assert_eq!(settled.immediate_profit_paid, 0);
    assert_eq!(settled.queued_profit, 88);
    assert_eq!(settled.claim.unwrap().sequence.get(), 2);
    assert_eq!(pool.head_claim_sequence.get(), 1);
    assert_eq!(pool.tail_claim_sequence.get(), 2);
    assert_ne!(pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED, 0);

    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Short, false);
    let mut full = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    for seed in 50..82 {
        append_claim(pool_key, &mut pool, &mut full, key(seed), 1).unwrap();
    }
    let mut rollover = QueuePageState::new(pool_key, 1, 0, 33).unwrap();
    let outcome = close_profitable_cash_position(
        pool_key,
        market_key,
        controller_key,
        Some(key(90)),
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        Some(&mut full),
        Some(&mut rollover),
        500,
        99_000_000,
        None,
    )
    .unwrap();
    assert_eq!(outcome.immediate_profit_paid, 0);
    assert_eq!(outcome.queued_profit, 88);
    assert_eq!(outcome.claim.unwrap().sequence.get(), 33);
    assert_eq!(full.next_page_index.get(), 1);
    assert_eq!(rollover.entries[0].claim, key(90));
    assert_eq!(pool.tail_page_index.get(), 1);
    assert_eq!(pool.next_page_index.get(), 2);
    assert_eq!(market.short_open_interest.get(), 0);
}

#[test]
fn profitable_cash_close_handles_zero_atom_profit_and_shape_failures_atomically() {
    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Long, false);
    let outcome = close_profitable_cash_position(
        pool_key,
        market_key,
        controller_key,
        None,
        &mut pool,
        &mut market,
        &mut controller,
        &mut position,
        None,
        None,
        500,
        100_000_001,
        None,
    )
    .unwrap();
    assert_eq!(outcome.adjustment.raw_profit, 0);
    assert_eq!(outcome.immediate_profit_paid, 0);
    assert_eq!(outcome.queued_profit, 0);
    assert_eq!(outcome.principal_refund, 100);
    assert_eq!(pool.represented_quote_custody().unwrap(), 400);

    let (pool_key, market_key, controller_key, mut pool, mut market, mut controller, mut position) =
        opened_cash_close_fixture(PositionSide::Long, false);
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    let before = (pool, market, controller, position, page);
    assert_eq!(
        close_profitable_cash_position(
            pool_key,
            market_key,
            controller_key,
            None,
            &mut pool,
            &mut market,
            &mut controller,
            &mut position,
            Some(&mut page),
            None,
            500,
            101_000_000,
            None,
        ),
        Err(StateError::SettlementShapeMismatch)
    );
    assert_eq!((pool, market, controller, position, page), before);

    for status in [PoolStatus::ClaimsOnly, PoolStatus::Terminal] {
        let (
            pool_key,
            market_key,
            controller_key,
            mut pool,
            mut market,
            mut controller,
            mut position,
        ) = opened_cash_close_fixture(PositionSide::Long, false);
        pool.status = status as u8;
        market.status = status as u8;
        let before = (pool, market, controller, position);
        assert_eq!(
            close_profitable_cash_position(
                pool_key,
                market_key,
                controller_key,
                None,
                &mut pool,
                &mut market,
                &mut controller,
                &mut position,
                None,
                None,
                500,
                100_000_001,
                None,
            ),
            Err(StateError::InvalidState)
        );
        assert_eq!((pool, market, controller, position), before);
    }
}

#[test]
fn invalid_tags_and_cash_claim_source_cosplay_are_rejected() {
    let pool_key = key(9);
    let mut position = cash_position(pool_key);
    position.funding_kind = 9;
    assert_eq!(position.validate(), Err(StateError::InvalidTag));

    let mut claim = cash_claim(pool_key);
    claim.status = 9;
    assert_eq!(claim.validate(), Err(StateError::InvalidTag));

    let mut cash_with_source = cash_position(pool_key);
    cash_with_source.source_claim_sequence = U128Le::new(7);
    assert_eq!(cash_with_source.validate(), Err(StateError::InvalidState));

    let mut claim_funded = cash_position(pool_key);
    claim_funded.funding_kind = FundingKind::Claim as u8;
    claim_funded.source_claim_sequence = U128Le::new(7);
    claim_funded.validate().unwrap();

    claim_funded.source_claim_sequence = U128Le::new(NO_CLAIM_SEQUENCE);
    assert_eq!(claim_funded.validate(), Err(StateError::InvalidState));

    let mut inconsistent_leverage = cash_position(pool_key);
    inconsistent_leverage.leverage_x = U16Le::new(1);
    inconsistent_leverage.collateral = U128Le::new(100);
    inconsistent_leverage.notional = U128Le::new(101);
    inconsistent_leverage.max_payable_profit = U128Le::new(101);
    inconsistent_leverage.reserved_profit = U128Le::new(101);
    assert_eq!(
        inconsistent_leverage.validate(),
        Err(StateError::InvalidState)
    );

    let mut recycled_claim = cash_claim(pool_key);
    recycled_claim.funding_kind = FundingKind::Claim as u8;
    recycled_claim.sequence = U128Le::new(2);
    recycled_claim.source_claim_sequence = U128Le::new(1);
    recycled_claim.validate().unwrap();
    recycled_claim.source_claim_sequence = U128Le::new(2);
    assert_eq!(recycled_claim.validate(), Err(StateError::InvalidState));
}

#[test]
fn fifo_partial_payment_retains_priority_until_zero_face_is_popped() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();

    assert_eq!(
        append_claim(pool_key, &mut pool, &mut page, key(10), 100)
            .unwrap()
            .sequence,
        1
    );
    assert_eq!(
        append_claim(pool_key, &mut pool, &mut page, key(11), 50)
            .unwrap()
            .sequence,
        2
    );
    assert_eq!(pool.total_queued.get(), 150);
    pool.claim_service_cash = U128Le::new(150);

    let partial = pay_fifo_head(pool_key, &mut pool, &mut page, 40).unwrap();
    assert_eq!(partial.sequence, 1);
    assert_eq!(partial.paid, 40);
    assert_eq!(partial.remaining_face, 60);
    assert_eq!(pool.head_claim_sequence.get(), 1);
    assert_eq!(
        pop_settled_fifo_head(pool_key, &mut pool, &mut page, None),
        Err(StateError::HeadNotSettled)
    );

    let final_payment = pay_fifo_head(pool_key, &mut pool, &mut page, 1_000).unwrap();
    assert_eq!(final_payment.paid, 60);
    assert_eq!(pool.queue_flags, QUEUE_FLAG_HEAD_SETTLED);
    assert_eq!(
        pop_settled_fifo_head(pool_key, &mut pool, &mut page, None).unwrap(),
        1
    );
    assert_eq!(pool.head_claim_sequence.get(), 2);
    assert_eq!(pool.total_queued.get(), 50);

    pay_fifo_head(pool_key, &mut pool, &mut page, 50).unwrap();
    pop_settled_fifo_head(pool_key, &mut pool, &mut page, None).unwrap();
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.head_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    assert_eq!(pool.tail_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    pool.validate().unwrap();
    page.validate().unwrap();
}

#[test]
fn coupled_fifo_service_updates_claim_controller_and_releases_excess_on_pop() {
    let pool_key = key(9);
    let claim_key = key(10);
    let mut pool = pool();
    let mut controller = controller(pool_key);
    controller.reserved_profit = U128Le::new(0);
    controller.open_positions = U32Le::new(0);
    controller.queued_claim_face = U128Le::new(100);
    controller.active_claims = U32Le::new(1);
    let mut claim = cash_claim(pool_key);
    claim.original_face = U128Le::new(100);
    claim.remaining_face = U128Le::new(100);
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    append_claim(pool_key, &mut pool, &mut page, claim_key, 100).unwrap();
    pool.claim_service_cash = U128Le::new(40);

    let partial = service_fifo_claim(
        pool_key,
        &mut pool,
        &mut controller,
        &mut page,
        claim_key,
        &mut claim,
    )
    .unwrap();
    assert_eq!(partial.paid, 40);
    assert_eq!(partial.remaining_face, 60);
    assert_eq!(pool.total_queued.get(), 60);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(controller.queued_claim_face.get(), 60);
    assert_eq!(controller.active_claims.get(), 1);
    assert_eq!(claim.remaining_face.get(), 60);
    assert_eq!(claim.status, ClaimStatus::PartiallyPaid as u8);
    assert_eq!(page.entries[0].remaining_face.get(), 60);
    assert_eq!(pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED, 0);
    assert_eq!(
        pop_fifo_head_with_claim(pool_key, &mut pool, &mut page, claim_key, &claim, None),
        Err(StateError::ClaimMismatch)
    );

    pool.claim_service_cash = U128Le::new(80);
    let final_payment = service_fifo_claim(
        pool_key,
        &mut pool,
        &mut controller,
        &mut page,
        claim_key,
        &mut claim,
    )
    .unwrap();
    assert_eq!(final_payment.paid, 60);
    assert_eq!(final_payment.remaining_face, 0);
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 20);
    assert_eq!(controller.queued_claim_face.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(claim.status, ClaimStatus::Settled as u8);
    assert_ne!(pool.queue_flags & QUEUE_FLAG_HEAD_SETTLED, 0);

    assert_eq!(
        pop_fifo_head_with_claim(pool_key, &mut pool, &mut page, claim_key, &claim, None).unwrap(),
        1
    );
    assert_eq!(pool.head_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    assert_eq!(pool.tail_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.free_cash.get(), 20);
    assert_eq!(claim.status, ClaimStatus::Settled as u8);
    pool.validate().unwrap();
    controller.validate().unwrap();
    claim.validate().unwrap();
    page.validate().unwrap();
}

#[test]
fn queued_and_funding_claim_keeps_active_count_when_priority_is_paid_and_popped() {
    let pool_key = key(9);
    let claim_key = key(10);
    let mut pool = pool();
    let mut controller = controller(pool_key);
    controller.reserved_profit = U128Le::new(0);
    controller.open_positions = U32Le::new(0);
    controller.queued_claim_face = U128Le::new(60);
    controller.claim_collateral_in_use = U128Le::new(40);
    controller.active_claims = U32Le::new(1);
    let mut claim = cash_claim(pool_key);
    claim.original_face = U128Le::new(100);
    claim.remaining_face = U128Le::new(60);
    claim.collateral_in_use = U128Le::new(40);
    claim.status = ClaimStatus::QueuedAndFunding as u8;
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    append_claim(pool_key, &mut pool, &mut page, claim_key, 60).unwrap();
    pool.claim_service_cash = U128Le::new(60);
    pool.open_claim_collateral = U128Le::new(40);

    let paid = service_fifo_claim(
        pool_key,
        &mut pool,
        &mut controller,
        &mut page,
        claim_key,
        &mut claim,
    )
    .unwrap();
    assert_eq!(paid.paid, 60);
    assert_eq!(claim.status, ClaimStatus::FundingPosition as u8);
    assert_eq!(claim.collateral_in_use.get(), 40);
    assert_eq!(controller.active_claims.get(), 1);
    assert_eq!(controller.claim_collateral_in_use.get(), 40);
    assert_eq!(
        pop_fifo_head_with_claim(pool_key, &mut pool, &mut page, claim_key, &claim, None).unwrap(),
        1
    );
    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.open_claim_collateral.get(), 40);
    assert_eq!(pool.head_claim_sequence.get(), NO_CLAIM_SEQUENCE);
}

#[test]
fn coupled_fifo_service_rejects_underrepresented_aggregates_atomically() {
    let pool_key = key(9);
    let claim_key = key(10);
    for case in 0u8..5 {
        let mut pool = pool();
        let mut controller = controller(pool_key);
        controller.reserved_profit = U128Le::new(0);
        controller.open_positions = U32Le::new(0);
        controller.active_claims = U32Le::new(1);
        let mut claim = cash_claim(pool_key);
        let (face, service_cash) = if case < 3 { (100, 40) } else { (60, 20) };
        claim.original_face = U128Le::new(100);
        claim.remaining_face = U128Le::new(face);
        controller.queued_claim_face = U128Le::new(face);
        let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
        append_claim(pool_key, &mut pool, &mut page, claim_key, face).unwrap();
        pool.claim_service_cash = U128Le::new(service_cash);

        if case >= 3 {
            claim.collateral_in_use = U128Le::new(40);
            claim.status = ClaimStatus::QueuedAndFunding as u8;
            pool.open_claim_collateral = U128Le::new(40);
            controller.claim_collateral_in_use = U128Le::new(40);
        }
        match case {
            0 => pool.total_queued = U128Le::new(service_cash),
            1 => controller.queued_claim_face = U128Le::new(service_cash),
            2 => controller.active_claims = U32Le::new(0),
            3 => pool.open_claim_collateral = U128Le::new(39),
            4 => controller.claim_collateral_in_use = U128Le::new(39),
            _ => unreachable!(),
        }

        pool.validate().unwrap();
        controller.validate().unwrap();
        claim.validate().unwrap();
        page.validate().unwrap();
        let before = (pool, controller, page, claim);
        assert_eq!(
            service_fifo_claim(
                pool_key,
                &mut pool,
                &mut controller,
                &mut page,
                claim_key,
                &mut claim,
            ),
            Err(StateError::ClaimMismatch),
            "aggregate case {case}"
        );
        assert_eq!((pool, controller, page, claim), before, "case {case}");
    }
}

#[test]
fn a_full_page_links_to_the_next_page_without_reordering() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut first = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    for index in 0..QUEUE_PAGE_CAPACITY {
        let outcome =
            append_claim(pool_key, &mut pool, &mut first, key((index + 10) as u8), 1).unwrap();
        assert_eq!(outcome.sequence, index as u128 + 1);
    }
    let pool_before_rejected_append = pool;
    let first_before_rejected_append = first;
    assert_eq!(
        append_claim(pool_key, &mut pool, &mut first, key(99), 1),
        Err(StateError::QueueFull)
    );
    assert_eq!(pool, pool_before_rejected_append);
    assert_eq!(first, first_before_rejected_append);

    let mut second = QueuePageState::new(
        pool_key,
        pool.next_page_index.get(),
        first.page_index.get(),
        pool.next_claim_sequence.get(),
    )
    .unwrap();
    link_next_queue_page(pool_key, &mut pool, &mut first, &second).unwrap();
    let appended = append_claim(pool_key, &mut pool, &mut second, key(99), 7).unwrap();
    assert_eq!(appended.sequence, 33);
    assert_eq!(pool.tail_page_index.get(), 1);
    pool.claim_service_cash = U128Le::new(39);

    for index in 0..QUEUE_PAGE_CAPACITY {
        let paid = pay_fifo_head(pool_key, &mut pool, &mut first, 1).unwrap();
        assert_eq!(paid.sequence, index as u128 + 1);
        let next = (index + 1 == QUEUE_PAGE_CAPACITY).then_some(&second);
        pop_settled_fifo_head(pool_key, &mut pool, &mut first, next).unwrap();
    }
    assert_eq!(pool.head_page_index.get(), 1);
    assert_eq!(pool.head_claim_sequence.get(), 33);
    let paid = pay_fifo_head(pool_key, &mut pool, &mut second, 7).unwrap();
    assert_eq!(paid.sequence, 33);
    pop_settled_fifo_head(pool_key, &mut pool, &mut second, None).unwrap();
    assert_eq!(pool.total_queued.get(), 0);
}

#[test]
fn broken_page_links_and_sequence_overflow_are_atomic() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    for index in 0..QUEUE_PAGE_CAPACITY {
        append_claim(pool_key, &mut pool, &mut page, key((index + 20) as u8), 1).unwrap();
    }
    let wrong_next = QueuePageState::new(pool_key, 1, NO_PAGE_INDEX, 33).unwrap_err();
    assert_eq!(wrong_next, StateError::InvalidState);

    let page_before = page;
    let pool_before = pool;
    let next = QueuePageState::new(pool_key, 2, 0, 33).unwrap();
    assert_eq!(
        link_next_queue_page(pool_key, &mut pool, &mut page, &next),
        Err(StateError::BrokenPageLink)
    );
    assert_eq!(pool, pool_before);
    assert_eq!(page, page_before);

    let mut non_tail_pool = PoolState::new(77, key(1), key(2), key(3));
    let mut non_tail_page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    non_tail_page.next_page_index = U64Le::new(1);
    let non_tail_pool_before = non_tail_pool;
    let non_tail_page_before = non_tail_page;
    assert_eq!(
        append_claim(pool_key, &mut non_tail_pool, &mut non_tail_page, key(78), 1,),
        Err(StateError::BrokenPageLink)
    );
    assert_eq!(non_tail_pool, non_tail_pool_before);
    assert_eq!(non_tail_page, non_tail_page_before);

    let mut overflow_pool = PoolState::new(77, key(1), key(2), key(3));
    overflow_pool.next_claim_sequence = U128Le::new(NO_CLAIM_SEQUENCE - 1);
    let mut overflow_page =
        QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, NO_CLAIM_SEQUENCE - 1).unwrap();
    let overflow_pool_before = overflow_pool;
    let overflow_page_before = overflow_page;
    assert_eq!(
        append_claim(pool_key, &mut overflow_pool, &mut overflow_page, key(77), 1),
        Err(StateError::ArithmeticOverflow)
    );
    assert_eq!(overflow_pool, overflow_pool_before);
    assert_eq!(overflow_page, overflow_page_before);
}

#[test]
fn queue_priority_is_protocol_sequence_not_caller_metadata() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();

    // Caller-chosen keys deliberately sort opposite to arrival. There is no
    // priority-fee, size, timestamp, or caller-provided sequence field in the
    // queue entry; the pool assigns the only ordering value.
    let first = append_claim(pool_key, &mut pool, &mut page, key(250), 1).unwrap();
    let second = append_claim(pool_key, &mut pool, &mut page, key(1), 1_000_000).unwrap();
    assert_eq!((first.sequence, second.sequence), (1, 2));
    assert_eq!(page.entries[0].claim, key(250));
    assert_eq!(page.entries[1].claim, key(1));
}

#[test]
fn free_cash_cannot_skip_an_existing_or_new_fifo_queue() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    pool.free_cash = U128Le::new(7);
    pool.validate().unwrap();

    let pool_before = pool;
    let page_before = page;
    assert_eq!(
        append_claim(pool_key, &mut pool, &mut page, key(10), 10),
        Err(StateError::InvalidState)
    );
    assert_eq!(pool, pool_before);
    assert_eq!(page, page_before);

    pool.free_cash = U128Le::new(0);
    append_claim(pool_key, &mut pool, &mut page, key(10), 10).unwrap();
    pool.free_cash = U128Le::new(1);
    assert_eq!(pool.validate(), Err(StateError::InvalidState));
}

#[test]
fn fifo_payments_are_bounded_by_dedicated_claim_service_cash() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    append_claim(pool_key, &mut pool, &mut page, key(10), 10).unwrap();

    assert_eq!(
        pay_fifo_head(pool_key, &mut pool, &mut page, 10),
        Err(StateError::InsufficientClaimServiceCash)
    );
    pool.claim_service_cash = U128Le::new(4);
    let paid = pay_fifo_head(pool_key, &mut pool, &mut page, 10).unwrap();
    assert_eq!(paid.paid, 4);
    assert_eq!(paid.remaining_face, 6);
    assert_eq!(pool.total_queued.get(), 6);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(
        pay_fifo_head(pool_key, &mut pool, &mut page, 10),
        Err(StateError::InsufficientClaimServiceCash)
    );
}

#[test]
fn non_head_claim_can_fund_positions_without_losing_fifo_identity() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    append_claim(pool_key, &mut pool, &mut page, key(10), 100).unwrap();
    append_claim(pool_key, &mut pool, &mut page, key(11), 50).unwrap();

    let mut controller = controller(pool_key);
    controller.queued_claim_face = U128Le::new(50);
    controller.active_claims = U32Le::new(1);
    let mut claim = cash_claim(pool_key);
    claim.queue_slot = U16Le::new(1);
    claim.sequence = U128Le::new(2);
    claim.original_face = U128Le::new(50);
    claim.remaining_face = U128Le::new(50);

    let partial = consume_claim_for_position(
        pool_key,
        &mut pool,
        &mut controller,
        &mut page,
        key(11),
        &mut claim,
        20,
    )
    .unwrap();
    assert_eq!(partial.remaining_at_priority, 30);
    assert_eq!(partial.collateral_in_use, 20);
    assert_eq!(claim.status, ClaimStatus::QueuedAndFunding as u8);
    assert_eq!(page.entries[1].remaining_face.get(), 30);
    assert_eq!(pool.total_queued.get(), 130);
    assert_eq!(pool.open_claim_collateral.get(), 20);
    assert_eq!(controller.queued_claim_face.get(), 30);
    assert_eq!(controller.claim_collateral_in_use.get(), 20);

    let final_use = consume_claim_for_position(
        pool_key,
        &mut pool,
        &mut controller,
        &mut page,
        key(11),
        &mut claim,
        30,
    )
    .unwrap();
    assert_eq!(final_use.remaining_at_priority, 0);
    assert_eq!(final_use.collateral_in_use, 50);
    assert_eq!(claim.status, ClaimStatus::FundingPosition as u8);
    assert_eq!(page.entries[1].remaining_face.get(), 0);
    assert_eq!(pool.total_queued.get(), 100);
    assert_eq!(pool.open_claim_collateral.get(), 50);

    pool.claim_service_cash = U128Le::new(125);
    pay_fifo_head(pool_key, &mut pool, &mut page, 100).unwrap();
    assert_eq!(
        pop_settled_fifo_head(pool_key, &mut pool, &mut page, None).unwrap(),
        1
    );
    assert_eq!(pool.head_claim_sequence.get(), 2);
    assert_eq!(pool.queue_flags, QUEUE_FLAG_HEAD_SETTLED);
    assert_eq!(pool.claim_service_cash.get(), 25);
    assert_eq!(pool.free_cash.get(), 0);

    assert_eq!(
        pop_settled_fifo_head(pool_key, &mut pool, &mut page, None).unwrap(),
        2
    );
    assert_eq!(pool.head_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    assert_eq!(pool.tail_claim_sequence.get(), NO_CLAIM_SEQUENCE);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.free_cash.get(), 25);
    claim.validate().unwrap();
    controller.validate().unwrap();
    pool.validate().unwrap();
    page.validate().unwrap();
}

#[test]
fn claim_collateral_binding_failures_are_atomic() {
    let pool_key = key(9);
    let mut pool = pool();
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    append_claim(pool_key, &mut pool, &mut page, key(10), 50).unwrap();
    let mut controller = controller(pool_key);
    controller.queued_claim_face = U128Le::new(50);
    let mut claim = cash_claim(pool_key);

    let pool_before = pool;
    let controller_before = controller;
    let page_before = page;
    let claim_before = claim;
    assert_eq!(
        consume_claim_for_position(
            pool_key,
            &mut pool,
            &mut controller,
            &mut page,
            key(99),
            &mut claim,
            10,
        ),
        Err(StateError::ClaimMismatch)
    );
    assert_eq!(pool, pool_before);
    assert_eq!(controller, controller_before);
    assert_eq!(page, page_before);
    assert_eq!(claim, claim_before);
}

fn open_full_head_claim_position(
    collateral: u128,
    notional: u128,
) -> (
    Key32,
    PoolState,
    MarketRiskState,
    ControllerRiskState,
    QueuePageState,
    ClaimState,
    PositionState,
) {
    let pool_key = key(9);
    let mut pool = pool();
    let mut market = market(pool_key);
    let mut controller = ControllerRiskState::new(pool_key, key(4)).unwrap();
    let mut page = QueuePageState::new(pool_key, 0, NO_PAGE_INDEX, 1).unwrap();
    append_claim(pool_key, &mut pool, &mut page, key(10), collateral).unwrap();
    controller.queued_claim_face = U128Le::new(collateral);
    controller.active_claims = U32Le::new(1);
    let mut claim = ClaimState::new_cash(pool_key, key(4), 9, 1, 0, 0, 1, collateral).unwrap();

    let opened = open_claim_position_from_queue(
        pool_key,
        key(8),
        key(4),
        key(10),
        &mut pool,
        &mut market,
        &mut controller,
        &mut page,
        &mut claim,
        0,
        10,
        PositionSide::Long,
        collateral,
        notional,
        10,
        100_000_000,
        1_000,
        0,
        0,
    )
    .unwrap();
    (
        pool_key,
        pool,
        market,
        controller,
        page,
        claim,
        opened.position,
    )
}

#[test]
fn claim_open_is_atomic_contingent_collateral_not_cash_principal() {
    let (pool_key, pool, market, controller, page, claim, position) =
        open_full_head_claim_position(50, 500);

    assert_eq!(pool.total_queued.get(), 0);
    assert_eq!(pool.open_claim_collateral.get(), 50);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(pool.queue_flags, QUEUE_FLAG_HEAD_SETTLED);
    assert_eq!(pool.represented_quote_custody().unwrap(), 0);
    assert_eq!(market.long_open_interest.get(), 500);
    assert_eq!(market.reserved_profit.get(), 500);
    assert_eq!(controller.queued_claim_face.get(), 0);
    assert_eq!(controller.claim_collateral_in_use.get(), 50);
    assert_eq!(controller.open_positions.get(), 1);
    assert_eq!(controller.active_claims.get(), 1);
    assert_eq!(claim.status, ClaimStatus::FundingPosition as u8);
    assert_eq!(claim.remaining_face.get(), 0);
    assert_eq!(claim.collateral_in_use.get(), 50);
    assert_eq!(page.entries[0].remaining_face.get(), 0);
    assert_eq!(position.funding_kind, FundingKind::Claim as u8);
    assert_eq!(position.source_claim_sequence.get(), 1);
    assert_eq!(position.controller, key(4));
    assert_eq!(position.pool, pool_key);
}

#[test]
fn flat_claim_close_after_source_pop_can_pay_surviving_collateral_immediately() {
    let (pool_key, mut pool, mut market, mut controller, mut page, mut claim, mut position) =
        open_full_head_claim_position(50, 500);
    pool.claim_service_cash = U128Le::new(60);
    pop_fifo_head_with_claim(pool_key, &mut pool, &mut page, key(10), &claim, None).unwrap();
    assert_eq!(pool.free_cash.get(), 60);

    let outcome = close_nonpositive_claim_position(
        pool_key,
        key(8),
        key(4),
        None,
        &mut pool,
        &mut market,
        &mut controller,
        &mut claim,
        &mut position,
        None,
        None,
        60,
        100_000_000,
        None,
    )
    .unwrap();

    assert_eq!(outcome.immediate_payout, 50);
    assert_eq!(outcome.queued_payout, 0);
    assert_eq!(outcome.child_claim, None);
    assert_eq!(outcome.represented_custody_after, 10);
    assert_eq!(pool.free_cash.get(), 10);
    assert_eq!(pool.open_claim_collateral.get(), 0);
    assert_eq!(pool.protected_principal.get(), 0);
    assert_eq!(controller.claim_collateral_in_use.get(), 0);
    assert_eq!(controller.open_positions.get(), 0);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(claim.status, ClaimStatus::Settled as u8);
    assert_eq!(position.status, PositionStatus::Closed as u8);
}

#[test]
fn losing_claim_close_moves_survivor_to_later_linked_fifo_claim() {
    let (pool_key, mut pool, mut market, mut controller, mut page, mut source, mut position) =
        open_full_head_claim_position(50, 500);

    let outcome = close_nonpositive_claim_position(
        pool_key,
        key(8),
        key(4),
        Some(key(11)),
        &mut pool,
        &mut market,
        &mut controller,
        &mut source,
        &mut position,
        Some(&mut page),
        None,
        0,
        94_000_000,
        None,
    )
    .unwrap();

    assert_eq!(outcome.gross_loss, 30);
    assert_eq!(outcome.claim_loss_extinguished, 30);
    assert_eq!(outcome.residual_bad_debt_added, 0);
    assert_eq!(outcome.immediate_payout, 0);
    assert_eq!(outcome.queued_payout, 20);
    let child = outcome.child_claim.unwrap();
    assert_eq!(child.sequence.get(), 2);
    assert_eq!(child.source_claim_sequence.get(), 1);
    assert_eq!(child.funding_kind, FundingKind::Claim as u8);
    assert_eq!(source.status, ClaimStatus::Settled as u8);
    assert_eq!(controller.active_claims.get(), 1);
    assert_eq!(controller.queued_claim_face.get(), 20);
    assert_eq!(controller.claim_collateral_in_use.get(), 0);
    assert_eq!(pool.total_queued.get(), 20);
    assert_eq!(pool.open_claim_collateral.get(), 0);
    assert_eq!(pool.queue_flags, QUEUE_FLAG_HEAD_SETTLED);
    assert_eq!(page.entries[0].remaining_face.get(), 0);
    assert_eq!(page.entries[1].claim, key(11));
    assert_eq!(page.entries[1].remaining_face.get(), 20);
}

#[test]
fn profitable_claim_close_applies_haircut_and_never_restores_source_priority() {
    let (pool_key, mut pool, mut market, mut controller, mut page, mut source, mut position) =
        open_full_head_claim_position(50, 500);

    let outcome = close_profitable_claim_position(
        pool_key,
        key(8),
        key(4),
        Some(key(11)),
        &mut pool,
        &mut market,
        &mut controller,
        &mut source,
        &mut position,
        Some(&mut page),
        None,
        0,
        110_000_000,
        None,
    )
    .unwrap();

    let adjustment = outcome.adjustment.unwrap();
    assert!(adjustment.capped_trader_profit > 0);
    assert_eq!(outcome.queued_payout, 50 + adjustment.capped_trader_profit);
    assert_eq!(outcome.immediate_payout, 0);
    let child = outcome.child_claim.unwrap();
    assert_eq!(child.sequence.get(), 2);
    assert_eq!(child.source_claim_sequence.get(), 1);
    assert_eq!(child.original_face.get(), outcome.queued_payout);
    assert_eq!(source.sequence.get(), 1);
    assert_eq!(source.status, ClaimStatus::Settled as u8);
    assert_eq!(page.entries[0].remaining_face.get(), 0);
    assert_eq!(page.entries[1].sequence.get(), 2);
    assert_eq!(pool.fee_accrual.get(), adjustment.win_fee);
    assert_eq!(pool.protected_principal.get(), 0);
}

#[test]
fn permissionless_claim_bankruptcy_extinguishes_liability_without_creating_cash() {
    let (pool_key, mut pool, mut market, mut controller, _page, mut source, mut position) =
        open_full_head_claim_position(50, 500);
    let before_custody = pool.represented_quote_custody().unwrap();

    let outcome = resolve_bankrupt_claim_position(
        pool_key,
        key(8),
        key(4),
        &mut pool,
        &mut market,
        &mut controller,
        &mut source,
        &mut position,
        0,
        80_000_000,
        None,
    )
    .unwrap();

    assert_eq!(outcome.gross_loss, 100);
    assert_eq!(outcome.claim_loss_extinguished, 50);
    assert_eq!(outcome.residual_bad_debt_added, 50);
    assert_eq!(outcome.immediate_payout, 0);
    assert_eq!(outcome.queued_payout, 0);
    assert_eq!(outcome.child_claim, None);
    assert_eq!(pool.free_cash.get(), 0);
    assert_eq!(pool.claim_service_cash.get(), 0);
    assert_eq!(pool.residual_bad_debt.get(), 50);
    assert_eq!(pool.represented_quote_custody().unwrap(), before_custody);
    assert_eq!(source.status, ClaimStatus::Extinguished as u8);
    assert_eq!(controller.active_claims.get(), 0);
    assert_eq!(position.status, PositionStatus::Closed as u8);
}

#[test]
fn claim_close_wrong_shape_and_nonbankrupt_resolution_are_atomic() {
    let (pool_key, mut pool, mut market, mut controller, _page, mut source, mut position) =
        open_full_head_claim_position(50, 500);
    let before = (pool, market, controller, source, position);
    assert_eq!(
        close_nonpositive_claim_position(
            pool_key,
            key(8),
            key(4),
            None,
            &mut pool,
            &mut market,
            &mut controller,
            &mut source,
            &mut position,
            None,
            None,
            0,
            94_000_000,
            None,
        ),
        Err(StateError::SettlementShapeMismatch)
    );
    assert_eq!((pool, market, controller, source, position), before);

    assert_eq!(
        resolve_bankrupt_claim_position(
            pool_key,
            key(8),
            key(4),
            &mut pool,
            &mut market,
            &mut controller,
            &mut source,
            &mut position,
            0,
            95_000_000,
            None,
        ),
        Err(StateError::PositionNotBankrupt)
    );
    assert_eq!((pool, market, controller, source, position), before);
}

#[test]
fn pool_wind_down_is_one_way_and_requires_positions_then_claims_to_be_drained() {
    let mut pool = pool();
    let original = pool;
    assert_eq!(
        transition_pool_status(&mut pool, PoolStatus::ClaimsOnly),
        Err(StateError::InvalidStatusTransition)
    );
    assert_eq!(pool, original);

    transition_pool_status(&mut pool, PoolStatus::CloseOnly).unwrap();
    assert_eq!(pool.status, PoolStatus::CloseOnly as u8);
    let close_only = pool;
    assert_eq!(
        transition_pool_status(&mut pool, PoolStatus::Active),
        Err(StateError::InvalidStatusTransition)
    );
    assert_eq!(pool, close_only);

    pool.protected_principal = U128Le::new(1);
    let with_open_cash = pool;
    assert_eq!(
        transition_pool_status(&mut pool, PoolStatus::ClaimsOnly),
        Err(StateError::InvalidStatusTransition)
    );
    assert_eq!(pool, with_open_cash);
    pool.protected_principal = U128Le::new(0);
    transition_pool_status(&mut pool, PoolStatus::ClaimsOnly).unwrap();

    pool.total_queued = U128Le::new(1);
    pool.next_claim_sequence = U128Le::new(2);
    pool.head_claim_sequence = U128Le::new(1);
    pool.tail_claim_sequence = U128Le::new(1);
    let with_claim = pool;
    assert_eq!(
        transition_pool_status(&mut pool, PoolStatus::Terminal),
        Err(StateError::InvalidStatusTransition)
    );
    assert_eq!(pool, with_claim);
    pool.total_queued = U128Le::new(0);
    pool.head_claim_sequence = U128Le::new(NO_CLAIM_SEQUENCE);
    pool.tail_claim_sequence = U128Le::new(NO_CLAIM_SEQUENCE);
    transition_pool_status(&mut pool, PoolStatus::Terminal).unwrap();
    assert_eq!(pool.status, PoolStatus::Terminal as u8);
    let terminal = pool;
    assert_eq!(
        transition_pool_status(&mut pool, PoolStatus::Terminal),
        Err(StateError::InvalidStatusTransition)
    );
    assert_eq!(pool, terminal);
}

#[test]
fn market_wind_down_rejects_open_interest_and_skipped_or_reverse_transitions() {
    let pool_key = key(9);
    let mut market = market(pool_key);
    let original = market;
    assert_eq!(
        transition_market_status(pool_key, &mut market, PoolStatus::Terminal),
        Err(StateError::InvalidStatusTransition)
    );
    assert_eq!(market, original);
    transition_market_status(pool_key, &mut market, PoolStatus::CloseOnly).unwrap();

    market.long_open_interest = U128Le::new(1);
    let with_open_interest = market;
    assert_eq!(
        transition_market_status(pool_key, &mut market, PoolStatus::Terminal),
        Err(StateError::InvalidStatusTransition)
    );
    assert_eq!(market, with_open_interest);
    market.long_open_interest = U128Le::new(0);
    transition_market_status(pool_key, &mut market, PoolStatus::Terminal).unwrap();
    assert_eq!(market.status, PoolStatus::Terminal as u8);
    assert_eq!(
        transition_market_status(pool_key, &mut market, PoolStatus::Active),
        Err(StateError::InvalidStatusTransition)
    );
}

#[test]
fn idle_cash_withdrawal_is_controller_bounded_and_valid_in_every_pool_mode() {
    for status in [
        PoolStatus::Active,
        PoolStatus::CloseOnly,
        PoolStatus::ClaimsOnly,
        PoolStatus::Terminal,
    ] {
        let pool_key = key(9);
        let controller_key = key(4);
        let mut pool = pool();
        pool.status = status as u8;
        pool.idle_cash_collateral = U128Le::new(100);
        let mut controller = ControllerRiskState::new(pool_key, controller_key).unwrap();
        controller.available_cash_collateral = U128Le::new(100);
        let outcome = withdraw_idle_cash(
            pool_key,
            controller_key,
            &mut pool,
            &mut controller,
            110,
            40,
        )
        .unwrap();
        assert_eq!(outcome.amount, 40);
        assert_eq!(outcome.surplus_recorded, 10);
        assert_eq!(outcome.represented_custody_after, 70);
        assert_eq!(pool.idle_cash_collateral.get(), 60);
        assert_eq!(pool.unallocated_vault_surplus.get(), 10);
        assert_eq!(controller.available_cash_collateral.get(), 60);
    }
}

#[test]
fn invalid_idle_cash_withdrawals_are_atomic() {
    let pool_key = key(9);
    let controller_key = key(4);
    let mut pool = pool();
    pool.idle_cash_collateral = U128Le::new(100);
    let mut controller = ControllerRiskState::new(pool_key, controller_key).unwrap();
    controller.available_cash_collateral = U128Le::new(100);
    let before = (pool, controller);

    assert_eq!(
        withdraw_idle_cash(
            pool_key,
            controller_key,
            &mut pool,
            &mut controller,
            100,
            101,
        ),
        Err(StateError::InsufficientCashCollateral)
    );
    assert_eq!((pool, controller), before);
    assert_eq!(
        withdraw_idle_cash(pool_key, key(5), &mut pool, &mut controller, 100, 1,),
        Err(StateError::InsufficientCashCollateral)
    );
    assert_eq!((pool, controller), before);
    assert_eq!(
        withdraw_idle_cash(pool_key, controller_key, &mut pool, &mut controller, 99, 1,),
        Err(StateError::CustodyShortfall)
    );
    assert_eq!((pool, controller), before);
}
