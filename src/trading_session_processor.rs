// Included inside processor so the new authority boundary reuses the existing
// matcher, price acceptance and financial engine rather than duplicating them.
use super::*;
use crate::trading_session::{self as policy, Action, Fill, Grant, Operation, Scope};

pub const GRANT_SEED: &[u8] = b"trading-session-v1";
pub const CREATE: u8 = 83;
pub const REVOKE: u8 = 84;
pub const TRADE: u8 = 85;
pub const AUTHORIZE_NINJA: u8 = 86;
pub const CANCEL_NINJA: u8 = 87;
pub const RESOLVE_CANCELLED_NINJA: u8 = 88;
pub const NINJA_ACCOUNT_LEN: usize = 320;
const NINJA_EXTENSION_MAGIC: &[u8; 8] = b"MUKNSES1";

#[derive(Clone, Copy)]
pub struct NinjaBinding {
    pub grant: Pubkey,
    pub epoch: u64,
    pub nonce: u64,
    pub resolved: bool,
}

pub fn ninja_binding(account: &AccountInfo) -> Result<Option<NinjaBinding>, ProgramError> {
    let data = account.try_borrow_data()?;
    if data.len() == state::private_order_authorization_account_len() {
        return Ok(None);
    }
    if data.len() != NINJA_ACCOUNT_LEN
        || &data[256..264] != NINJA_EXTENSION_MAGIC
        || data[312] > 1
        || data[313..].iter().any(|b| *b != 0)
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let grant = Pubkey::new_from_array(data[264..296].try_into().unwrap());
    let epoch = number(&data, 296)?;
    if grant == Pubkey::default() || epoch == 0 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(Some(NinjaBinding {
        grant,
        epoch,
        nonce: number(&data, 304)?,
        resolved: data[312] == 1,
    }))
}

fn write_binding(account: &AccountInfo, binding: NinjaBinding) -> ProgramResult {
    expect_writable(account)?;
    let mut data = account.try_borrow_mut_data()?;
    if data.len() != NINJA_ACCOUNT_LEN {
        return Err(ProgramError::InvalidAccountData);
    }
    data[256..264].copy_from_slice(NINJA_EXTENSION_MAGIC);
    data[264..296].copy_from_slice(binding.grant.as_ref());
    data[296..304].copy_from_slice(&binding.epoch.to_le_bytes());
    data[304..312].copy_from_slice(&binding.nonce.to_le_bytes());
    data[312] = u8::from(binding.resolved);
    data[313..].fill(0);
    Ok(())
}

pub fn ensure_ninja_resolved(account: &AccountInfo) -> ProgramResult {
    if ninja_binding(account)?.is_some_and(|b| !b.resolved) {
        return Err(PercolatorError::Unauthorized.into());
    }
    Ok(())
}

pub fn domain() -> [u8; 32] {
    // Release-bound devnet domain. Client must independently verify RPC genesis.
    solana_program::hash::hashv(&[
        b"mukon-trading-session-v1",
        b"EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
    ])
    .to_bytes()
}

fn denied(_: policy::Error) -> ProgramError {
    PercolatorError::Unauthorized.into()
}

pub fn address(program: &Pubkey, owner: &Pubkey, portfolio: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GRANT_SEED, owner.as_ref(), portfolio.as_ref()], program)
}

#[inline(never)]
pub fn load(program: &Pubkey, account: &AccountInfo) -> Result<Grant, ProgramError> {
    expect_owner(account, program)?;
    if account.executable {
        return Err(ProgramError::InvalidAccountData);
    }
    let grant = Grant::decode(&account.try_borrow_data()?).map_err(denied)?;
    let scope = grant.scope();
    if scope.program != program.to_bytes() || scope.domain != domain() {
        return Err(PercolatorError::Unauthorized.into());
    }
    let (expected, _) = address(
        program,
        &Pubkey::new_from_array(scope.owner),
        &Pubkey::new_from_array(scope.portfolio),
    );
    expect_key(account, &expected)?;
    Ok(grant)
}

#[inline(never)]
fn save(account: &AccountInfo, grant: Grant) -> ProgramResult {
    expect_writable(account)?;
    let mut bytes = account.try_borrow_mut_data()?;
    if bytes.len() != policy::ACCOUNT_LEN {
        return Err(ProgramError::InvalidAccountData);
    }
    bytes.copy_from_slice(&grant.encode());
    Ok(())
}

