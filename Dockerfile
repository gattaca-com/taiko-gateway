FROM lukemathwalker/cargo-chef:latest-rust-1.84.0 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder 
COPY --from=planner /app/recipe.json recipe.json

RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN cargo build --release --bin gateway


FROM debian:stable-slim AS runtime
WORKDIR /app

RUN apt-get update
RUN apt-get install -y openssl ca-certificates libssl3 libssl-dev

COPY --from=builder /app/target/release/gateway /usr/local/bin
ENTRYPOINT ["/usr/local/bin/gateway"]
