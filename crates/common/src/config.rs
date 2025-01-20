use std::{fs, ops::Deref, time::Duration};

use alloy_primitives::{Address, B256};
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
    pub chain_id: u64,
    pub rpc_url: Url,
    pub ws_url: Url,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct L2ChainConfig {
    pub name: String,
    pub chain_id: u64,
    /// non-preconf RPC
    pub rpc_url: Url,
    /// non-preconf WS
    pub ws_url: Url,
    pub taiko_token: Address,
    pub l1_contract: Address,
    pub l2_contract: Address,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConfig {
    pub rpc_port: u16,
    pub simulator_url: Url,
    pub propose_frequency_secs: u64,
    pub l2_target_block_time_ms: u64,
    pub coinbase: Address,
    pub dry_run: bool,
    pub use_blobs: bool,
    #[serde(default = "default_bool::<false>")]
    pub force_reorgs: bool,
    pub anchor_input: String,
}

pub const fn default_bool<const U: bool>() -> bool {
    U
}

pub fn load_static_config() -> StaticConfig {
    let path =
        std::env::args().nth(1).expect("missing config path. Run with 'gateway my_config.toml'");
    let config_file = fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("Unable to find config file: '{}'", path));
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
        .expect("invalid private key");

    EnvConfig { proposer_signer_key }
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
    pub dry_run: bool,
}

impl From<&StaticConfig> for SequencerConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            simulator_url: config.gateway.simulator_url.clone(),
            target_block_time: Duration::from_millis(config.gateway.l2_target_block_time_ms),
            coinbase_address: config.gateway.coinbase,
            dry_run: config.gateway.dry_run,
        }
    }
}

pub struct ProposerConfig {
    pub propose_frequency: Duration,
    pub force_reorgs: bool,
    pub use_blobs: bool,
    pub dry_run: bool,
}

impl From<&StaticConfig> for ProposerConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            propose_frequency: Duration::from_secs(config.gateway.propose_frequency_secs),
            force_reorgs: config.gateway.force_reorgs,
            use_blobs: config.gateway.use_blobs,
            dry_run: config.gateway.dry_run,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaikoConfig {
    /// preconf RPC
    pub preconf_url: Url,
    pub config: L2ChainConfig,
    pub params: TaikoChainParams,
    pub anchor_input: B256,
}

impl Deref for TaikoConfig {
    type Target = L2ChainConfig;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

impl TaikoConfig {
    pub fn new(static_config: &StaticConfig, chain_config: TaikoChainParams) -> Self {
        let mut anchor_input = static_config.gateway.anchor_input.as_bytes().to_vec();
        anchor_input.resize(32, 0);
        let anchor_input = B256::try_from(anchor_input.as_slice()).unwrap();

        Self {
            preconf_url: static_config.gateway.simulator_url.clone(),
            config: static_config.l2.clone(),
            params: chain_config,
            anchor_input,
        }
    }
}

/// Config with dynamic params stored on-chain
#[derive(Debug, Clone, Copy)]
pub struct TaikoChainParams {
    pub base_fee_config: BaseFeeConfig,
    /// Max difference between l1 block number of where proposeBlock is called and the anchored one
    pub max_anchor_height_offset: u64,
    /// Max difference between timestamp of the propose block and anchor timestamp
    pub max_anchor_timestamp_offset: u64,
    /// Max gas limit for a block, this + anchor gas limit is the gas limit of the block
    pub block_max_gas_limit: u64,
}

impl TaikoChainParams {
    pub const fn new(
        base_fee_config: BaseFeeConfig,
        max_anchor_height_offset: u64,
        block_max_gas_limit: u64,
    ) -> Self {
        Self {
            base_fee_config,
            max_anchor_height_offset,
            max_anchor_timestamp_offset: max_anchor_height_offset * 12,
            block_max_gas_limit,
        }
    }
}

impl TaikoChainParams {
    pub const fn new_helder() -> Self {
        const BASE_FEE_CONFIG: BaseFeeConfig = BaseFeeConfig {
            adjustment_quotient: 8,
            sharing_pctg: 0,
            gas_issuance_per_second: 5_000_000,
            min_gas_excess: 1_340_000_000,
            max_gas_issuance_per_block: 600_000_000,
        };

        const MAX_ANCHOR_HEIGHT_OFFSET: u64 = 64;
        const BLOCK_MAX_GAS_LIMIT: u64 = 240_000_000;

        Self::new(BASE_FEE_CONFIG, MAX_ANCHOR_HEIGHT_OFFSET, BLOCK_MAX_GAS_LIMIT)
    }
}