fn current_scope(
    program: &Pubkey,
    market: &AccountInfo,
    portfolio: &AccountInfo,
    signer: [u8; 32],
) -> Result<Scope, ProgramError> {
    expect_owner(market, program)?;
    expect_owner(portfolio, program)?;
    let market_bytes = market.try_borrow_data()?;
    let portfolio_bytes = portfolio.try_borrow_data()?;
    let (header, owner) = state::read_portfolio_owner_preflight(&portfolio_bytes)?;
    if header.market_group_id != market.key.to_bytes()
        || header.portfolio_account_id != portfolio.key.to_bytes()
    {
        return Err(PercolatorError::Unauthorized.into());
    }
    Ok(Scope {
        owner,
        signer,
        program: program.to_bytes(),
        domain: domain(),
        market: market.key.to_bytes(),
        portfolio: portfolio.key.to_bytes(),
        market_instance: state::read_order_market_instance_id(&market_bytes)?,
        portfolio_instance: state::read_portfolio_instance_id(&portfolio_bytes)?,
        asset_market_ids: [
            state::read_asset_market_id(&market_bytes, 0)?,
            state::read_asset_market_id(&market_bytes, 1)?,
            state::read_asset_market_id(&market_bytes, 2)?,
        ],
    })
}

fn number(data: &[u8], offset: usize) -> Result<u64, ProgramError> {
    let bytes: [u8; 8] = data
        .get(offset..offset + 8)
        .ok_or(ProgramError::InvalidInstructionData)?
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;
    Ok(u64::from_le_bytes(bytes))
}

#[inline(never)]
pub fn process<'a>(
    program: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    data: &[u8],
) -> ProgramResult {
    match data.first().copied() {
        Some(CREATE) if data.len() == 9 => create(program, accounts, number(data, 1)?),
        Some(REVOKE) if data.len() == 9 => revoke(program, accounts, number(data, 1)?),
        Some(TRADE) if data.len() == 60 => trade(program, accounts, data),
        Some(AUTHORIZE_NINJA) if data.len() == 89 => authorize_ninja(program, accounts, data),
        Some(CANCEL_NINJA) if data.len() == 17 => cancel_ninja(program, accounts, data),
        Some(RESOLVE_CANCELLED_NINJA) if data.len() == 1 => {
            resolve_cancelled_ninja(program, accounts)
        }
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

// Session signer, market, portfolio, newly allocated authorization signer,
// grant. No hidden terms are placed in the public instruction or grant.
#[inline(never)]
fn authorize_ninja<'a>(
    program: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    data: &[u8],
) -> ProgramResult {
    if accounts.len() != 5 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let signer = account(accounts, 0)?;
    let market = account(accounts, 1)?;
    let portfolio = account(accounts, 2)?;
    let authorization_ai = account(accounts, 3)?;
    let grant_ai = account(accounts, 4)?;
    expect_signer(signer)?;
    expect_signer(authorization_ai)?;
    expect_owner(authorization_ai, program)?;
    expect_writable(authorization_ai)?;
    expect_writable(grant_ai)?;
    let grant = load(program, grant_ai)?;
    let scope = current_scope(program, market, portfolio, signer.key.to_bytes())?;
    let clock = Clock::get()?;
    let action = Action {
        scope,
        epoch: number(data, 1)?,
        nonce: number(data, 9)?,
        now: clock.unix_timestamp,
        operation: Operation::NinjaCreate,
        asset_index: 0,
        asset_market_id: 0,
    };
    let next = grant
        .reserve_ninja(&action, authorization_ai.key.to_bytes())
        .map_err(denied)?;
    let delegate: [u8; 32] = data[17..49].try_into().unwrap();
    let expiry_slot = number(data, 49)?;
    let commitment: [u8; 32] = data[57..89].try_into().unwrap();
    if delegate == [0; 32] || commitment == [0; 32] || expiry_slot <= clock.slot {
        return Err(ProgramError::InvalidInstructionData);
    }
    {
        let bytes = authorization_ai.try_borrow_data()?;
        if bytes.len() != NINJA_ACCOUNT_LEN || bytes.iter().any(|b| *b != 0) {
            return Err(ProgramError::InvalidAccountData);
        }
    }
    state::init_private_order_authorization_account(
        &mut authorization_ai.try_borrow_mut_data()?,
        &state::PrivateOrderAuthorizationV16 {
            commitment,
            market_group: scope.market,
            portfolio: scope.portfolio,
            owner: scope.owner,
            delegate,
            authorization_account_id: authorization_ai.key.to_bytes(),
            expiry_slot,
            created_slot: clock.slot,
            market_instance_id: scope.market_instance,
            portfolio_instance_id: scope.portfolio_instance,
            state: constants::ORDER_AUTHORIZATION_STATE_ACTIVE,
            _reserved: [0; 15],
        },
    )?;
    write_binding(
        authorization_ai,
        NinjaBinding {
            grant: *grant_ai.key,
            epoch: action.epoch,
            nonce: action.nonce,
            resolved: false,
        },
    )?;
    save(grant_ai, next)
}

// The new session may cancel only its own exact outstanding reservation.
#[inline(never)]
fn cancel_ninja<'a>(
    program: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    data: &[u8],
) -> ProgramResult {
    if accounts.len() != 3 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let signer = account(accounts, 0)?;
    let authorization_ai = account(accounts, 1)?;
    let grant_ai = account(accounts, 2)?;
    expect_signer(signer)?;
    expect_owner(authorization_ai, program)?;
    expect_writable(authorization_ai)?;
    let binding = ninja_binding(authorization_ai)?.ok_or(ProgramError::InvalidAccountData)?;
    expect_key(grant_ai, &binding.grant)?;
    let grant = load(program, grant_ai)?;
    let mut scope = grant.scope();
    scope.signer = signer.key.to_bytes();
    let action = Action {
        scope,
        epoch: number(data, 1)?,
        nonce: number(data, 9)?,
        now: Clock::get()?.unix_timestamp,
        operation: Operation::NinjaCancel,
        asset_index: 0,
        asset_market_id: 0,
    };
    let mut auth = state::read_private_order_authorization(&authorization_ai.try_borrow_data()?)?;
    validate_binding(&auth, authorization_ai, &binding, &grant)?;
    if auth.state != constants::ORDER_AUTHORIZATION_STATE_ACTIVE {
        return Err(PercolatorError::Unauthorized.into());
    }
    let next = grant
        .request_ninja_cancel(&action, authorization_ai.key.to_bytes(), binding.nonce)
        .map_err(denied)?;
    auth.state = constants::ORDER_AUTHORIZATION_STATE_REVOKED;
    state::write_private_order_authorization(&mut authorization_ai.try_borrow_mut_data()?, &auth)?;
    save(grant_ai, next)?;
    resolve_ninja(program, authorization_ai, grant_ai, 0, 0, None)
}

