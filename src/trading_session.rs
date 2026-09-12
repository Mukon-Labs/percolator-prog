//! Checked policy core for general trading sessions. NOT an instruction handler.
//!
//! The consuming program must authenticate accounts, signers, Clock, canonical
//! PDAs and lifecycle results before calling this module, and persist the result
//! atomically with execution. Nothing here changes the existing owner-only ABI.

pub const DURATION_SECONDS: i64 = 3_600;
pub const PER_ACTION_NOTIONAL_E6: u64 = 100_000_000;
pub const TOTAL_NOTIONAL_E6: u64 = 1_000_000_000;
pub const MAX_PENDING: usize = 10;
pub const MAX_ACTIONS: u64 = 100;
pub const QUANTITY_SCALE: u128 = 1_000_000;
pub const ACCOUNT_LEN: usize = 704;
pub const ACCOUNT_MAGIC: &[u8; 8] = b"MUKSESS1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Identity,
    Expired,
    Revoked,
    Replay,
    Forbidden,
    Amount,
    Limit,
    Overflow,
    Pending,
    Reservation,
}

/// All identities must be obtained from verified program-owned state, not a
/// caller's unchecked instruction arguments. Domain is a release-bound value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scope {
    pub owner: [u8; 32],
    pub signer: [u8; 32],
    pub program: [u8; 32],
    pub domain: [u8; 32],
    pub market: [u8; 32],
    pub portfolio: [u8; 32],
    pub market_instance: u64,
    pub portfolio_instance: u64,
    pub asset_market_ids: [u64; 3],
}

