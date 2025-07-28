use std::{fs, ops::Deref, path::PathBuf, time::Duration};

use alloy_consensus::constants::GWEI_TO_WEI;
use alloy_primitives::{Address, Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use eyre::ensure;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{proposer::BLOBS_SAFE_SIZE, taiko::BaseFeeConfig};

/// Config to deserialize toml config file
#[derive(Debug, Deserialize, Serialize)]
pub struct StaticConfig {
    pub app_id: String,
    pub l1: L1ChainConfig,
    pub l2: L2ChainConfig,
    pub gateway: GatewayConfig,
}

impl StaticConfig {
    pub fn validate(&self) -> eyre::Result<()> {
        ensure!(
            self.gateway.throttle_factor >= 0.0 && self.gateway.throttle_factor < 1.0,
            "throttle_factor must be between 0.0 and 1.0"
        );

        ensure!(
            !self.gateway.auto_deposit_bond_enabled || self.gateway.auto_deposit_bond_factor >= 1.0,
            "auto_deposit_bond_factor must be >= 1.0 if auto_deposit_bond_enabled is true"
        );

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct L1ChainConfig {
    pub name: String,
    pub rpc_url: Url,
    pub ws_url: Url,
    pub beacon_url: Url,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct L2ChainConfig {
    pub name: String,
    pub rpc_url: Urls,
    pub ws_url: Urls,
    pub taiko_token: Address,
    pub l1_contract: Address,
    pub l2_contract: Address,
    pub router_contract: Address,
    pub whitelist_contract: Address,
    pub wrapper_contract: Address,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConfig {
    pub rpc_port: u16,
    pub simulator_url: Url,
    pub l2_target_block_time_ms: u64,
    pub dry_run: bool,
    /// If we dont receive a new L1 block for this amount of time, stop sequencing
    pub l1_delay_secs: u64,
    /// Wait until this many blocks have passed to check that the L1 propose tx hasnt reorged out
    pub l1_safe_lag: u64,
    /// Number of blocks to wait before refreshing the anchor, the larger this is the more blocks
    /// we can fit in a batch, but we risk it getting stale when proposing
    ///
    /// This is on top of the l1_safe_lag above, so from the latest L1 blocks we keep
    /// blocks up to anchor_batch_lag + l1_safe_lag old
    pub anchor_batch_lag: u64,
    /// Url to post soft blocks to
    pub soft_block_url: Url,
    /// Url to get status from
    pub status_url: Url,
    #[serde(flatten)]
    pub lookahead: LookaheadConfig,
    pub jwt_secret_path: PathBuf,
    pub coinbase: Address,
    /// Number of simulators to run in parallel when sorting, higher means better blocks but more
    /// overhead
    pub max_sims_per_loop: usize,
    /// if there are more than this batches waiting to be proposed
    pub throttle_queue_target: usize,
    /// throttle block size, target size will be throttled by (1 - factor)^(queue_size - target)
    pub throttle_factor: f64,
    /// when a batch exceeds this size in blobs we'll propose it immediately
    #[serde(default = "default_usize::<3>")]
    pub blob_target: usize,
    /// Minimum priority fee to use for batch proposals in gwei
    #[serde(default = "default_priority_fee")]
    pub min_priority_fee: f64,

    /// Minimum bond calculation: min_bond = (bond_base + bond_per_batch) *
    /// n_batches_bond_threshold
    #[serde(default = "default_u64::<200>")]
    pub n_batches_bond_threshold: u64,
    /// Automatically deposit bond when below threshold
    #[serde(default = "default_bool::<true>")]
    pub auto_deposit_bond_enabled: bool,
    /// Deposit (min_bond * auto_bond_deposit_factor - contract_balance) when contract_balance is
    /// lower than min_bond
    #[serde(default = "default_auto_deposit_bond_factor")]
    pub auto_deposit_bond_factor: f64,

    /// Send alert if proposer's ETH balance falls below this value
    #[serde(default = "default_alert_eth_balance_threshold")]
    pub alert_eth_balance_threshold: f64,
    /// Send alert if total token balance (contract + wallet) falls below this value
    #[serde(default = "default_alert_total_token_threshold")]
    pub alert_total_token_threshold: f64,
}

pub const fn default_bool<const U: bool>() -> bool {
    U
}

pub const fn default_usize<const U: usize>() -> usize {
    U
}

pub const fn default_u64<const U: u64>() -> u64 {
    U
}

fn default_priority_fee() -> f64 {
    1.0
}

fn default_auto_deposit_bond_factor() -> f64 {
    2.0
}

fn default_alert_eth_balance_threshold() -> f64 {
    0.5
}

fn default_alert_total_token_threshold() -> f64 {
    1000.0
}

pub fn load_static_config() -> StaticConfig {
    let path =
        std::env::args().nth(1).expect("missing config path. Run with 'gateway my_config.toml'");
    let config_file = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("unable to find config file: '{}'", path));
    let config: StaticConfig = toml::from_str(&config_file).expect("failed to parse toml");

    config
}

/// Config with all ENV variables
pub struct EnvConfig {
    /// Private key to send L1 transactions
    pub proposer_signer_key: PrivateKeySigner,
}

pub fn load_env_vars() -> EnvConfig {
    let proposer_signer_key = std::env::var("PROPOSER_SIGNER_KEY")
        .expect("PROPOSER_SIGNER_KEY must be set")
        .parse()
        .expect("invalid proposer private key");

    EnvConfig { proposer_signer_key }
}

#[derive(Debug, Copy, Clone, Deserialize, Serialize)]
pub struct LookaheadConfig {
    // If current_operator = A, next_operator = B, delay_slots = 3, A stops creating new blocks
    // 32 - 3 = 29 slots in the epoch (this leaves 3 slots for posting the final batches)
    pub handover_window_slots: u64,
    // In example above, B will start creating blocks handover_start_buffer_ms after A stops
    pub handover_start_buffer_ms: u64,
}

pub struct RpcConfig {
    /// Port to open the RPC server on
    pub port: u16,
    /// Address of RPC node with txpool on
    pub rpc_url: Urls,
    /// Address of RPC node to subscribe to mempool
    pub ws_url: Urls,
}

impl From<&StaticConfig> for RpcConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            port: config.gateway.rpc_port,
            rpc_url: config.l2.rpc_url.clone(),
            ws_url: config.l2.ws_url.clone(),
        }
    }
}

pub struct SequencerConfig {
    pub simulator_url: Url,
    pub target_block_time: Duration,
    pub coinbase_address: Address,
    pub operator_address: Address,
    pub l1_delay: Duration,
    /// slots
    pub l1_safe_lag: u64,
    pub anchor_batch_lag: u64,
    pub soft_block_url: Url,
    pub status_url: Url,
    pub max_sims_per_loop: usize,
    pub jwt_secret: Bytes,
    pub throttle_queue_target: usize,
    pub throttle_factor: f64,
    pub batch_target_size: usize,
}

impl From<(&StaticConfig, Vec<u8>, Address)> for SequencerConfig {
    fn from((config, jwt_secret, operator_address): (&StaticConfig, Vec<u8>, Address)) -> Self {
        Self {
            simulator_url: config.gateway.simulator_url.clone(),
            target_block_time: Duration::from_millis(config.gateway.l2_target_block_time_ms),
            l1_delay: Duration::from_secs(config.gateway.l1_delay_secs),
            l1_safe_lag: config.gateway.l1_safe_lag,
            anchor_batch_lag: config.gateway.anchor_batch_lag,
            soft_block_url: config.gateway.soft_block_url.clone(),
            status_url: config.gateway.status_url.clone(),
            max_sims_per_loop: config.gateway.max_sims_per_loop,
            jwt_secret: jwt_secret.into(),
            coinbase_address: config.gateway.coinbase,
            operator_address,
            throttle_queue_target: config.gateway.throttle_queue_target,
            throttle_factor: config.gateway.throttle_factor,
            batch_target_size: config.gateway.blob_target * BLOBS_SAFE_SIZE,
        }
    }
}

pub struct ProposerConfig {
    pub dry_run: bool,
    /// time
    pub l1_safe_lag: Duration,
    pub coinbase: Address,
    pub batch_target_size: usize,
    /// wei
    pub min_priority_fee: u128,
}

impl From<&StaticConfig> for ProposerConfig {
    fn from(config: &StaticConfig) -> Self {
        let fee_wei = (config.gateway.min_priority_fee * GWEI_TO_WEI as f64).round() as u128;

        Self {
            dry_run: config.gateway.dry_run,
            l1_safe_lag: Duration::from_secs(config.gateway.l1_safe_lag * 12),
            coinbase: config.gateway.coinbase,
            batch_target_size: config.gateway.blob_target * BLOBS_SAFE_SIZE,
            min_priority_fee: fee_wei,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaikoConfig {
    /// preconf RPC
    pub preconf_url: Url,
    pub config: L2ChainConfig,
    pub params: TaikoChainParams,
}

impl Deref for TaikoConfig {
    type Target = L2ChainConfig;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

impl TaikoConfig {
    pub fn new(static_config: &StaticConfig, chain_config: TaikoChainParams) -> Self {
        Self {
            preconf_url: static_config.gateway.simulator_url.clone(),
            config: static_config.l2.clone(),
            params: chain_config,
        }
    }
}

/// Config with dynamic params stored on-chain
#[derive(Debug, Clone, Copy)]
pub struct TaikoChainParams {
    pub chain_id: u64,
    pub base_fee_config: BaseFeeConfig,
    /// Max difference between l1 block number of where proposeBlock is called and the anchored one
    pub max_anchor_height_offset: u64,
    /// Max difference between timestamp of the propose block and anchor timestamp
    pub max_anchor_timestamp_offset: u64,
    /// Max gas limit for a block, this + anchor gas limit is the gas limit of the block
    pub block_max_gas_limit: u128,
    /// Liveness bond base
    pub bond_base: U256,
    /// Liveness bond multiplier
    pub bond_per_block: U256,
    /// Max number of blocks in a batch
    pub max_blocks_per_batch: usize,
}

impl TaikoChainParams {
    pub const fn new(
        chain_id: u64,
        base_fee_config: BaseFeeConfig,
        max_anchor_height_offset: u64,
        block_max_gas_limit: u64,
        bond_base: U256,
        bond_per_block: U256,
        max_blocks_per_batch: usize,
    ) -> Self {
        Self {
            chain_id,
            base_fee_config,
            max_anchor_height_offset,
            max_anchor_timestamp_offset: max_anchor_height_offset * 12,
            block_max_gas_limit: block_max_gas_limit as u128,
            bond_base,
            bond_per_block,
            max_blocks_per_batch,
        }
    }

    pub fn safe_anchor_height_offset(&self) -> u64 {
        self.max_anchor_height_offset - 2
    }
}

impl TaikoChainParams {
    #[cfg(test)]
    pub const fn new_helder() -> Self {
        const BASE_FEE_CONFIG: BaseFeeConfig = BaseFeeConfig {
            adjustment_quotient: 8,
            sharing_pctg: 75,
            gas_issuance_per_second: 5_000_000,
            min_gas_excess: 1_340_000_000,
            max_gas_issuance_per_block: 600_000_000,
        };

        const MAX_ANCHOR_HEIGHT_OFFSET: u64 = 64;
        const BLOCK_MAX_GAS_LIMIT: u64 = 240_000_000;

        const HELDER_TAIKO_CHAIN_ID: u64 = 167010;
        const MAX_BLOCKS_PER_BATCH: usize = 768;

        Self::new(
            HELDER_TAIKO_CHAIN_ID,
            BASE_FEE_CONFIG,
            MAX_ANCHOR_HEIGHT_OFFSET,
            BLOCK_MAX_GAS_LIMIT,
            U256::ZERO,
            U256::ZERO,
            MAX_BLOCKS_PER_BATCH,
        )
    }
}

#[derive(Debug, Clone)]
pub struct Urls(pub Vec<Url>);

impl std::fmt::Display for Urls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.len() == 1 {
            write!(f, "{}", self.0[0])
        } else {
            f.write_str("[")?;
            for (i, url) in self.0.iter().enumerate() {
                if i != 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{url}")?;
            }
            f.write_str("]")
        }
    }
}

impl Deref for Urls {
    type Target = Vec<Url>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Url> for Urls {
    fn from(url: Url) -> Self {
        Self(vec![url])
    }
}

impl<'de> Deserialize<'de> for Urls {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            One(Url),
            Many(Vec<Url>),
        }

        let helper = Helper::deserialize(deserializer)?;

        match helper {
            Helper::One(url) => Ok(Self(vec![url])),
            Helper::Many(urls) => {
                if urls.is_empty() {
                    Err(serde::de::Error::custom("cannot be an empty vector"))
                } else {
                    Ok(Self(urls))
                }
            }
        }
    }
}

impl Serialize for Urls {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.0.len() == 1 {
            self.0[0].serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}
