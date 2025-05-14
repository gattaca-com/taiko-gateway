# Makes sure the nightly-2025-02-26 toolchain is installed
toolchain := "nightly-2025-02-26"
set shell := ["bash", "-cu"]

fmt:
  rustup toolchain install {{toolchain}} > /dev/null 2>&1 && \
  cargo +{{toolchain}} fmt

fmt-check:
  rustup toolchain install {{toolchain}} > /dev/null 2>&1 && \
  cargo +{{toolchain}} fmt --check

clippy:
  cargo clippy --all-features --no-deps -- -D warnings

test-tx:
  source .env && \
  RPC_URL=https://rpc.helder.taiko.xyz \
  SENDER_KEY=${PROPOSER_SIGNER_KEY} \
  cargo test --package pc-tests --test spammer -- tests::test_send_locally --nocapture

test-many:
  source .env && \
  RPC_URL=https://rpc.helder.taiko.xyz \
  SENDER_KEY=${PROPOSER_SIGNER_KEY} \
  cargo test --package pc-tests --test spammer -- tests::test_spam_many --nocapture


docker-build:
  docker build -t taiko_gateway .

run:
  source .env && \
  CONFIG_FILE="gateway.toml" && \
  PROPOSER_SIGNER_KEY=${PROPOSER_SIGNER_KEY} \
  RUST_BACKTRACE=${RUST_BACKTRACE} \
  RUST_LOG=${RUST_LOG} \
  LOG_PATH=${LOG_PATH} \
  METRICS_PORT=${METRICS_PORT} \
  cargo run --bin gateway -- "$CONFIG_FILE"

