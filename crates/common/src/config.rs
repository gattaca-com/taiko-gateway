use std::{fs, time::Duration};

use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::taiko::{self, l2};

/// Config to deserialize config file
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
    pub rpc_url: Url,
    pub ws_url: Url,
    pub taiko_token: Address,
    pub l1_contract: Address,
    pub l2_contract: Address,
    #[serde(default = "default_bool::<false>")]
    pub force_reorgs: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GatewayConfig {
    pub rpc_port: u16,
    pub simulator_url: Url,
    pub l2_target_block_time_ms: u64,
    pub coinbase: Address,
    pub dry_run: bool,
    pub use_blobs: bool,
    pub use_batch: bool,
    #[serde(default = "default_bool::<false>")]
    pub force_reorgs: bool,
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
    /// Chain id for this RPC
    pub chain_id: u64,
    /// Address of the local state simulator to forward requests to
    pub simulator_url: Url,
    /// Address of RPC node with txpool on
    pub rpc_url: Url,
    /// Address of RPC node to subscribe to mempool
    pub ws_url: Url,
}

impl From<&StaticConfig> for RpcConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            port: config.gateway.rpc_port,
            chain_id: config.l2.chain_id,
            simulator_url: config.gateway.simulator_url.clone(),
            rpc_url: config.l2.rpc_url.clone(),
            ws_url: config.l2.ws_url.clone(),
        }
    }
}

pub struct SequencerConfig {
    pub target_block_time: Duration,
    pub coinbase_address: Address,
    pub dry_run: bool,
}

impl From<&StaticConfig> for SequencerConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            target_block_time: Duration::from_millis(config.gateway.l2_target_block_time_ms),
            coinbase_address: config.gateway.coinbase,
            dry_run: config.gateway.dry_run,
        }
    }
}

pub struct WokerConfig {
    /// Unique id for the worker
    pub id: String,
    /// Url to send requests to
    pub simulator_url: Url,
}

impl From<&StaticConfig> for WokerConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            id: config.l2.name.to_lowercase(),
            simulator_url: config.gateway.simulator_url.clone(),
        }
    }
}

pub struct ProposerConfig {
    pub continue_loop_time: Duration,
    pub force_reorgs: bool,
}

impl From<&StaticConfig> for ProposerConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            continue_loop_time: Duration::from_secs(1),
            force_reorgs: config.gateway.force_reorgs,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaikoConfig {
    /// non-preconf RPC
    pub rpc_url: Url,
    /// non-preconf WS
    pub ws_url: Url,
    /// preconf RPC
    pub preconf_url: Url,

    pub chain_id: u64,

    pub token_l1_address: Address,
    pub l1_contract_address: Address,
    pub l2_contract_address: Address,
}

/// Max difference between l1 block number of where proposeBlock is called and the anchored one
const MAX_ANCHOR_HEIGHT_OFFSET: u64 = 64;

/// Max gas limit for a block, this + anchor gas limit is the gas limit of the block
const BLOCK_MAX_GAS_LIMIT: u64 = 240_000_000;

/// Config with dynamic params stored on-chain
#[derive(Debug, Clone)]
pub struct TaikoChainConfig {
    pub base_fee_config: l2::TaikoData::BaseFeeConfig,
    pub max_anchor_height_offset: u64,
    /// Max difference between timestamp of the propose block and anchor timestamp
    pub max_anchor_timestamp_offset: u64,
    pub block_max_gas_limit: u64,
}

impl TaikoChainConfig {
    pub fn new(
        base_fee_config: l2::TaikoData::BaseFeeConfig,
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

impl From<taiko::l1::TaikoData::Config> for TaikoChainConfig {
    fn from(config: taiko::l1::TaikoData::Config) -> Self {
        Self::new(
            convert_base_fee_config(config.baseFeeConfig),
            config.maxAnchorHeightOffset,
            config.blockMaxGasLimit as u64,
        )
    }
}

const fn convert_base_fee_config(
    l1_base_fee_config: taiko::l1::TaikoData::BaseFeeConfig,
) -> taiko::l2::TaikoData::BaseFeeConfig {
    l2::TaikoData::BaseFeeConfig {
        adjustmentQuotient: l1_base_fee_config.adjustmentQuotient,
        sharingPctg: l1_base_fee_config.sharingPctg,
        gasIssuancePerSecond: l1_base_fee_config.gasIssuancePerSecond,
        minGasExcess: l1_base_fee_config.minGasExcess,
        maxGasIssuancePerBlock: l1_base_fee_config.maxGasIssuancePerBlock,
    }
}

impl TaikoChainConfig {
    pub fn new_helder() -> Self {
        Self::new(HELDER_BASE_FEE_CONFIG, MAX_ANCHOR_HEIGHT_OFFSET, BLOCK_MAX_GAS_LIMIT)
    }
}

impl From<&StaticConfig> for TaikoConfig {
    fn from(config: &StaticConfig) -> Self {
        Self {
            preconf_url: config.gateway.simulator_url.clone(),
            rpc_url: config.l2.rpc_url.clone(),
            ws_url: config.l2.ws_url.clone(),
            chain_id: config.l2.chain_id,
            token_l1_address: config.l2.taiko_token,
            l1_contract_address: config.l2.l1_contract,
            l2_contract_address: config.l2.l2_contract,
        }
    }
}

const HELDER_BASE_FEE_CONFIG: l2::TaikoData::BaseFeeConfig = l2::TaikoData::BaseFeeConfig {
    adjustmentQuotient: 8,
    sharingPctg: 0,
    gasIssuancePerSecond: 5_000_000,
    minGasExcess: 1_340_000_000,
    maxGasIssuancePerBlock: 600_000_000,
};
