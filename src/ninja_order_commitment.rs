//! Canonical committed Ninja Order verification and decoding.
//!
//! Pending private authorizations store only the commitment and deliberately
//! public lifecycle metadata. The reveal is accepted only by the atomic
//! execution instruction, where every bound field is checked again before the
//! trade and one-shot authorization consumption occur in the same transaction.

use solana_program::{entrypoint::ProgramResult, hash::hashv, program_error::ProgramError};

pub const NINJA_ORDER_COMMITMENT_DOMAIN_V1: &[u8] = b"ninja.orders.commitment.v1";
pub const NINJA_ORDER_REVEAL_MAGIC_V1: &[u8; 8] = b"NINJORD1";
pub const NINJA_ORDER_REVEAL_V1_LEN: usize = 344;
pub const NINJA_ORDER_COMMITMENT_LEN: usize = 32;

const MARKET_OFF: usize = 8;
const PORTFOLIO_OFF: usize = 40;
const OWNER_OFF: usize = 72;
const DELEGATE_OFF: usize = 104;
const AUTHORIZATION_OFF: usize = 136;
const EXPIRY_SLOT_OFF: usize = 168;
const BRANCH_COUNT_OFF: usize = 176;
const BRANCH_LEN: usize = 64;
const BRANCHES_OFF: usize = 184;
const SALT_OFF: usize = 312;
const MAX_FEE_BPS: u64 = 100;
const MAX_ORACLE_PRICE_E6: u64 = 1_000_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NinjaOrderRevealHeaderV1 {
    pub market: [u8; 32],
    pub portfolio: [u8; 32],
    pub owner: [u8; 32],
    pub delegate: [u8; 32],
    pub authorization: [u8; 32],
    pub expiry_slot: u64,
    pub branch_count: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NinjaOrderBranchV1 {
    pub size_q: i128,
    pub trigger_price_e6: u64,
    pub limit_price_e6: u64,
    pub max_fee_bps: u64,
    pub asset_market_id: u64,
    pub asset_index: u16,
    pub trigger_condition: u8,
    pub reduce_only: u8,
}

fn invalid() -> ProgramError {
    ProgramError::InvalidInstructionData
}

fn read_u16(payload: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(payload[offset..offset + 2].try_into().unwrap())
}

fn read_u64(payload: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(payload[offset..offset + 8].try_into().unwrap())
}

fn read_i128(payload: &[u8], offset: usize) -> i128 {
    i128::from_le_bytes(payload[offset..offset + 16].try_into().unwrap())
}

fn read_bytes32(payload: &[u8], offset: usize) -> [u8; 32] {
    payload[offset..offset + 32].try_into().unwrap()
}

fn read_branch(payload: &[u8], offset: usize) -> NinjaOrderBranchV1 {
    NinjaOrderBranchV1 {
        size_q: read_i128(payload, offset),
        trigger_price_e6: read_u64(payload, offset + 16),
        limit_price_e6: read_u64(payload, offset + 24),
        max_fee_bps: read_u64(payload, offset + 32),
        asset_market_id: read_u64(payload, offset + 40),
        asset_index: read_u16(payload, offset + 48),
        trigger_condition: payload[offset + 50],
        reduce_only: payload[offset + 51],
    }
}

fn validate_branch(payload: &[u8], offset: usize) -> ProgramResult {
    let branch = read_branch(payload, offset);
    if branch.size_q == 0
        || branch.size_q == i128::MIN
        || branch.trigger_price_e6 == 0
        || branch.trigger_price_e6 > MAX_ORACLE_PRICE_E6
        || branch.limit_price_e6 == 0
        || branch.limit_price_e6 > MAX_ORACLE_PRICE_E6
        || branch.max_fee_bps > MAX_FEE_BPS
        || branch.asset_market_id == 0
        || branch.trigger_condition > 1
        || branch.reduce_only > 1
        || (branch.size_q > 0 && branch.limit_price_e6 < branch.trigger_price_e6)
        || (branch.size_q < 0 && branch.limit_price_e6 > branch.trigger_price_e6)
        || payload[offset + 52..offset + BRANCH_LEN]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(invalid());
    }
    Ok(())
}

