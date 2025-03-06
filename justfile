# Makes sure the nightly-2024-10-01 toolchain is installed
toolchain := "nightly-2024-10-01"
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
  FULL_RPC=http://0.0.0.0:8545 \
  GATEWAY_RPC=http://localhost:10001 \
  SENDER_KEY=${PROPOSER_SIGNER_KEY} \
  cargo test --package pc-tests --test spammer -- tests::test_send_locally --nocapture

test-many:
  source .env && \
  FULL_RPC=http://0.0.0.0:8545 \
  GATEWAY_RPC=http://0.0.0.0:8545 \
  SENDER_KEY=${PROPOSER_SIGNER_KEY} \
  cargo test --package pc-tests --test spammer -- tests::test_spam_many --nocapture