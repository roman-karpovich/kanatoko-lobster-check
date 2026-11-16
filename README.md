# Lobster deadline replay

Two tests exercise adding and withdrawing Soroswap liquidity through Lobster,
using captured Protocol 29 mainnet state. Each test runs once with WASM built
from the pinned source and once with the vault WASM selected by Lobster's
mainnet factory. No transaction is signed or submitted.

## Run

Prerequisite: rustup with Rust 1.95.0 (`rustup toolchain install 1.95.0`).

```sh
./check.sh
# Once Cargo dependencies are available:
./check.sh --offline
```

The first build can download Rust crates. Test execution always replays the
committed captures, with no RPC fallback. Kanatoko 29.1.2 is installed from
crates.io. Expected: two passing tests, twice.

## What is observed

| Deadline | Add | Withdraw |
|---|---|---|
| captured ledger sequence + 60 | Lobster error 513 | Lobster error 514 |
| captured timestamp - 1 | Lobster error 513 | Lobster error 514 |
| captured timestamp | Lobster error 513 | Lobster error 514 |
| future timestamp | succeeds | succeeds |

The withdrawal test also advances the clock to a previously future deadline:
equality rejects, then a deadline one second later succeeds. Rejected calls
preserve the checked vault token balances, pool token balances, vault LP
balance and active-position state. Successful add creates LP shares;
successful withdraw burns them, returns tokens and clears the active position.

The Lobster API documentation calls the argument a ledger number. The real
integration uses timestamp seconds. Existing frontend and decision-service
callers use `99999999999`, so this does not demonstrate their current calls
failing. No loss of funds or production vulnerability is claimed.

## Pinned inputs

- Lobster source: `Lobster-Protocol/stellar` at
  `fff3798576f423d8235cb15e2b1f02c98d681f05`. The complete contract Rust sources,
  workspace manifests, lockfile and license are included under `upstream/lobster`.
- `wasm/lobster_contract.wasm`: built from that unchanged source using Rust
  1.95.0 and Stellar CLI 27.0.0; SHA-256
  `4aa026b618997775d74dfa54be386ffef9e94cbd7c033658ec16119911661ab2`.
  This is a different toolchain from upstream's documented release build.
- Mainnet factory:
  `CAFGQVKFCZITN7UJUOIJPMULGZRUR7RXG6DAYJ2VBGIGFWHHFGR6MCR5`.
  Its persistent `lob_hash` selected
  `a72a67ac4b9fa0ad561032d8d433d4da0024100cdb27f9098cb89997243b8382`.
  That exact code was fetched read-only as `wasm/lobster_mainnet.wasm`.
- The factory's persistent `SW_ROUTER` selected
  `CAG5LRYQ5JVEUI5TEID72EYOVX44TTUJT5BQR2J6J77FH65PCCFAJDDH`.
  The `.env.example` router belongs to testnet and is not used here.
- Kanatoko: the published `29.1.2` crate, pinned exactly in `Cargo.toml` and
  by registry checksum in `Cargo.lock`. Its local-address collision fix keeps
  a new vault from inheriting balances at an SDK counter address.
- `fixtures/add.json`: mainnet ledger 65508083, timestamp 1794826457;
  14 present and 8 confirmed-absent keys, zero final replay RPC reads.
- `fixtures/withdraw.json`: mainnet ledger 65508085, timestamp 1794826467;
  15 present and 8 confirmed-absent keys, zero final replay RPC reads.

Each capture contains its own coherent ledger provenance and content digests.
The test reads the XLM/USDC pair and reserve ratio through the captured router.

## Scope and reproduction of the build

Authorization is mocked and funding is local. The vault is registered locally
with the captured factory's router; no live user vault is modified. These are
Host execution checks, not transaction admission, fees, signatures or consensus
tests. Balances are checked to be zero before local vault funding.

To rebuild the source WASM, enter `upstream/lobster/SmartContract` and run
with Stellar CLI 27.0.0:

```sh
RUSTUP_TOOLCHAIN=1.95.0 RUSTC_WRAPPER= stellar contract build \
  --locked --package lobster-contract --out-dir /path/to/this/repro/wasm
```

Changing compiler or source can change the WASM hash. The fixture tests can
then check that exact new binary, but do not call it the pinned build above.
The tests support explicit read-only refresh via `KANATOKO_CAPTURE_RPC` when
called directly with `cargo test -- --test-threads=1`; `check.sh` deliberately
clears that variable. A network protocol change requires a matching Host and
new captures rather than an automatic fallback.

See `NOTICE` and `LICENSE`. Registry dependencies retain their own licenses.
