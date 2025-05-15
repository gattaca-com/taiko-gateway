use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

use ::backtrace::Backtrace;
use alloy_primitives::hex;
use alloy_rpc_types::Header;
use alloy_sol_types::SolInterface;
use tracing::{error, info};
use tracing_appender::{non_blocking::WorkerGuard, rolling::Rotation};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer};
use url::Url;

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
static APP_ID: OnceLock<String> = OnceLock::new();
static DISCORD_WEBHOOK_URL: OnceLock<Url> = OnceLock::new();
static DISCORD_USER: OnceLock<String> = OnceLock::new();

pub fn init_statics(app_id: String) {
    APP_ID.set(app_id).unwrap();

    if let Ok(webhook_url) = std::env::var("DISCORD_WEBHOOK_URL") {
        DISCORD_WEBHOOK_URL
            .set(Url::parse(&webhook_url).expect("invalid DISCORD_WEBHOOK_URL"))
            .unwrap();
    }
    if let Ok(user_tag) = std::env::var("DISCORD_USER") {
        DISCORD_USER.set(user_tag).unwrap();
    }
}

pub fn alert_discord(message: &str) {
    if is_test_env() {
        return;
    }

    let Some(webhook_url) = DISCORD_WEBHOOK_URL.get() else { return };
    let user_tag = DISCORD_USER.get().map(String::as_str).unwrap_or("");

    let max_len = 1850.min(message.len());
    let msg = format!("{user_tag}\n{}", &message[..max_len]);

    let content = HashMap::from([("content", msg.clone())]);

    if let Err(err) =
        reqwest::blocking::Client::new().post(webhook_url.clone()).json(&content).send()
    {
        error!("failed to send discord alert: {err}");
        eprintln!("failed to send discord alert: {err}");
    }
}

const fn is_test_env() -> bool {
    cfg!(test) || cfg!(debug_assertions)
}

pub fn init_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let app_id = APP_ID.get().map(String::as_str).unwrap_or("unknown");
        let backtrace = Backtrace::new();
        let crash_log =
            format!("panic: APP_ID: `{app_id}`\n{info}\nfull backtrace:\n{backtrace:?}\n");
        error!("{crash_log}");
        eprintln!("{crash_log}");
        alert_discord(&crash_log);
    }));
}

// Tracing

pub fn initialize_test_tracing() {
    tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).init();
}

pub fn init_tracing_log() -> (WorkerGuard, Option<WorkerGuard>) {
    let format = tracing_subscriber::fmt::format()
        .with_level(true)
        .with_thread_ids(false)
        .with_target(false);

    let log_level = std::env::var("RUST_LOG")
        .map(|lev| lev.parse().expect("invalid RUST_LOG, change to eg 'info'"))
        .unwrap_or(tracing::Level::INFO);

    if is_test_env() {
        let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
        let layer = tracing_subscriber::fmt::layer()
            .event_format(format)
            .with_writer(writer)
            .with_filter(get_crate_filter(log_level));

        tracing_subscriber::registry().with(layer).init();

        (guard, None)
    } else {
        let (stdout_writer, stdout_guard) = tracing_appender::non_blocking(std::io::stdout());
        let layer = tracing_subscriber::fmt::layer()
            .event_format(format.clone())
            .with_writer(stdout_writer)
            .with_filter(get_crate_filter(log_level));

        let registry = tracing_subscriber::registry().with(layer);

        if let Ok(path) = std::env::var("LOG_PATH") {
            let max_logs = std::env::var("MAX_LOGS")
                .unwrap_or("30".to_string())
                .parse::<usize>()
                .expect("invalid MAX_LOGS, change to eg '30'");

            let file_appender = tracing_appender::rolling::Builder::new()
                .max_log_files(max_logs)
                .rotation(Rotation::DAILY)
                .build(path)
                .expect("failed to create file log appender");

            let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);
            let layer = tracing_subscriber::fmt::layer()
                .event_format(format)
                .with_ansi(false)
                .with_writer(file_writer)
                .with_filter(get_crate_filter(log_level));

            registry.with(layer).init();
            (stdout_guard, Some(file_guard))
        } else {
            registry.init();
            (stdout_guard, None)
        }
    }
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

pub fn extract_revert_reason<T: SolInterface>(input: &str) -> Option<T> {
    const MARKER: &str = "data: \"0x";

    let start = input.find(MARKER)?;
    let code_start = start + MARKER.len();
    let code_end = code_start + 8; // 4 bytes in hex
    let hex_str = input.get(code_start..code_end)?;
    let bytes = hex::decode(hex_str).ok()?;

    let err = T::abi_decode(&bytes, true).ok()?;

    Some(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taiko::pacaya::preconf::PreconfWhitelist::{
        InvalidOperatorCount, PreconfWhitelistErrors,
    };

    #[test]
    fn test_extract_revert_reason() {
        let input = r#"server returned an error response: error code 3: execution reverted, data: "0x7d943b8f""#;
        let result: PreconfWhitelistErrors = extract_revert_reason(input).unwrap();
        assert_eq!(PreconfWhitelistErrors::InvalidOperatorCount(InvalidOperatorCount {}), result);
    }
}