fn validate_binding(
    auth: &state::PrivateOrderAuthorizationV16,
    authorization_ai: &AccountInfo,
    binding: &NinjaBinding,
    grant: &Grant,
) -> ProgramResult {
    let scope = grant.scope();
    if binding.resolved
        || binding.epoch != grant.epoch()
        || auth.authorization_account_id != authorization_ai.key.to_bytes()
        || auth.owner != scope.owner
        || auth.market_group != scope.market
        || auth.portfolio != scope.portfolio
        || auth.market_instance_id != scope.market_instance
        || auth.portfolio_instance_id != scope.portfolio_instance
    {
        return Err(PercolatorError::Unauthorized.into());
    }
    Ok(())
}

#[inline(never)]
pub fn resolve_ninja(
    program: &Pubkey,
    authorization_ai: &AccountInfo,
    grant_ai: &AccountInfo,
    asset_index: u16,
    asset_market_id: u64,
    fill: Option<&Fill>,
) -> ProgramResult {
    expect_owner(authorization_ai, program)?;
    let binding = ninja_binding(authorization_ai)?.ok_or(ProgramError::InvalidAccountData)?;
    expect_key(grant_ai, &binding.grant)?;
    let grant = load(program, grant_ai)?;
    let auth = state::read_private_order_authorization(&authorization_ai.try_borrow_data()?)?;
    validate_binding(&auth, authorization_ai, &binding, &grant)?;
    if (fill.is_some() && auth.state != constants::ORDER_AUTHORIZATION_STATE_CONSUMED)
        || (fill.is_none() && auth.state != constants::ORDER_AUTHORIZATION_STATE_REVOKED)
    {
        return Err(PercolatorError::Unauthorized.into());
    }
    let next = grant
        .resolve_ninja(
            &grant.scope(),
            binding.epoch,
            authorization_ai.key.to_bytes(),
            binding.nonce,
            asset_index,
            asset_market_id,
            fill,
        )
        .map_err(denied)?;
    save(grant_ai, next)?;
    write_binding(
        authorization_ai,
        NinjaBinding {
            resolved: true,
            ..binding
        },
    )
}

