#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
# The checked-in fixtures are the default; this entrypoint never refreshes them.
unset KANATOKO_CAPTURE_RPC LOBSTER_MAINNET_WASM
cargo +1.95.0 test --locked "$@"
LOBSTER_MAINNET_WASM=1 cargo +1.95.0 test --locked --offline
