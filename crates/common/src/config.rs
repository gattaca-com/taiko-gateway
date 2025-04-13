use std::{fs, ops::Deref, path::PathBuf, time::Duration};

use alloy_primitives::{Address, U256};
use alloy_signer_local::PrivateKeySigner;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::taiko::BaseFeeConfig;

/// Config to deserialize toml config file
#[derive(Debug, Deserialize, Serialize)]
pub struct StaticConfig {
    pub app_id: String,
    pub l1: L1ChainConfig,
    pub l2: L2ChainConfig,
    pub gateway: GatewayConfig,
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
    /// non-preconf RPC
    pub rpc_url: Url,
    /// non-preconf WS
    pub ws_url: Url,
    pub taiko_token: Address,
    pub l1_contract: Address,
    pub l2_contract: Address,
    pub router_contract: Address,
    pub whitelist_contract: Address,
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
    #[serde(flatten)]
    pub lookahead: LookaheadConfig,
    pub jwt_secret_path: PathBuf,
    pub coinbase: Address,
    /// Number of simulators to run in parallel when sorting, higher means better blocks but more
    /// overhead
    pub max_sims_per_loop: usize,
}

pub const fn default_bool<const U: bool>() -> bool {
    U
}

pub const fn default_usize<const U: usize>() -> usize {
    U
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
    pub rpc_url: Url,
    /// Address of RPC node to subscribe to mempool
    pub ws_url: Url,
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
    /// blocks
    pub l1_safe_lag: u64,
    pub anchor_batch_lag: u64,
    pub soft_block_url: Url,
    pub max_sims_per_loop: usize,
    pub jwt_secret: Vec<u8>,
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
            max_sims_per_loop: config.gateway.max_sims_per_loop,
            jwt_secret,
            coinbase_address: config.gateway.coinbase,
            operator_address,
        }
    }
}

pub struct ProposerConfig {
    pub dry_run: bool,
    /// time
    pub l1_safe_lag: Duration,
    pub coinbase: Address,
}

impl From<&StaticConfig> for ProposerConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            dry_run: config.gateway.dry_run,
            l1_safe_lag: Duration::from_secs(config.gateway.l1_safe_lag * 12),
            coinbase: config.gateway.coinbase,
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