#[inline(never)]
fn resolve_cancelled_ninja<'a>(program: &Pubkey, accounts: &'a [AccountInfo<'a>]) -> ProgramResult {
    if accounts.len() != 2 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    resolve_ninja(
        program,
        account(accounts, 0)?,
        account(accounts, 1)?,
        0,
        0,
        None,
    )
}

// Accounts: owner signer/payer, market, portfolio, canonical grant, new session
// signer, System Program. Owner consents to the fixed limits; funding the session
// fee allowance is a separate explicit System transfer in the owner's transaction.
#[inline(never)]
fn create<'a>(
    program: &Pubkey,
    accounts: &'a [AccountInfo<'a>],
    expected_epoch: u64,
) -> ProgramResult {
    if accounts.len() != 6 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let owner = account(accounts, 0)?;
    let market = account(accounts, 1)?;
    let portfolio = account(accounts, 2)?;
    let grant_ai = account(accounts, 3)?;
    let session = account(accounts, 4)?;
    let system = account(accounts, 5)?;
    expect_signer(owner)?;
    expect_signer(session)?;
    for ai in [owner, market, portfolio, grant_ai] {
        expect_writable(ai)?;
    }
    expect_key(system, &system_program::ID)?;
    expect_owner(market, program)?;
    expect_owner(portfolio, program)?;
    expect_owner(session, &system_program::ID)?;
    if session.executable
        || !session.data_is_empty()
        || session.key == owner.key
        || [market.key, portfolio.key, grant_ai.key].contains(&session.key)
    {
        return Err(PercolatorError::Unauthorized.into());
    }
    let (header, actual_owner) =
        state::read_portfolio_owner_preflight(&portfolio.try_borrow_data()?)?;
    if actual_owner != owner.key.to_bytes()
        || header.market_group_id != market.key.to_bytes()
        || header.portfolio_account_id != portfolio.key.to_bytes()
    {
        return Err(PercolatorError::Unauthorized.into());
    }
    let (expected, bump) = address(program, owner.key, portfolio.key);
    expect_key(grant_ai, &expected)?;
    ensure_order_portfolio_instance(market, portfolio)?;
    let scope = current_scope(program, market, portfolio, session.key.to_bytes())?;
    let now = Clock::get()?.unix_timestamp;
    if grant_ai.owner == program {
        return renew_session(program, grant_ai, scope, expected_epoch, now);
    }
    if expected_epoch != 0
        || grant_ai.owner != &system_program::ID
        || !grant_ai.data_is_empty()
        || grant_ai.executable
    {
        return Err(PercolatorError::Unauthorized.into());
    }
    let grant = Grant::new(scope, 1, now).map_err(denied)?;
    let rent = solana_program::rent::Rent::get()?.minimum_balance(policy::ACCOUNT_LEN);
    let deficit = rent.saturating_sub(grant_ai.lamports());
    if deficit > 0 {
        invoke(
            &solana_program::system_instruction::transfer(owner.key, grant_ai.key, deficit),
            &[owner.clone(), grant_ai.clone(), system.clone()],
        )?;
    }
    let seeds: &[&[u8]] = &[
        GRANT_SEED,
        owner.key.as_ref(),
        portfolio.key.as_ref(),
        &[bump],
    ];
    invoke_signed(
        &solana_program::system_instruction::allocate(grant_ai.key, policy::ACCOUNT_LEN as u64),
        &[grant_ai.clone(), system.clone()],
        &[seeds],
    )?;
    invoke_signed(
        &solana_program::system_instruction::assign(grant_ai.key, program),
        &[grant_ai.clone(), system.clone()],
        &[seeds],
    )?;
    save(grant_ai, grant)
}

#[inline(never)]
fn renew_session(
    program: &Pubkey,
    account: &AccountInfo,
    scope: Scope,
    epoch: u64,
    now: i64,
) -> ProgramResult {
    let old = load(program, account)?;
    save(
        account,
        old.renew_for_scope(scope.owner, scope, epoch, now)
            .map_err(denied)?,
    )
}