/// Checks that a reveal is the one canonical v1 encoding accepted by the
/// browser and executor. This must run before hashing or executing a reveal.
pub fn validate_ninja_order_reveal_v1(payload: &[u8]) -> ProgramResult {
    if payload.len() != NINJA_ORDER_REVEAL_V1_LEN
        || &payload[..8] != NINJA_ORDER_REVEAL_MAGIC_V1
        || read_u64(payload, EXPIRY_SLOT_OFF) == 0
        || payload[BRANCH_COUNT_OFF + 1..BRANCHES_OFF]
            .iter()
            .any(|byte| *byte != 0)
        || payload[SALT_OFF..].iter().all(|byte| *byte == 0)
    {
        return Err(invalid());
    }
    let branch_count = payload[BRANCH_COUNT_OFF] as usize;
    if !(1..=2).contains(&branch_count) {
        return Err(invalid());
    }
    for index in 0..branch_count {
        validate_branch(payload, BRANCHES_OFF + index * BRANCH_LEN)?;
    }
    if branch_count == 1 {
        if payload[BRANCHES_OFF + BRANCH_LEN..SALT_OFF]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(invalid());
        }
    } else {
        let below = read_branch(payload, BRANCHES_OFF);
        let above = read_branch(payload, BRANCHES_OFF + BRANCH_LEN);
        if below.trigger_condition != 0
            || above.trigger_condition != 1
            || below.asset_index != above.asset_index
            || below.asset_market_id != above.asset_market_id
            || below.size_q != above.size_q
            || below.max_fee_bps != above.max_fee_bps
            || below.reduce_only != 1
            || above.reduce_only != 1
            || below.trigger_price_e6 >= above.trigger_price_e6
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn decode_ninja_order_header_v1(
    payload: &[u8],
) -> Result<NinjaOrderRevealHeaderV1, ProgramError> {
    validate_ninja_order_reveal_v1(payload)?;
    Ok(NinjaOrderRevealHeaderV1 {
        market: read_bytes32(payload, MARKET_OFF),
        portfolio: read_bytes32(payload, PORTFOLIO_OFF),
        owner: read_bytes32(payload, OWNER_OFF),
        delegate: read_bytes32(payload, DELEGATE_OFF),
        authorization: read_bytes32(payload, AUTHORIZATION_OFF),
        expiry_slot: read_u64(payload, EXPIRY_SLOT_OFF),
        branch_count: payload[BRANCH_COUNT_OFF],
    })
}

pub fn decode_ninja_order_branch_v1(
    payload: &[u8],
    branch_index: u8,
) -> Result<NinjaOrderBranchV1, ProgramError> {
    validate_ninja_order_reveal_v1(payload)?;
    if branch_index >= payload[BRANCH_COUNT_OFF] {
        return Err(invalid());
    }
    Ok(read_branch(
        payload,
        BRANCHES_OFF + branch_index as usize * BRANCH_LEN,
    ))
}

/// Returns SHA-256(domain || canonical reveal).
pub fn ninja_order_commitment_v1(payload: &[u8]) -> Result<[u8; 32], ProgramError> {
    validate_ninja_order_reveal_v1(payload)?;
    Ok(hashv(&[NINJA_ORDER_COMMITMENT_DOMAIN_V1, payload]).to_bytes())
}

/// Constant-work comparison after canonical validation and hashing.
pub fn verify_ninja_order_commitment_v1(
    expected: &[u8; NINJA_ORDER_COMMITMENT_LEN],
    payload: &[u8],
) -> ProgramResult {
    let actual = ninja_order_commitment_v1(payload)?;
    let mut difference = 0u8;
    for index in 0..NINJA_ORDER_COMMITMENT_LEN {
        difference |= actual[index] ^ expected[index];
    }
    if difference == 0 {
        Ok(())
    } else {
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_COMMITMENT: [u8; 32] = [
        0xf8, 0x64, 0xaa, 0x32, 0x4b, 0x5c, 0x22, 0xae, 0x68, 0x30, 0x8c, 0x59, 0x2f, 0x19, 0x89,
        0x84, 0xa6, 0x96, 0x9c, 0xc4, 0x53, 0xb0, 0x5f, 0xf8, 0xb4, 0x72, 0x23, 0x65, 0x98, 0xfb,
        0x25, 0xde,
    ];

    fn write_branch(
        payload: &mut [u8],
        offset: usize,
        trigger_price_e6: u64,
        limit_price_e6: u64,
        trigger_condition: u8,
    ) {
        payload[offset..offset + 16].copy_from_slice(&(-5_000_000i128).to_le_bytes());
        payload[offset + 16..offset + 24].copy_from_slice(&trigger_price_e6.to_le_bytes());
        payload[offset + 24..offset + 32].copy_from_slice(&limit_price_e6.to_le_bytes());
        payload[offset + 32..offset + 40].copy_from_slice(&100u64.to_le_bytes());
        payload[offset + 40..offset + 48].copy_from_slice(&44u64.to_le_bytes());
        payload[offset + 48..offset + 50].copy_from_slice(&3u16.to_le_bytes());
        payload[offset + 50] = trigger_condition;
        payload[offset + 51] = 1;
    }

    fn fixture() -> [u8; NINJA_ORDER_REVEAL_V1_LEN] {
        let mut payload = [0u8; NINJA_ORDER_REVEAL_V1_LEN];
        payload[..8].copy_from_slice(NINJA_ORDER_REVEAL_MAGIC_V1);
        for (index, byte) in [1u8, 2, 3, 4, 5].iter().enumerate() {
            payload[8 + index * 32..8 + (index + 1) * 32].fill(*byte);
        }
        payload[EXPIRY_SLOT_OFF..EXPIRY_SLOT_OFF + 8].copy_from_slice(&1_234_567u64.to_le_bytes());
        payload[BRANCH_COUNT_OFF] = 2;
        write_branch(&mut payload, BRANCHES_OFF, 90_000_000, 85_500_000, 0);
        write_branch(
            &mut payload,
            BRANCHES_OFF + BRANCH_LEN,
            110_000_000,
            104_500_000,
            1,
        );
        for (index, byte) in payload[SALT_OFF..].iter_mut().enumerate() {
            *byte = index as u8;
        }
        payload
    }

    #[test]
    fn matches_browser_and_executor_commitment_vector() {
        let payload = fixture();
        assert_eq!(
            ninja_order_commitment_v1(&payload).unwrap(),
            EXPECTED_COMMITMENT
        );
        assert_eq!(
            verify_ninja_order_commitment_v1(&EXPECTED_COMMITMENT, &payload),
            Ok(())
        );
        let header = decode_ninja_order_header_v1(&payload).unwrap();
        assert_eq!(header.market, [1; 32]);
        assert_eq!(header.branch_count, 2);
        let above = decode_ninja_order_branch_v1(&payload, 1).unwrap();
        assert_eq!(above.trigger_condition, 1);
        assert_eq!(above.asset_market_id, 44);
    }

    #[test]
    fn rejects_noncanonical_or_wrong_reveals() {
        let mut reserved = fixture();
        reserved[177] = 1;
        assert_eq!(validate_ninja_order_reveal_v1(&reserved), Err(invalid()));

        let mut mismatched_oco = fixture();
        mismatched_oco[BRANCHES_OFF + BRANCH_LEN] ^= 1;
        assert_eq!(
            validate_ninja_order_reveal_v1(&mismatched_oco),
            Err(invalid())
        );

        let payload = fixture();
        assert_eq!(decode_ninja_order_branch_v1(&payload, 2), Err(invalid()));
        let mut wrong = EXPECTED_COMMITMENT;
        wrong[0] ^= 1;
        assert_eq!(
            verify_ninja_order_commitment_v1(&wrong, &payload),
            Err(invalid())
        );
    }
}