impl Scope {
    fn validate(&self) -> Result<(), Error> {
        if [
            self.owner,
            self.signer,
            self.program,
            self.domain,
            self.market,
            self.portfolio,
        ]
        .iter()
        .any(|key| *key == [0; 32])
            || self.owner == self.signer
            || self.market == self.portfolio
            || self.market_instance == 0
            || self.portfolio_instance == 0
            || self.asset_market_ids.iter().any(|id| *id == 0)
        {
            return Err(Error::Identity);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    MarketTrade,
    NinjaCreate,
    NinjaCancel,
    Withdraw,
    Transfer,
    Admin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    pub scope: Scope,
    pub epoch: u64,
    pub nonce: u64,
    pub now: i64,
    pub operation: Operation,
    /// Market trades only. Both fields MUST be zero on private create/cancel;
    /// the committed reveal's market is checked when the order settles.
    pub asset_index: u16,
    pub asset_market_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Reservation {
    authorization: [u8; 32],
    nonce: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    scope: Scope,
    epoch: u64,
    created_at: i64,
    expires_at: i64,
    revoked: bool,
    next_nonce: u64,
    spent_e6: u64,
    reserved_e6: u64,
    reservations: [Option<Reservation>; MAX_PENDING],
}

/// Request price bounds are checked against the *accepted execution price*,
/// including for sells: a sell minimum alone is not an upper notional bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub requested_q: i128,
    pub filled_q: i128,
    pub price_e6: u64,
    pub minimum_price_e6: u64,
    pub maximum_price_e6: u64,
    pub fee_bps: u64,
    pub maximum_fee_bps: u64,
    pub position_before_q: i128,
    pub reduce_only: bool,
}

pub fn notional_ceil(q: i128, price_e6: u64) -> Result<u64, Error> {
    if q == 0 || q == i128::MIN || price_e6 == 0 {
        return Err(Error::Amount);
    }
    let numerator = q
        .unsigned_abs()
        .checked_mul(u128::from(price_e6))
        .ok_or(Error::Overflow)?;
    let rounded = numerator
        .checked_add(QUANTITY_SCALE - 1)
        .ok_or(Error::Overflow)?
        / QUANTITY_SCALE;
    u64::try_from(rounded).map_err(|_| Error::Overflow)
}

impl Fill {
    pub fn notional(&self) -> Result<u64, Error> {
        if self.requested_q == 0
            || self.requested_q == i128::MIN
            || self.filled_q == 0
            || self.filled_q != self.requested_q
            || self.minimum_price_e6 == 0
            || self.maximum_price_e6 < self.minimum_price_e6
            || self.price_e6 < self.minimum_price_e6
            || self.price_e6 > self.maximum_price_e6
            || self.maximum_fee_bps > 10_000
            || self.fee_bps > self.maximum_fee_bps
        {
            return Err(Error::Limit);
        }
        if self.reduce_only
            && (self.position_before_q == 0
                || self.position_before_q == i128::MIN
                || self.filled_q.signum() == self.position_before_q.signum()
                || self.filled_q.unsigned_abs() > self.position_before_q.unsigned_abs())
        {
            return Err(Error::Forbidden);
        }
        let notional = notional_ceil(self.filled_q, self.price_e6)?;
        if notional > PER_ACTION_NOTIONAL_E6 {
            return Err(Error::Limit);
        }
        Ok(notional)
    }
}

impl Grant {
    pub fn scope(&self) -> Scope {
        self.scope
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn revoked(&self) -> bool {
        self.revoked
    }

    /// Fixed, canonical wire format shared by the program and client. Reserved
    /// bytes and unused reservation entries must be zero, not merely ignored.
    #[inline(never)]
    pub fn encode(&self) -> [u8; ACCOUNT_LEN] {
        let mut out = [0u8; ACCOUNT_LEN];
        out[..8].copy_from_slice(ACCOUNT_MAGIC);
        let mut offset = 8;
        for key in [
            self.scope.owner,
            self.scope.signer,
            self.scope.program,
            self.scope.domain,
            self.scope.market,
            self.scope.portfolio,
        ] {
            out[offset..offset + 32].copy_from_slice(&key);
            offset += 32;
        }
        for value in [
            self.scope.market_instance,
            self.scope.portfolio_instance,
            self.scope.asset_market_ids[0],
            self.scope.asset_market_ids[1],
            self.scope.asset_market_ids[2],
            self.epoch,
            self.created_at as u64,
            self.expires_at as u64,
            self.next_nonce,
            self.spent_e6,
            self.reserved_e6,
        ] {
            out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            offset += 8;
        }
        out[288] = u8::from(self.revoked);
        for (i, reservation) in self.reservations.iter().enumerate() {
            if let Some(r) = reservation {
                let start = 296 + i * 40;
                out[start..start + 32].copy_from_slice(&r.authorization);
                out[start + 32..start + 40].copy_from_slice(&r.nonce.to_le_bytes());
            }
        }
        out
    }

    #[inline(never)]
    pub fn decode(data: &[u8]) -> Result<Self, Error> {
        if data.len() != ACCOUNT_LEN
            || &data[..8] != ACCOUNT_MAGIC
            || data[288] > 1
            || data[289..296].iter().any(|b| *b != 0)
            || data[696..].iter().any(|b| *b != 0)
        {
            return Err(Error::Identity);
        }
        let key = |o: usize| -> [u8; 32] { data[o..o + 32].try_into().unwrap() };
        let number = |o: usize| -> u64 { u64::from_le_bytes(data[o..o + 8].try_into().unwrap()) };
        let scope = Scope {
            owner: key(8),
            signer: key(40),
            program: key(72),
            domain: key(104),
            market: key(136),
            portfolio: key(168),
            market_instance: number(200),
            portfolio_instance: number(208),
            asset_market_ids: [number(216), number(224), number(232)],
        };
        let created_at = i64::try_from(number(248)).map_err(|_| Error::Identity)?;
        let mut grant = Self::new(scope, number(240), created_at)?;
        if grant.expires_at != i64::try_from(number(256)).map_err(|_| Error::Identity)? {
            return Err(Error::Identity);
        }
        grant.revoked = data[288] == 1;
        grant.next_nonce = number(264);
        grant.spent_e6 = number(272);
        grant.reserved_e6 = number(280);
        if grant.next_nonce > MAX_ACTIONS {
            return Err(Error::Limit);
        }
        let mut count = 0u64;
        for i in 0..MAX_PENDING {
            let start = 296 + i * 40;
            let authorization = key(start);
            let nonce = number(start + 32);
            if authorization == [0; 32] {
                if nonce != 0 {
                    return Err(Error::Reservation);
                }
            } else {
                if nonce >= grant.next_nonce
                    || [scope.owner, scope.signer, scope.market, scope.portfolio]
                        .contains(&authorization)
                    || grant
                        .reservations
                        .iter()
                        .flatten()
                        .any(|r| r.authorization == authorization || r.nonce == nonce)
                {
                    return Err(Error::Reservation);
                }
                grant.reservations[i] = Some(Reservation {
                    authorization,
                    nonce,
                });
                count += 1;
            }
        }
        if grant.reserved_e6 != count * PER_ACTION_NOTIONAL_E6 {
            return Err(Error::Reservation);
        }
        grant.ensure_capacity(0)?;
        Ok(grant)
    }

    pub fn validate_action(&self, action: &Action, operation: Operation) -> Result<(), Error> {
        self.check(action, operation)
    }

    /// Owner verification is a mandatory caller precondition. `epoch` comes
    /// from persistent owner-grant state and must never be reset on closure.
    #[inline(never)]
    pub fn new(scope: Scope, epoch: u64, now: i64) -> Result<Self, Error> {
        scope.validate()?;
        if epoch == 0 || now < 0 {
            return Err(Error::Identity);
        }
        Ok(Self {
            scope,
            epoch,
            created_at: now,
            expires_at: now.checked_add(DURATION_SECONDS).ok_or(Error::Overflow)?,
            revoked: false,
            next_nonce: 0,
            spent_e6: 0,
            reserved_e6: 0,
            reservations: [None; MAX_PENDING],
        })
    }

    pub fn spent_e6(&self) -> u64 {
        self.spent_e6
    }
    pub fn reserved_e6(&self) -> u64 {
        self.reserved_e6
    }
    pub fn next_nonce(&self) -> u64 {
        self.next_nonce
    }
    pub fn expires_at(&self) -> i64 {
        self.expires_at
    }

    fn check(&self, action: &Action, operation: Operation) -> Result<(), Error> {
        if action.scope != self.scope {
            return Err(Error::Identity);
        }
        if action.epoch != self.epoch || action.nonce != self.next_nonce {
            return Err(Error::Replay);
        }
        if self.revoked {
            return Err(Error::Revoked);
        }
        if action.now < self.created_at || action.now >= self.expires_at {
            return Err(Error::Expired);
        }
        if action.operation != operation {
            return Err(Error::Forbidden);
        }
        if operation == Operation::MarketTrade {
            self.check_asset(action.asset_index, action.asset_market_id)?;
        } else if action.asset_index != 0 || action.asset_market_id != 0 {
            // No per-order market disclosure before private settlement.
            return Err(Error::Identity);
        }
        if self.next_nonce >= MAX_ACTIONS {
            return Err(Error::Limit);
        }
        Ok(())
    }

    fn check_asset(&self, index: u16, market_id: u64) -> Result<(), Error> {
        if self.scope.asset_market_ids.get(index as usize) != Some(&market_id) {
            return Err(Error::Identity);
        }
        Ok(())
    }

    fn ensure_capacity(&self, amount: u64) -> Result<(), Error> {
        let total = self
            .spent_e6
            .checked_add(self.reserved_e6)
            .and_then(|v| v.checked_add(amount))
            .ok_or(Error::Overflow)?;
        if total > TOTAL_NOTIONAL_E6 {
            return Err(Error::Limit);
        }
        Ok(())
    }

    /// Returned state must be committed in the same transaction as the fill.
    /// Taking `self` by value keeps all failures non-mutating for host callers.
    #[inline(never)]
    pub fn validate_market_fill(&self, action: &Action, fill: &Fill) -> Result<(), Error> {
        self.check(action, Operation::MarketTrade)?;
        self.ensure_capacity(fill.notional()?)
    }

    #[inline(never)]
    pub fn market_fill(mut self, action: &Action, fill: &Fill) -> Result<Self, Error> {
        self.validate_market_fill(action, fill)?;
        let notional = fill.notional()?;
        self.spent_e6 = self.spent_e6.checked_add(notional).ok_or(Error::Overflow)?;
        self.next_nonce = self.next_nonce.checked_add(1).ok_or(Error::Overflow)?;
        Ok(self)
    }

    /// Reserve the full public per-order ceiling regardless of private terms.
    /// The base-chain record never contains the hidden requested size or price.
    #[inline(never)]
    pub fn reserve_ninja(
        mut self,
        action: &Action,
        authorization: [u8; 32],
    ) -> Result<Self, Error> {
        self.check(action, Operation::NinjaCreate)?;
        if authorization == [0; 32]
            || [
                self.scope.owner,
                self.scope.signer,
                self.scope.market,
                self.scope.portfolio,
            ]
            .contains(&authorization)
            || self
                .reservations
                .iter()
                .flatten()
                .any(|r| r.authorization == authorization)
        {
            return Err(Error::Reservation);
        }
        self.ensure_capacity(PER_ACTION_NOTIONAL_E6)?;
        let index = self
            .reservations
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Pending)?;
        self.reservations[index] = Some(Reservation {
            authorization,
            nonce: action.nonce,
        });
        self.reserved_e6 = self
            .reserved_e6
            .checked_add(PER_ACTION_NOTIONAL_E6)
            .ok_or(Error::Overflow)?;
        self.next_nonce = self.next_nonce.checked_add(1).ok_or(Error::Overflow)?;
        Ok(self)
    }

    /// Cancellation permission alone does NOT release the reservation. The
    /// authoritative terminal state must be verified separately by the caller.
    #[inline(never)]
    pub fn request_ninja_cancel(
        mut self,
        action: &Action,
        authorization: [u8; 32],
        reservation_nonce: u64,
    ) -> Result<Self, Error> {
        self.check(action, Operation::NinjaCancel)?;
        self.find_reservation(authorization, reservation_nonce)?;
        self.next_nonce = self.next_nonce.checked_add(1).ok_or(Error::Overflow)?;
        Ok(self)
    }

    fn find_reservation(&self, authorization: [u8; 32], nonce: u64) -> Result<usize, Error> {
        self.reservations
            .iter()
            .position(|r| {
                *r == Some(Reservation {
                    authorization,
                    nonce,
                })
            })
            .ok_or(Error::Reservation)
    }

    /// Only call AFTER verifying authoritative terminal state/fill in the
    /// consuming program. `None` means proven unfilled cancellation/expiry,
    /// NEVER account absence, a timeout, an indexer status or a failed callback.
    /// Already-authorized orders may settle after session expiry/revocation.
    #[inline(never)]
    pub fn resolve_ninja(
        mut self,
        scope: &Scope,
        epoch: u64,
        authorization: [u8; 32],
        nonce: u64,
        asset_index: u16,
        asset_market_id: u64,
        fill: Option<&Fill>,
    ) -> Result<Self, Error> {
        if *scope != self.scope {
            return Err(Error::Identity);
        }
        if epoch != self.epoch {
            return Err(Error::Replay);
        }
        let index = self.find_reservation(authorization, nonce)?;
        let notional = match fill {
            Some(fill) => {
                self.check_asset(asset_index, asset_market_id)?;
                fill.notional()?
            }
            None => {
                if asset_index != 0 || asset_market_id != 0 {
                    return Err(Error::Identity);
                }
                0
            }
        };
        self.spent_e6 = self.spent_e6.checked_add(notional).ok_or(Error::Overflow)?;
        self.reserved_e6 = self
            .reserved_e6
            .checked_sub(PER_ACTION_NOTIONAL_E6)
            .ok_or(Error::Overflow)?;
        self.reservations[index] = None;
        self.ensure_capacity(0)?;
        Ok(self)
    }

    /// Caller must verify the real owner's signature, even after session expiry.
    pub fn revoke(mut self, owner: [u8; 32], epoch: u64) -> Result<Self, Error> {
        if owner != self.scope.owner {
            return Err(Error::Identity);
        }
        if epoch != self.epoch {
            return Err(Error::Replay);
        }
        self.revoked = true;
        Ok(self)
    }

    /// Cannot reset the budget while old orders retain reservations. A new
    /// approval changes the key and monotonically advances the stored epoch.
    #[inline(never)]
    pub fn renew(
        self,
        owner: [u8; 32],
        new_signer: [u8; 32],
        expected_epoch: u64,
        now: i64,
    ) -> Result<Self, Error> {
        let mut scope = self.scope;
        scope.signer = new_signer;
        self.renew_for_scope(owner, scope, expected_epoch, now)
    }

    /// Explicit owner reapproval may bind refreshed market/portfolio instance
    /// IDs after recovery. It cannot move the grant to another owner, program,
    /// domain or account, or abandon any old private reservation.
    #[inline(never)]
    pub fn renew_for_scope(
        self,
        owner: [u8; 32],
        scope: Scope,
        expected_epoch: u64,
        now: i64,
    ) -> Result<Self, Error> {
        if owner != self.scope.owner {
            return Err(Error::Identity);
        }
        if scope.owner != self.scope.owner
            || scope.program != self.scope.program
            || scope.domain != self.scope.domain
            || scope.market != self.scope.market
            || scope.portfolio != self.scope.portfolio
        {
            return Err(Error::Identity);
        }
        if expected_epoch != self.epoch {
            return Err(Error::Replay);
        }
        if self.reserved_e6 != 0 || self.reservations.iter().any(Option::is_some) {
            return Err(Error::Pending);
        }
        if now < self.created_at || scope.signer == self.scope.signer {
            return Err(Error::Identity);
        }
        Self::new(
            scope,
            self.epoch.checked_add(1).ok_or(Error::Overflow)?,
            now,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_reapproval_after_instance_recovery_keeps_account_scope_and_epoch() {
        let first = Grant::new(scope(), 1, 100).unwrap();
        let mut refreshed = scope();
        refreshed.signer = [50; 32];
        refreshed.market_instance += 1;
        refreshed.portfolio_instance += 1;
        refreshed.asset_market_ids = [7, 8, 9];
        let next = first
            .renew_for_scope(scope().owner, refreshed, 1, 101)
            .unwrap();
        assert_eq!(next.scope(), refreshed);
        assert_eq!(next.epoch(), 2);
        let mut substituted = refreshed;
        substituted.portfolio = [51; 32];
        assert!(first
            .renew_for_scope(scope().owner, substituted, 1, 101)
            .is_err());
        assert!(first.renew_for_scope([51; 32], refreshed, 1, 101).is_err());
        assert!(first
            .renew_for_scope(scope().owner, refreshed, 0, 101)
            .is_err());
        let action = Action {
            scope: scope(),
            epoch: 1,
            nonce: 0,
            now: 100,
            operation: Operation::NinjaCreate,
            asset_index: 0,
            asset_market_id: 0,
        };
        let pending = first.reserve_ninja(&action, [60; 32]).unwrap();
        assert!(pending
            .renew_for_scope(scope().owner, refreshed, 1, 101)
            .is_err());
    }
    #[test]
    fn account_codec_rejects_noncanonical_and_roundtrips_reservations() {
        let grant = Grant::new(scope(), 1, 100).unwrap();
        assert_eq!(Grant::decode(&grant.encode()), Ok(grant));
        let action = Action {
            scope: scope(),
            epoch: 1,
            nonce: 0,
            now: 101,
            operation: Operation::NinjaCreate,
            asset_index: 0,
            asset_market_id: 0,
        };
        let reserved = grant.reserve_ninja(&action, [99; 32]).unwrap();
        assert_eq!(Grant::decode(&reserved.encode()), Ok(reserved));
        for index in [0, 289, 295, 696, 703] {
            let mut bytes = reserved.encode();
            bytes[index] ^= 1;
            assert!(Grant::decode(&bytes).is_err(), "byte {index}");
        }
        let mut bytes = reserved.encode();
        bytes[280] ^= 1;
        assert!(Grant::decode(&bytes).is_err());
        let mut bytes = reserved.encode();
        bytes[288] = 2;
        assert!(Grant::decode(&bytes).is_err());
        let mut bytes = reserved.encode();
        bytes[328..336].copy_from_slice(&1u64.to_le_bytes());
        assert!(Grant::decode(&bytes).is_err());
        assert!(Grant::decode(&reserved.encode()[..703]).is_err());
    }
    fn scope() -> Scope {
        Scope {
            owner: [1; 32],
            signer: [2; 32],
            program: [3; 32],
            domain: [4; 32],
            market: [5; 32],
            portfolio: [6; 32],
            market_instance: 1,
            portfolio_instance: 2,
            asset_market_ids: [10, 11, 12],
        }
    }
    fn grant() -> Grant {
        Grant::new(scope(), 1, 100).unwrap()
    }
    fn action(g: &Grant, op: Operation) -> Action {
        Action {
            scope: g.scope,
            epoch: g.epoch,
            nonce: g.next_nonce,
            now: 101,
            operation: op,
            asset_index: 0,
            asset_market_id: if op == Operation::MarketTrade { 10 } else { 0 },
        }
    }
    fn fill() -> Fill {
        Fill {
            requested_q: 1_000_000,
            filled_q: 1_000_000,
            price_e6: 100_000_000,
            minimum_price_e6: 99_000_000,
            maximum_price_e6: 100_000_000,
            fee_bps: 10,
            maximum_fee_bps: 20,
            position_before_q: 0,
            reduce_only: false,
        }
    }
    #[test]
    fn exact_ceiling_and_round_up() {
        assert_eq!(notional_ceil(1, 1), Ok(1));
        assert_eq!(
            notional_ceil(-1_000_000, 100_000_000),
            Ok(PER_ACTION_NOTIONAL_E6)
        );
        let g = grant()
            .market_fill(&action(&grant(), Operation::MarketTrade), &fill())
            .unwrap();
        assert_eq!(g.spent_e6(), PER_ACTION_NOTIONAL_E6);
        let mut f = fill();
        f.requested_q += 1;
        f.filled_q += 1;
        assert_eq!(f.notional(), Err(Error::Limit));
    }
    #[test]
    fn malformed_arithmetic_fails_closed() {
        assert_eq!(notional_ceil(0, 1), Err(Error::Amount));
        assert_eq!(notional_ceil(i128::MIN, 1), Err(Error::Amount));
        assert_eq!(notional_ceil(1, 0), Err(Error::Amount));
        assert_eq!(notional_ceil(i128::MAX, u64::MAX), Err(Error::Overflow));
        assert_eq!(Grant::new(scope(), 1, i64::MAX), Err(Error::Overflow));
    }
    #[test]
    fn expiry_is_exclusive_and_clock_cannot_go_backwards() {
        let g = grant();
        let mut a = action(&g, Operation::MarketTrade);
        a.now = 3_699;
        assert!(g.market_fill(&a, &fill()).is_ok());
        a.now = 3_700;
        assert_eq!(g.market_fill(&a, &fill()), Err(Error::Expired));
        a.now = 99;
        assert_eq!(g.market_fill(&a, &fill()), Err(Error::Expired));
    }
    #[test]
    fn all_identity_fields_are_bound() {
        let g = grant();
        for n in 0..11 {
            let mut a = action(&g, Operation::MarketTrade);
            match n {
                0 => a.scope.owner = [9; 32],
                1 => a.scope.signer = [9; 32],
                2 => a.scope.program = [9; 32],
                3 => a.scope.domain = [9; 32],
                4 => a.scope.market = [9; 32],
                5 => a.scope.portfolio = [9; 32],
                6 => a.scope.market_instance += 1,
                7 => a.scope.portfolio_instance += 1,
                8 => a.scope.asset_market_ids[0] += 1,
                9 => a.asset_index = 3,
                _ => a.asset_market_id += 1,
            }
            assert_eq!(g.market_fill(&a, &fill()), Err(Error::Identity));
        }
    }
    #[test]
    fn privileged_operations_and_wrong_routes_rejected() {
        let g = grant();
        for op in [
            Operation::Withdraw,
            Operation::Transfer,
            Operation::Admin,
            Operation::NinjaCancel,
            Operation::NinjaCreate,
        ] {
            assert_eq!(
                g.market_fill(&action(&g, op), &fill()),
                Err(Error::Forbidden)
            );
        }
    }
    #[test]
    fn nonce_prevents_repeat_and_concurrent_replay() {
        let g = grant();
        let a = action(&g, Operation::MarketTrade);
        let next = g.market_fill(&a, &fill()).unwrap();
        assert_eq!(next.market_fill(&a, &fill()), Err(Error::Replay));
        let mut a = action(&next, Operation::MarketTrade);
        a.epoch += 1;
        assert_eq!(next.market_fill(&a, &fill()), Err(Error::Replay));
    }
    #[test]
    fn cumulative_budget_includes_both_directions() {
        let mut g = grant();
        for n in 0..10 {
            let mut f = fill();
            if n % 2 == 1 {
                f.requested_q *= -1;
                f.filled_q *= -1;
            }
            g = g
                .market_fill(&action(&g, Operation::MarketTrade), &f)
                .unwrap();
        }
        assert_eq!(g.spent_e6(), TOTAL_NOTIONAL_E6);
        assert_eq!(
            g.market_fill(&action(&g, Operation::MarketTrade), &fill()),
            Err(Error::Limit)
        );
    }
    #[test]
    fn reduce_only_cannot_reverse_increase_or_open() {
        let mut f = fill();
        f.reduce_only = true;
        for before in [0, 1_000_000, -999_999, i128::MIN] {
            f.position_before_q = before;
            assert_eq!(f.notional(), Err(Error::Forbidden));
        }
        f.position_before_q = -1_000_000;
        assert!(f.notional().is_ok());
        f.position_before_q = -2_000_000;
        assert!(f.notional().is_ok());
    }
    #[test]
    fn sell_has_upper_price_and_fee_bounds_and_no_partial_fill() {
        let mut f = fill();
        f.requested_q = -1_000_000;
        f.filled_q = -1_000_000;
        assert!(f.notional().is_ok());
        f.price_e6 += 1;
        assert_eq!(f.notional(), Err(Error::Limit));
        f = fill();
        f.fee_bps = 21;
        assert_eq!(f.notional(), Err(Error::Limit));
        f = fill();
        f.filled_q -= 1;
        assert_eq!(f.notional(), Err(Error::Limit));
    }
    #[test]
    fn private_and_public_actions_share_budget_across_assets() {
        let mut g = grant();
        for n in 0..10u8 {
            let a = action(&g, Operation::NinjaCreate);
            g = g.reserve_ninja(&a, [20 + n; 32]).unwrap();
        }
        assert_eq!(g.reserved_e6(), TOTAL_NOTIONAL_E6);
        assert_eq!(
            g.market_fill(&action(&g, Operation::MarketTrade), &fill()),
            Err(Error::Limit)
        );
        assert_eq!(
            g.reserve_ninja(&action(&g, Operation::NinjaCreate), [40; 32]),
            Err(Error::Limit)
        );
    }
    #[test]
    fn cancellation_request_does_not_free_ambiguous_allowance() {
        let g = grant();
        let g = g
            .reserve_ninja(&action(&g, Operation::NinjaCreate), [20; 32])
            .unwrap();
        let g = g
            .request_ninja_cancel(&action(&g, Operation::NinjaCancel), [20; 32], 0)
            .unwrap();
        assert_eq!(g.reserved_e6(), PER_ACTION_NOTIONAL_E6);
        let resolved = g
            .resolve_ninja(&scope(), 1, [20; 32], 0, 0, 0, None)
            .unwrap();
        assert_eq!(resolved.reserved_e6(), 0);
        assert_eq!(resolved.spent_e6(), 0);
        assert_eq!(
            resolved.resolve_ninja(&scope(), 1, [20; 32], 0, 0, 0, None),
            Err(Error::Reservation)
        );
    }
    #[test]
    fn private_resolution_is_exact_and_once_only() {
        let g = grant();
        let g = g
            .reserve_ninja(&action(&g, Operation::NinjaCreate), [20; 32])
            .unwrap();
        assert_eq!(
            g.resolve_ninja(&scope(), 1, [20; 32], 1, 0, 0, None),
            Err(Error::Reservation)
        );
        assert_eq!(
            g.resolve_ninja(&scope(), 1, [20; 32], 0, 3, 13, Some(&fill())),
            Err(Error::Identity)
        );
        let next = g
            .resolve_ninja(&scope(), 1, [20; 32], 0, 0, 10, Some(&fill()))
            .unwrap();
        assert_eq!(next.spent_e6(), PER_ACTION_NOTIONAL_E6);
        assert_eq!(next.reserved_e6(), 0);
        assert_eq!(
            next.resolve_ninja(&scope(), 1, [20; 32], 0, 0, 10, Some(&fill())),
            Err(Error::Reservation)
        );
    }
    #[test]
    fn renewal_cannot_abandon_reservations_or_reuse_epoch() {
        let g = grant();
        let pending = g
            .reserve_ninja(&action(&g, Operation::NinjaCreate), [20; 32])
            .unwrap();
        assert_eq!(
            pending.renew(scope().owner, [8; 32], 1, 102),
            Err(Error::Pending)
        );
        assert_eq!(
            g.renew(scope().owner, scope().signer, 1, 102),
            Err(Error::Identity)
        );
        let next = g.renew(scope().owner, [8; 32], 1, 102).unwrap();
        assert_eq!(next.epoch, 2);
        assert_eq!(next.next_nonce, 0);
        let mut a = action(&next, Operation::MarketTrade);
        a.epoch = 1;
        assert_eq!(next.market_fill(&a, &fill()), Err(Error::Replay));
    }
    #[test]
    fn revoke_stops_new_actions_not_existing_order_resolution() {
        let g = grant();
        let g = g
            .reserve_ninja(&action(&g, Operation::NinjaCreate), [20; 32])
            .unwrap();
        assert_eq!(g.revoke([9; 32], 1), Err(Error::Identity));
        let g = g.revoke(scope().owner, 1).unwrap();
        assert_eq!(
            g.market_fill(&action(&g, Operation::MarketTrade), &fill()),
            Err(Error::Revoked)
        );
        assert_eq!(
            g.reserve_ninja(&action(&g, Operation::NinjaCreate), [21; 32]),
            Err(Error::Revoked)
        );
        assert!(g
            .resolve_ninja(&scope(), 1, [20; 32], 0, 0, 10, Some(&fill()))
            .is_ok());
    }
    #[test]
    fn operation_cap_bounds_cancel_churn() {
        let mut g = grant();
        g = g
            .reserve_ninja(&action(&g, Operation::NinjaCreate), [20; 32])
            .unwrap();
        while g.next_nonce() < MAX_ACTIONS {
            g = g
                .request_ninja_cancel(&action(&g, Operation::NinjaCancel), [20; 32], 0)
                .unwrap();
        }
        assert_eq!(
            g.request_ninja_cancel(&action(&g, Operation::NinjaCancel), [20; 32], 0),
            Err(Error::Limit)
        );
    }

    #[test]
    fn private_reservations_disclose_no_asset_terms() {
        let g = grant();
        let mut a = action(&g, Operation::NinjaCreate);
        a.asset_index = 1;
        a.asset_market_id = 11;
        assert_eq!(g.reserve_ninja(&a, [20; 32]), Err(Error::Identity));
        let g = g
            .reserve_ninja(&action(&g, Operation::NinjaCreate), [20; 32])
            .unwrap();
        // The reserve is uniform; its committed market is validated only at fill.
        assert_eq!(
            g.reservations[0],
            Some(Reservation {
                authorization: [20; 32],
                nonce: 0
            })
        );
        assert!(g
            .resolve_ninja(&scope(), 1, [20; 32], 0, 1, 11, Some(&fill()))
            .is_ok());
    }

    #[test]
    fn quantity_scale_matches_the_engine() {
        assert_eq!(QUANTITY_SCALE, percolator::POS_SCALE);
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_notional_never_rounds_down(q in 1u64..=u64::MAX, price in 1u64..=u64::MAX) {
            let numerator = u128::from(q) * u128::from(price);
            if let Ok(value) = notional_ceil(i128::from(q), price) {
                proptest::prop_assert!(u128::from(value) * QUANTITY_SCALE >= numerator);
                proptest::prop_assert!(u128::from(value - 1) * QUANTITY_SCALE < numerator);
            }
        }

        #[test]
        fn arbitrary_mixed_sequence_preserves_allowance(
            operations in proptest::collection::vec((0u8..5, 1u64..=150_000_000), 1..150)
        ) {
            let mut g = grant();
            for (kind, price) in operations {
                let before = g;
                let result = match kind {
                    0 => {
                        let mut f = fill();
                        f.price_e6 = price;
                        f.minimum_price_e6 = 1;
                        f.maximum_price_e6 = 150_000_000;
                        g.market_fill(&action(&g, Operation::MarketTrade), &f)
                    }
                    1 => {
                        let mut id = [20;32];
                        id[..8].copy_from_slice(&g.next_nonce.to_le_bytes());
                        g.reserve_ninja(&action(&g, Operation::NinjaCreate), id)
                    }
                    _ => match g.reservations.iter().flatten().next().copied() {
                        Some(r) if kind == 2 => g.request_ninja_cancel(
                            &action(&g, Operation::NinjaCancel), r.authorization, r.nonce),
                        Some(r) if kind == 3 => g.resolve_ninja(
                            &scope(), 1, r.authorization, r.nonce, 0, 0, None),
                        Some(r) => g.resolve_ninja(
                            &scope(), 1, r.authorization, r.nonce, 0, 10, Some(&fill())),
                        None => Err(Error::Reservation),
                    },
                };
                if let Ok(next) = result { g = next; } else {
                    proptest::prop_assert_eq!(g, before);
                }
                proptest::prop_assert!(g.spent_e6 + g.reserved_e6 <= TOTAL_NOTIONAL_E6);
                proptest::prop_assert_eq!(g.reserved_e6,
                    g.reservations.iter().flatten().count() as u64 * PER_ACTION_NOTIONAL_E6);
                proptest::prop_assert!(g.spent_e6 >= before.spent_e6);
                proptest::prop_assert!(g.next_nonce >= before.next_nonce);
                proptest::prop_assert!(g.next_nonce <= MAX_ACTIONS);
            }
        }
    }
}
