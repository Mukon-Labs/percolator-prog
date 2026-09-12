// Reuse the existing deployed-ABI LiteSVM fixtures. Run this target with the
// `session_runtime_` filter; no RPC, real wallet or remote accounts are used.
include!("v16_cu.rs");
use percolator_prog::processor::trading_sessions as sessions;
use percolator_prog::trading_session::{Grant as SessionGrant, ACCOUNT_LEN as SESSION_LEN};

fn session_env() -> (V16CuEnv, Keypair, Keypair, Pubkey, Pubkey) {
    let mut env = V16CuEnv::new_with_market_params_and_price_move(3, 10_000, 10_000, 10_000);
    env.initialize_order_market_instance(991);
    let owner = Keypair::new();
    let session = Keypair::new();
    env.svm.airdrop(&owner.pubkey(), 1_000_000_000).unwrap();
    env.svm.airdrop(&session.pubkey(), 1_000_000_000).unwrap();
    let portfolio = env.create_portfolio(&owner);
    let grant = sessions::address(&env.program_id, &owner.pubkey(), &portfolio).0;
    (env, owner, session, portfolio, grant)
}

#[test]
#[ignore = "requires the separately built local Ninja adapter ELF via NINJA_ADAPTER_TEST_ELF"]
fn session_runtime_real_adapter_child_grant_uses_reserved_parent_without_owner_signature() {
    let program_id: Pubkey = "7C37Xn3NLknqmSaxASYy2uRkb1RQcXigPmJCANUNYnvq".parse().unwrap();
    let adapter: Pubkey = "B4Ja8dASFYZcPs16rPHoiHCxpkizjXSEygUbf5Pjx98c".parse().unwrap();
    let mut env = V16CuEnv::new_with_init_params_capacity_and_program(
        V16CuMarketParams { max_portfolio_assets: 3, ..Default::default() }, 3, program_id);
    env.initialize_order_market_instance(991);
    let mut clock = env.svm.get_sysvar::<Clock>();
    clock.unix_timestamp = 100; clock.slot = 1; env.svm.set_sysvar(&clock);
    env.svm.add_program(adapter, &std::fs::read(std::env::var("NINJA_ADAPTER_TEST_ELF")
        .expect("explicit local adapter artifact")).unwrap());
    let owner = Keypair::new();
    let session = Keypair::new();
    let child = Keypair::new();
    let trigger = Keypair::new();
    env.svm.airdrop(&owner.pubkey(), 1_000_000_000).unwrap();
    env.svm.airdrop(&session.pubkey(), 1_000_000_000).unwrap();
    let portfolio = env.create_portfolio(&owner);
    let grant = sessions::address(&program_id, &owner.pubkey(), &portfolio).0;
    create_session(&mut env, &owner, &session, portfolio, grant, 0).unwrap();
    let auth = Keypair::new();
    let delegate = Pubkey::find_program_address(&[b"ninja-v16", auth.pubkey().as_ref()], &adapter).0;
    reserve_session_ninja_with_key(&mut env, &session, portfolio, grant, 0, delegate, [55; 32], &auth).unwrap();
    let child_grant = Pubkey::find_program_address(&[b"ninja-session", auth.pubkey().as_ref()], &adapter).0;
    let mut data = vec![167, 208, 162, 51, 32, 73, 239, 163];
    data.extend_from_slice(trigger.pubkey().as_ref());
    data.extend_from_slice(&3700i64.to_le_bytes());
    let instruction = Instruction { program_id: adapter, data,
        accounts: vec![
            AccountMeta::new_readonly(owner.pubkey(), false),
            AccountMeta::new(session.pubkey(), true),
            AccountMeta::new_readonly(grant, false),
            AccountMeta::new(child.pubkey(), false),
            AccountMeta::new(child_grant, false),
            AccountMeta::new_readonly(auth.pubkey(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
        ] };
    let before_owner = env.svm.get_account(&owner.pubkey()).unwrap().lamports;
    let before_grant = env.svm.get_account(&grant).unwrap().data;
    let wrong = Keypair::new(); env.svm.airdrop(&wrong.pubkey(), 1_000_000_000).unwrap();
    let mut spoof = instruction.clone(); spoof.accounts[1] = AccountMeta::new(wrong.pubkey(), true);
    assert!(send_raw_tx(&mut env.svm, &env.payer, spoof, &[&wrong]).is_err());
    assert!(env.svm.get_account(&child_grant).is_none());
    let new_result = send_raw_tx(&mut env.svm, &env.payer, instruction.clone(), &[&session]);
    if new_result.is_err() {
        // Diagnostic control: exercise the unchanged owner entrypoint in the
        // same VM. Neither result is accepted as a release pass on failure.
        let mut legacy = instruction.clone();
        legacy.data[..8].copy_from_slice(&[181, 122, 115, 58, 237, 31, 7, 208]);
        legacy.accounts = vec![
            AccountMeta::new(owner.pubkey(), true), AccountMeta::new(child.pubkey(), false),
            AccountMeta::new(child_grant, false), AccountMeta::new_readonly(auth.pubkey(), false),
            AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
        ];
        let control = send_raw_tx(&mut env.svm, &env.payer, legacy, &[&owner]);
        panic!("adapter bridge: {new_result:?}; legacy owner control: {control:?}");
    }
    let child_state = env.svm.get_account(&child_grant).unwrap();
    assert_eq!(child_state.owner, adapter);
    assert_eq!(child_state.data.len(), 212);
    assert_eq!(&child_state.data[12..44], owner.pubkey().as_ref());
    assert_eq!(env.svm.get_account(&child.pubkey()).unwrap().lamports, 10_000_000);
    assert_eq!(env.svm.get_account(&owner.pubkey()).unwrap().lamports, before_owner);
    assert_eq!(env.svm.get_account(&grant).unwrap().data, before_grant);
    assert!(send_raw_tx(&mut env.svm, &env.payer, instruction, &[&session]).is_err());
}

fn create_session(
    env: &mut V16CuEnv,
    owner: &Keypair,
    session: &Keypair,
    portfolio: Pubkey,
    grant: Pubkey,
    epoch: u64,
) -> Result<u64, String> {
    let mut data = vec![sessions::CREATE];
    data.extend_from_slice(&epoch.to_le_bytes());
    send_raw_tx(
        &mut env.svm,
        &env.payer,
        Instruction {
            program_id: env.program_id,
            data,
            accounts: vec![
                AccountMeta::new(owner.pubkey(), true),
                AccountMeta::new(env.market, false),
                AccountMeta::new(portfolio, false),
                AccountMeta::new(grant, false),
                AccountMeta::new_readonly(session.pubkey(), true),
                AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
            ],
        },
        &[owner, session],
    )
}

fn session_state(env: &V16CuEnv, grant: Pubkey) -> SessionGrant {
    let account = env.svm.get_account(&grant).unwrap();
    assert_eq!(account.owner, env.program_id);
    assert_eq!(account.data.len(), SESSION_LEN);
    SessionGrant::decode(&account.data).unwrap()
}

fn session_raw(
    env: &mut V16CuEnv,
    data: Vec<u8>,
    accounts: Vec<AccountMeta>,
    signers: &[&Keypair],
) -> Result<u64, String> {
    send_raw_tx(
        &mut env.svm,
        &env.payer,
        Instruction {
            program_id: env.program_id,
            accounts,
            data,
        },
        signers,
    )
}

#[test]
fn session_runtime_owner_grant_prefunding_revoke_and_epoch_replay() {
    let (mut env, owner, session, portfolio, grant) = session_env();
    env.svm.airdrop(&grant, 1).unwrap(); // Grief-funded PDA still initializes.
    create_session(&mut env, &owner, &session, portfolio, grant, 0).unwrap();
    let first = session_state(&env, grant);
    assert_eq!(first.scope().owner, owner.pubkey().to_bytes());
    assert_eq!(first.scope().signer, session.pubkey().to_bytes());
    assert_eq!(first.scope().domain, sessions::domain());
    assert_eq!(
        Pubkey::new_from_array(first.scope().domain).to_string(),
        "E76TRrtSDk5Nb93nfvTtvfSd6m1SUJNkYc8WfG1ssMaK"
    );
    assert_eq!(first.epoch(), 1);
    assert!(!first.revoked());
    let other = Keypair::new();
    env.svm.airdrop(&other.pubkey(), 1_000_000).unwrap();
    assert!(create_session(&mut env, &owner, &other, portfolio, grant, 0).is_err());
    assert_eq!(session_state(&env, grant), first);
    let mut revoke = vec![sessions::REVOKE];
    revoke.extend_from_slice(&1u64.to_le_bytes());
    assert!(session_raw(
        &mut env,
        revoke.clone(),
        vec![
            AccountMeta::new_readonly(session.pubkey(), true),
            AccountMeta::new(grant, false)
        ],
        &[&session]
    )
    .is_err());
    session_raw(
        &mut env,
        revoke,
        vec![
            AccountMeta::new_readonly(owner.pubkey(), true),
            AccountMeta::new(grant, false),
        ],
        &[&owner],
    )
    .unwrap();
    assert!(session_state(&env, grant).revoked());
    create_session(&mut env, &owner, &other, portfolio, grant, 1).unwrap();
    assert_eq!(session_state(&env, grant).epoch(), 2);
}

fn reserve_session_ninja(
    env: &mut V16CuEnv,
    signer: &Keypair,
    portfolio: Pubkey,
    grant: Pubkey,
    nonce: u64,
    delegate: Pubkey,
    commitment: [u8; 32],
) -> Result<Pubkey, String> {
    let authorization = Keypair::new();
    reserve_session_ninja_with_key(
        env,
        signer,
        portfolio,
        grant,
        nonce,
        delegate,
        commitment,
        &authorization,
    )
}

fn reserve_session_ninja_with_key(
    env: &mut V16CuEnv,
    signer: &Keypair,
    portfolio: Pubkey,
    grant: Pubkey,
    nonce: u64,
    delegate: Pubkey,
    commitment: [u8; 32],
    authorization: &Keypair,
) -> Result<Pubkey, String> {
    let mut data = vec![sessions::AUTHORIZE_NINJA];
    data.extend_from_slice(&1u64.to_le_bytes());
    data.extend_from_slice(&nonce.to_le_bytes());
    data.extend_from_slice(delegate.as_ref());
    data.extend_from_slice(&100u64.to_le_bytes());
    data.extend_from_slice(&commitment);
    // A test-only preallocated account avoids signing any external transaction.
    env.svm
        .set_account(
            authorization.pubkey(),
            Account {
                lamports: 10_000_000,
                data: vec![0; sessions::NINJA_ACCOUNT_LEN],
                owner: env.program_id,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
    session_raw(
        env,
        data,
        vec![
            AccountMeta::new_readonly(signer.pubkey(), true),
            AccountMeta::new_readonly(env.market, false),
            AccountMeta::new_readonly(portfolio, false),
            AccountMeta::new(authorization.pubkey(), true),
            AccountMeta::new(grant, false),
        ],
        &[signer, authorization],
    )?;
    Ok(authorization.pubkey())
}

#[test]
fn session_runtime_ninja_reservation_cancel_and_no_premature_rent_close() {
    let (mut env, owner, session, portfolio, grant) = session_env();
    create_session(&mut env, &owner, &session, portfolio, grant, 0).unwrap();
    let auth = reserve_session_ninja(
        &mut env,
        &session,
        portfolio,
        grant,
        0,
        Pubkey::new_unique(),
        [55; 32],
    )
    .unwrap();
    let reserved = session_state(&env, grant);
    assert_eq!(reserved.reserved_e6(), 100_000_000);
    assert_eq!(reserved.next_nonce(), 1);
    assert!(reserve_session_ninja(
        &mut env,
        &session,
        portfolio,
        grant,
        0,
        Pubkey::new_unique(),
        [55; 32]
    )
    .is_err());
    let other = Keypair::new();
    env.svm.airdrop(&other.pubkey(), 1_000_000).unwrap();
    assert!(create_session(&mut env, &owner, &other, portfolio, grant, 1).is_err());
    assert!(env
        .send(
            ProgInstruction::ClosePrivateOrderAuthorization,
            vec![
                AccountMeta::new_readonly(owner.pubkey(), true),
                AccountMeta::new(auth, false),
                AccountMeta::new(owner.pubkey(), false)
            ],
            &[&owner]
        )
        .is_err());
    let mut cancel = vec![sessions::CANCEL_NINJA];
    cancel.extend_from_slice(&1u64.to_le_bytes());
    cancel.extend_from_slice(&1u64.to_le_bytes());
    session_raw(
        &mut env,
        cancel,
        vec![
            AccountMeta::new_readonly(session.pubkey(), true),
            AccountMeta::new(auth, false),
            AccountMeta::new(grant, false),
        ],
        &[&session],
    )
    .unwrap();
    assert_eq!(session_state(&env, grant).reserved_e6(), 0);
    assert_eq!(session_state(&env, grant).spent_e6(), 0);
    assert_eq!(session_state(&env, grant).next_nonce(), 2);
    assert!(session_raw(
        &mut env,
        vec![sessions::RESOLVE_CANCELLED_NINJA],
        vec![
            AccountMeta::new(auth, false),
            AccountMeta::new(grant, false)
        ],
        &[]
    )
    .is_err());
    env.send(
        ProgInstruction::CloseTerminalPrivateOrderAuthorization,
        vec![
            AccountMeta::new(auth, false),
            AccountMeta::new(owner.pubkey(), false),
        ],
        &[],
    )
    .unwrap();
}

#[test]
fn session_runtime_expiry_wrong_owner_and_malformed_instructions_fail_closed() {
    let (mut env, owner, session, portfolio, grant) = session_env();
    let attacker = Keypair::new();
    env.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    assert!(create_session(&mut env, &attacker, &session, portfolio, grant, 0).is_err());
    create_session(&mut env, &owner, &session, portfolio, grant, 0).unwrap();
    let before = session_state(&env, grant);
    let mut clock = env.svm.get_sysvar::<Clock>();
    clock.unix_timestamp = before.expires_at();
    env.svm.set_sysvar(&clock);
    assert!(reserve_session_ninja(
        &mut env,
        &session,
        portfolio,
        grant,
        0,
        Pubkey::new_unique(),
        [55; 32]
    )
    .is_err());
    assert_eq!(session_state(&env, grant), before);
    for tag in sessions::CREATE..=sessions::RESOLVE_CANCELLED_NINJA {
        assert!(session_raw(&mut env, vec![tag, 255], vec![], &[]).is_err());
    }
}

fn session_trade_data(
    nonce: u64,
    size: i128,
    minimum: u64,
    maximum: u64,
    reduce_only: bool,
) -> Vec<u8> {
    let mut data = vec![sessions::TRADE];
    data.extend_from_slice(&1u64.to_le_bytes());
    data.extend_from_slice(&nonce.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&size.to_le_bytes());
    data.extend_from_slice(&minimum.to_le_bytes());
    data.extend_from_slice(&maximum.to_le_bytes());
    data.extend_from_slice(&100u64.to_le_bytes());
    data.push(u8::from(reduce_only));
    data
}

#[test]
fn session_runtime_real_matcher_fill_charges_shared_budget_and_rolls_back_failures() {
    let (mut env, owner, session, portfolio, grant) = session_env();
    env.deposit(&owner, portfolio, 1_000_000_000);
    let lp = Keypair::new();
    let lp_portfolio = env.create_portfolio(&lp);
    env.deposit(&lp, lp_portfolio, 1_000_000_000);
    let matcher = Pubkey::new_unique();
    env.svm.add_program(
        matcher,
        &std::fs::read(auth_matcher_program_path()).unwrap(),
    );
    let (context, delegate, _) = env.init_auth_matcher_context(matcher, &lp, lp_portfolio);
    create_session(&mut env, &owner, &session, portfolio, grant, 0).unwrap();
    let metas = vec![
        AccountMeta::new_readonly(session.pubkey(), true),
        AccountMeta::new(env.market, false),
        AccountMeta::new(portfolio, false),
        AccountMeta::new(lp_portfolio, false),
        AccountMeta::new_readonly(matcher, false),
        AccountMeta::new(context, false),
        AccountMeta::new_readonly(delegate, false),
        AccountMeta::new(grant, false),
        AccountMeta::new_readonly(owner.pubkey(), false),
    ];
    let q = 10 * POS_SCALE as i128;
    let opening_cu = session_raw(
        &mut env,
        session_trade_data(0, q, 90, 110, false),
        metas.clone(),
        &[&session],
    )
    .unwrap();
    assert!(opening_cu < 750_000, "session trade CU {opening_cu}");
    assert_eq!(session_state(&env, grant).spent_e6(), 1000);
    let before_grant = session_state(&env, grant);
    let before_portfolio = env.svm.get_account(&portfolio).unwrap().data;
    // Replay, excessive quantity, fake reduction and impossible price bounds.
    for data in [
        session_trade_data(0, q, 90, 110, false),
        session_trade_data(1, i128::MAX, 90, 110, false),
        session_trade_data(1, q, 90, 110, true),
        session_trade_data(1, -q, 101, 110, true),
    ] {
        assert!(session_raw(&mut env, data, metas.clone(), &[&session]).is_err());
        assert_eq!(session_state(&env, grant), before_grant);
        assert_eq!(
            env.svm.get_account(&portfolio).unwrap().data,
            before_portfolio
        );
    }
    session_raw(
        &mut env,
        session_trade_data(1, -q, 90, 110, true),
        metas.clone(),
        &[&session],
    )
    .unwrap();
    assert_eq!(session_state(&env, grant).spent_e6(), 2000);
    assert!(!has_active_leg_for_asset(
        &env.portfolio_state(portfolio),
        0
    ));
    reserve_session_ninja(
        &mut env,
        &session,
        portfolio,
        grant,
        2,
        Pubkey::new_unique(),
        [55; 32],
    )
    .unwrap();
    assert_eq!(session_state(&env, grant).spent_e6(), 2000);
    assert_eq!(session_state(&env, grant).reserved_e6(), 100_000_000);
    let mut forged = env.svm.get_account(&grant).unwrap();
    // Account-owner validation, independent of a byte-perfect copy.
    forged.owner = solana_sdk::system_program::ID;
    env.svm.set_account(grant, forged).unwrap();
    assert!(session_raw(
        &mut env,
        session_trade_data(3, q, 90, 110, false),
        metas,
        &[&session]
    )
    .is_err());
}

#[test]
fn session_runtime_private_settlement_is_bound_capped_atomic_and_survives_session_expiry() {
    let (mut env, owner, session, portfolio, grant) = session_env();
    env.deposit(&owner, portfolio, 1_000_000_000);
    let lp = Keypair::new();
    let lp_portfolio = env.create_portfolio(&lp);
    env.deposit(&lp, lp_portfolio, 1_000_000_000);
    let matcher = Pubkey::new_unique();
    env.svm.add_program(
        matcher,
        &std::fs::read(auth_matcher_program_path()).unwrap(),
    );
    let (context, matcher_delegate, _) = env.init_auth_matcher_context(matcher, &lp, lp_portfolio);
    let trigger = Keypair::new();
    env.svm.airdrop(&trigger.pubkey(), 1_000_000).unwrap();
    create_session(&mut env, &owner, &session, portfolio, grant, 0).unwrap();
    let asset_market_id = env.market_state().1.assets[0].market_id;
    for (nonce, size) in [(0u64, 1_000_001_000_000i128), (1, 10 * POS_SCALE as i128)] {
        let auth = Keypair::new();
        let reveal = ninja_reveal(
            env.market,
            portfolio,
            owner.pubkey(),
            trigger.pubkey(),
            auth.pubkey(),
            100,
            1,
            [
                OrderAuthorizationBranch {
                    asset_index: 0,
                    asset_market_id,
                    size_q: size,
                    max_fee_bps: 100,
                    trigger_price_e6: 100,
                    limit_price_e6: 110,
                    trigger_condition: ORDER_TRIGGER_AT_OR_BELOW,
                    reduce_only: 0,
                },
                OrderAuthorizationBranch::default(),
            ],
        );
        let commitment = ninja_order_commitment::ninja_order_commitment_v1(&reveal).unwrap();
        reserve_session_ninja_with_key(
            &mut env,
            &session,
            portfolio,
            grant,
            nonce,
            trigger.pubkey(),
            commitment,
            &auth,
        )
        .unwrap();
        let mut data = vec![77, 0];
        data.extend_from_slice(&reveal);
        let metas = vec![
            AccountMeta::new_readonly(trigger.pubkey(), true),
            AccountMeta::new(env.market, false),
            AccountMeta::new(portfolio, false),
            AccountMeta::new(lp_portfolio, false),
            AccountMeta::new_readonly(matcher, false),
            AccountMeta::new(context, false),
            AccountMeta::new_readonly(matcher_delegate, false),
            AccountMeta::new(auth.pubkey(), false),
            AccountMeta::new(owner.pubkey(), false),
            AccountMeta::new(grant, false),
        ];
        let reserved = session_state(&env, grant);
        let before = env.svm.get_account(&portfolio).unwrap().data;
        assert!(session_raw(&mut env, data.clone(), metas[..9].to_vec(), &[&trigger]).is_err());
        assert_eq!(session_state(&env, grant), reserved);
        if nonce == 0 {
            assert!(
                session_raw(&mut env, data, metas, &[&trigger]).is_err(),
                "private fill cannot evade $100 cap"
            );
            assert_eq!(session_state(&env, grant), reserved);
            assert_eq!(env.svm.get_account(&portfolio).unwrap().data, before);
            env.send(
                ProgInstruction::RevokePrivateOrder,
                vec![
                    AccountMeta::new_readonly(owner.pubkey(), true),
                    AccountMeta::new(auth.pubkey(), false),
                ],
                &[&owner],
            )
            .unwrap();
            assert!(env
                .send(
                    ProgInstruction::CloseTerminalPrivateOrderAuthorization,
                    vec![
                        AccountMeta::new(auth.pubkey(), false),
                        AccountMeta::new(owner.pubkey(), false)
                    ],
                    &[]
                )
                .is_err());
            session_raw(
                &mut env,
                vec![sessions::RESOLVE_CANCELLED_NINJA],
                vec![
                    AccountMeta::new(auth.pubkey(), false),
                    AccountMeta::new(grant, false),
                ],
                &[],
            )
            .unwrap();
        } else {
            let mut clock = env.svm.get_sysvar::<Clock>();
            clock.unix_timestamp = reserved.expires_at();
            env.svm.set_sysvar(&clock);
            session_raw(&mut env, data, metas, &[&trigger]).unwrap();
            assert_eq!(session_state(&env, grant).spent_e6(), 1000);
            assert_eq!(session_state(&env, grant).reserved_e6(), 0);
            assert!(has_active_leg_for_asset(&env.portfolio_state(portfolio), 0));
            assert!(env
                .svm
                .get_account(&auth.pubkey())
                .is_none_or(|a| a.lamports == 0));
        }
    }
}
