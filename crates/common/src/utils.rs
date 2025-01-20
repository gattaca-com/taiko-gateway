use std::time::{SystemTime, UNIX_EPOCH};

use ::backtrace::Backtrace;
use alloy_rpc_types::Header;
use tracing::{error, info};
use tracing_appender::{non_blocking::WorkerGuard, rolling::Rotation};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};

// Time

/// Seconds
pub fn utcnow_sec() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}
/// Millis
pub fn utcnow_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}
/// Micros
pub fn utcnow_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64
}
/// Nanos
pub fn utcnow_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

pub fn timestamp_of_slot_start_ms(slot: u64, genesis_time_sec: u64) -> u64 {
    (genesis_time_sec + slot * 12) * 1_000
}

pub fn timestamp_of_prev_slot_start_ms(slot: u64, genesis_time_sec: u64) -> u64 {
    (genesis_time_sec + slot.saturating_sub(1) * 12) * 1_000
}

pub fn verify_and_log_block(preconf_header: &Header, new_header: &Header, panic_on_error: bool) {
    let header = &preconf_header;
    if header.hash != new_header.hash {
        let our_block = format!("ours: {}", serde_json::to_string_pretty(&header).unwrap(),);
        let chain_block = format!("chain: {}", serde_json::to_string_pretty(&new_header).unwrap(),);

        error!(bn =? new_header.number, hash =? new_header.hash, "failed l2 block verification");
        error!(%our_block);
        error!(%chain_block);

        if panic_on_error {
            panic!("block verify mismatch\n{our_block}\n{chain_block}")
        } else {
            alert_discord(&format!("NOTE: block verify mismatch\n{our_block}\n{chain_block}",));
        }
    } else {
        info!(bn =? new_header.number, hash =? new_header.hash, "verified l2 block");
    }
}

// Alerts

pub fn alert_discord(message: &str) {
    error!("MOCK DISCORD ALERT: {}", message);
}

const fn is_test_env() -> bool {
    cfg!(test) || cfg!(debug_assertions)
}

pub fn initialize_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let backtrace = Backtrace::new();
        let crash_log = format!("panic: {info}\nfull backtrace:\n{backtrace:?}\n");
        error!("{crash_log}");
        eprintln!("{crash_log}");
        alert_discord(&crash_log);
    }));
}

// Tracing

pub fn initialize_test_tracing() {
    tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).init();
}

pub fn initialize_tracing_log() -> WorkerGuard {
    let log_level = std::env::var("RUST_LOG")
        .map(|lev| lev.parse().expect("invalid RUST_LOG, change to eg 'info'"))
        .unwrap_or(tracing::Level::INFO);

    let (writer, guard) = if is_test_env() {
        tracing_appender::non_blocking(std::io::stdout())
    } else {
        let log_path = std::env::var("LOG_PATH").unwrap_or("/logs".into());
        let file_appender = tracing_appender::rolling::Builder::new()
            .max_log_files(30)
            .rotation(Rotation::DAILY)
            .build(log_path)
            .expect("failed to create log appender!");

        tracing_appender::non_blocking(file_appender)
    };

    let filter = get_crate_filter(log_level);
    let layer =
        tracing_subscriber::fmt::layer().with_target(false).with_writer(writer).with_filter(filter);
    tracing_subscriber::registry().with(layer).init();

    guard
}

pub const OUR_CRATES: [&str; 4] = ["common", "proposer", "rpc", "sequencer"];

/// Make sure we only get logs for our crates and exlude the others
fn get_crate_filter(crates_level: tracing::Level) -> EnvFilter {
    let mut env_filter = EnvFilter::new("info");

    for our_crate in OUR_CRATES {
        env_filter =
            env_filter.add_directive(format!("pc_{our_crate}={crates_level}").parse().unwrap())
    }

    env_filter
}