// Persistent grant is never closed: epochs cannot reset through close/recreate.
#[inline(never)]
fn revoke<'a>(program: &Pubkey, accounts: &'a [AccountInfo<'a>], epoch: u64) -> ProgramResult {
    if accounts.len() != 2 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let owner = account(accounts, 0)?;
    let grant_ai = account(accounts, 1)?;
    expect_signer(owner)?;
    let grant = load(program, grant_ai)?
        .revoke(owner.key.to_bytes(), epoch)
        .map_err(denied)?;
    save(grant_ai, grant)
}

// Standard 7 matcher accounts, grant, real owner, then validated matcher tail.
// Bytes: tag, epoch:u64, nonce:u64, asset:u16, size:i128, min/max:u64, maxFee:u64,
// reduceOnly:u8. Both price bounds are mandatory, including a sell upper bound.
#[inline(never)]
fn trade<'a>(program: &Pubkey, accounts: &'a [AccountInfo<'a>], data: &[u8]) -> ProgramResult {
    let signer = account(accounts, 0)?;
    let market = account(accounts, 1)?;
    let portfolio = account(accounts, 2)?;
    let grant_ai = account(accounts, 7)?;
    let owner_ai = account(accounts, 8)?;
    expect_signer(signer)?;
    expect_writable(grant_ai)?;
    let grant = load(program, grant_ai)?;
    let scope = current_scope(program, market, portfolio, signer.key.to_bytes())?;
    expect_key(owner_ai, &Pubkey::new_from_array(scope.owner))?;
    let asset_index = u16::from_le_bytes(data[17..19].try_into().unwrap());
    let size = i128::from_le_bytes(data[19..35].try_into().unwrap());
    let minimum = number(data, 35)?;
    let maximum = number(data, 43)?;
    let fee_cap = number(data, 51)?;
    if data[59] > 1
        || minimum == 0
        || maximum < minimum
        || fee_cap > 10_000
        || size == 0
        || size == i128::MIN
    {
        return Err(ProgramError::InvalidInstructionData);
    }
    let action = Action {
        scope,
        epoch: number(data, 1)?,
        nonce: number(data, 9)?,
        now: Clock::get()?.unix_timestamp,
        operation: Operation::MarketTrade,
        asset_index,
        asset_market_id: *scope
            .asset_market_ids
            .get(asset_index as usize)
            .ok_or(ProgramError::InvalidInstructionData)?,
    };
    grant
        .validate_action(&action, Operation::MarketTrade)
        .map_err(denied)?;
    // Upper execution-price bound makes this a conservative preflight. Actual
    // accounting below charges the accepted engine price, not the estimate.
    let worst = Fill {
        requested_q: size,
        filled_q: size,
        price_e6: maximum,
        minimum_price_e6: minimum,
        maximum_price_e6: maximum,
        fee_bps: fee_cap,
        maximum_fee_bps: fee_cap,
        position_before_q: -size,
        reduce_only: data[59] == 1,
    };
    grant
        .validate_market_fill(&action, &worst)
        .map_err(denied)?;
    if worst.reduce_only {
        validate_reduce_only_order(
            market,
            portfolio,
            &state::OrderAuthorizationBranchV16 {
                size_q: size,
                asset_index,
                reduce_only: 1,
                ..Default::default()
            },
        )?;
    }
    let mut receipt = None;
    let filled = execute_trade_cpi_scoped(
        program,
        accounts,
        &Pubkey::new_from_array(scope.owner),
        2,
        asset_index,
        size,
        5, // Preserve the existing frontend taker fee; the user's fee cap still applies.
        if size > 0 { maximum } else { minimum },
        Some(fee_cap),
        Some(&mut receipt),
    )?;
    if !filled {
        return Err(PercolatorError::InvalidInstruction.into());
    }
    let receipt = receipt.ok_or(ProgramError::InvalidAccountData)?;
    let actual = Fill {
        filled_q: receipt.size_q,
        price_e6: receipt.price_e6,
        fee_bps: receipt.fee_bps,
        ..worst
    };
    charge_market(grant_ai, &grant, &action, &actual)
}

#[inline(never)]
fn charge_market(
    account: &AccountInfo,
    grant: &Grant,
    action: &Action,
    fill: &Fill,
) -> ProgramResult {
    save(account, grant.market_fill(action, fill).map_err(denied)?)
}
