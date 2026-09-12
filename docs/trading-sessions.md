# General trading sessions: policy core

Status: **incomplete feature; not enabled or deployable for user testing**.
`src/trading_session.rs` is a deterministic policy module only. No instruction
discriminant, account layout, signing path or existing authorization is changed.

## Initial development policy

- One hour; expiry is exclusive and evaluated using authenticated chain time.
- $100 notional per trade/order; $1,000 cumulative gross notional, not a loss cap.
- Market trades and private order creation/cancellation; no withdrawals,
  transfers or administrative authority.
- All supported assets share the budget. A private order reserves a uniform
  $100 until authoritative settlement/cancellation. The reservation stores only
  authorization identity and nonce, not hidden market, price, size or direction.
- Both opening and closing trades consume gross allowance. A session cannot
  disguise increasing/reversing exposure as reduce-only. Owner signing remains
  the fallback after expiry or exhaustion.
- A cancellation request, timeout or missing account does not release a reserve.
  A proven terminal outcome resolves it exactly once. Already-authorized orders
  retain their own lifetimes after the general session expires or is revoked.
- Epochs/nonces prevent replay; renewal cannot abandon pending reservations.
  A bounded 100-operation ceiling prevents unlimited session action churn and
  must be included in eventual owner consent, not silently assumed authorized.

## Required integration before activation

The public policy functions are not an authentication boundary. Consuming
instructions must check signer privileges, canonical program-owned grant PDA,
owner, program/domain, market/portfolio instances, allowed asset instances,
authenticated Clock, exact account lists, CPI targets and current state.
Persist accounting in the same atomic transaction as the actual accepted fill.
Never accept caller-provided fill/terminal-state assertions as evidence.

Private session-created authorizations need an explicit versioned binding to
the shared grant and a guarded callback path. Current private authorization
reserved bytes are required to be zero in v16, the Ninja adapter and the indexer;
do not repurpose them unilaterally or apply new rules to existing orders.
Update all consumers together and prove retries/cleanup cannot release or spend
the same allowance twice. Do not disclose private terms during grant creation.

The existing owner-only handlers remain mandatory until that integration exists.
Do not replace the owner account with a browser signer, add a permissive bypass,
or display an enabled session based on local state alone.

Still required: versioned grant storage and owner create/revoke/renew handlers,
guarded market execution, Ninja authorization/settlement bridge, adapter/indexer
compatibility, exact client codecs, explicit fee funding, memory-only signer
lifecycle, MWA approval/readback and enable/revoke UI, SBF/TEE and wallet tests,
then a reviewed exact devnet release. No new secrets are needed for the policy
core. Deposits and fee top-ups require explicit main-wallet consent.

## Checks

```sh
NO_DNA=1 PROPTEST_CASES=1024 cargo test --offline --lib
NO_DNA=1 cargo check --offline --lib
rustfmt --edition 2021 --check src/trading_session.rs
git diff --check
```

The host suite contains 19 policy tests (including two property tests) plus 15
existing regression tests. Property tests cover integer rounding and randomized
public/private budget sequences. These do **not** substitute for full account,
instruction, SBF, callback, MWA or hosted deployment tests.
