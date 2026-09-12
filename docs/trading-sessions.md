# General trading sessions — staged devnet implementation

Status: implemented locally across v16, the Ninja adapter, indexer and frontend;
activation remains off until exact reviewed devnet releases and hosted checks.
This is not a mainnet-ready security audit or proof of hardware-wallet/TEE UX.

## Owner consent and authority

One hour (exclusive chain-clock expiry), $100 per action, $1,000 cumulative gross
notional, 100 actions, assets0–2 (SOL/BTC/ETH). Opens and closes both count.
Private orders reserve a uniform $100 without exposing their asset/size/price.
At most10 on-chain reservations; the current frontend still allows only one
active Ninja order per portfolio. Proven fills charge actual accepted notional
and resolve once; timeout/absence cannot release allowance. Parent expiry or
revocation stops new actions, but existing child orders retain their own expiry.
Owner reapproval uses a new signer and epoch, with no outstanding reservations.

No withdrawal, transfer, mint, admin or arbitrary-CPI authority is granted.
The program authenticates real signers, program-owned canonical accounts,
owner/market/portfolio/asset instances, epoch, nonce, clock, accepted price and
fee. Shared accounting commits atomically with engine effects; failures roll back.
Legacy owner instructions and 256-byte private authorizations remain supported.

## ABI

The704-byte MUKSESS1 grant PDA is derived from
`["trading-session-v1", owner, portfolio]` under the runtime program ID.
Its fixed devnet domain binds the full genesis hash. It is retained after revoke
so close/recreation cannot reset epochs. Its rent is not included in fee refunds.

| Tag | Data bytes | Purpose / accounts |
| --- | ---: | --- |
|83|9|Owner create/renew: owner, market, portfolio, grant, new signer, System|
|84|9|Owner revoke: owner, grant|
|85|60|Market trade: standard7 matcher accounts, grant, real owner, validated matcher tail|
|86|89|Ninja reserve: session signer, market, portfolio, new authorization signer, grant|
|87|17|Session cancel: session signer, authorization, grant|
|88|1|Permissionless proven-revoked reservation resolution: authorization, grant|

Tag85 includes both execution-price bounds and a fee ceiling; it requests the
existing5bps taker fee. The real owner meta is checked on chain and allows
indexing without extra portfolio discovery. Tag86 creates a320-byte authorization:
old256-byte body plus MUKNSES1, parent grant, epoch, reservation nonce and resolved
marker. Private execution77 adds verified owner refund and parent grant; close
handlers refuse unresolved reservations.

The adapter adds `create_session_grant_via_trading_session` and
`settle_private_order_v2`. Existing callback layouts remain for scheduled old
orders. V2 carries the canonical parent, resolves its allowance atomically via
v16, and refunds successful authorization rent. The604-byte TEE buffer and212-byte
child grant layouts do not change.

## Device key and transaction lifecycle

Explicit consent creates a persistent non-extractable WebCrypto Ed25519 key in
IndexedDB. Refresh preserves the key, but does not silently resume trading.
Initial owner approval funds up to0.2devnet SOL plus persistent grant rent.
The key is never exported as private bytes. Same-origin compromise can still
spend its fee float and exercise its bounded on-chain authority.

Cross-tab locking and monotonic nonces serialize actions. Fresh action snapshots
use four known accounts in one batch, no recurring discovery/polling. Restricted
device signing rejects transfers and foreign/mixed instructions. It simulates,
gets a fresh blockhash, signs once, sends once, and reconciles the locally known
signature even if the RPC response is lost. It never automatically replaces a trade.
Owner revocation/refund remains available while placement is paused or market
instances have changed; the key is deleted only after finalized revocation and
zero fee balance. Clearing browser storage beforehand can lose the fee key.

## Exact build and tests

The default adapter build path produced a pre-entry access violation in both
legacy and new handlers. Do not deploy that output. The tested route uses the
already-installed platform-tools v1.52 compiler directly, offline/locked, then
strips the ELF. The build helper fails on stack-overflow diagnostics even if cargo
returns success. A validation/CPI frame split removed the observed stack warning.
Toolchain installation is a separate action; the helper never changes rustup.

From the frontend repository:
```sh
node experiments/magicblock-ninja/scripts/build-session-adapter.mjs
node experiments/magicblock-ninja/scripts/check-program-idl.mjs
npm run test:ci
npm run typecheck
npm run build
```

From this repository (absolute ELF paths for isolated worktrees):
```sh
NO_DNA=1 cargo test --offline --lib
NO_DNA=1 NINJA_ADAPTER_TEST_ELF=/absolute/tested/ninja_private_adapter.so cargo test --offline --test trading_session_runtime session_runtime_ -- --include-ignored
NO_DNA=1 V16_TEST_MATCHER_ELF=/absolute/matcher/percolator_match.so cargo test --offline --test v16_cu
```

Build the existing auth/hostile matcher fixtures first. The opt-in Surfpool test
uses only localhost18899, an explicitly offline validator, synthetic accounts and
in-memory fixture keys. No user credential, wallet file or remote fork is used.
The real LiteSVM cross-program bridge test is independent of that mocked fixture.
The complete576-test v16 suite requires the batch-capable test matcher revision
`60aac3a996d0264c47fbefe9a2625d6ee6fb6a47`; the older workspace matcher lacks
batch-tag3. Build that revision in an isolated test directory and pass its ELF
through the override. This does not authorize a deployed matcher change.

The first beta supports one device at a time. Revoke/refund on the old device
before approving a replacement elsewhere: the current refund UI binds its key
to the current grant signer. An already-replaced old key is retained, but needs
a separately reviewed recovery path; do not clear its storage.

## Release order

Source feature branches first. Obtain fresh exact release approval; upgrade v16,
then the exact tested adapter, deploy the compatible indexer with public execution
still disabled, and finally enable/promote the isolated development frontend.
Keep production disabled, singleton keeper untouched and demo stopped. Preserve
existing Ninja orders and test legacy callback compatibility. Verify exact live
bytes/source and readiness before the user creates one tiny session/order.
No live transaction or program/Fly deployment is authorized by this document.
