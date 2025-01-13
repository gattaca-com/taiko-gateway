use alloy_consensus::BlobTransactionSidecar;
use alloy_primitives::{Address, Bytes};
use pc_common::{
    config::TaikoConfig,
    proposer::{ProposeParams, ProposerRequest},
};

use crate::taiko::TaikoL1Proposer;

/// All extra information needed to build a "proposeBlock" tx
#[derive(Debug, Clone, Copy)]
pub struct ProposeData {
    pub coinbase: Address,
}

impl ProposeData {
    pub fn new(coinbase: Address) -> Self {
        Self { coinbase }
    }
}

pub enum Propose {
    Contract(TaikoL1Proposer),
}

impl Propose {
    pub fn new_contract(config: TaikoConfig, context: ProposeData) -> Self {
        Self::Contract(TaikoL1Proposer::new(config, context))
    }

    pub async fn get_propose_tx_blob(&self, request: ProposerRequest) -> eyre::Result<BlobFields> {
        let params = ProposeParams::from(request);
        match self {
            Self::Contract(p) => p.get_propose_tx_blob(params),
        }
    }

    pub async fn get_propose_tx_calldata(
        &self,
        request: ProposerRequest,
    ) -> eyre::Result<CalldataFields> {
        let params = ProposeParams::from(request);
        match self {
            Self::Contract(p) => p.get_propose_tx_calldata(params),
        }
    }

    pub async fn get_propose_txs_blob(
        &self,
        requests: Vec<ProposerRequest>,
    ) -> eyre::Result<BlobFields> {
        let params = requests.into_iter().map(ProposeParams::from).collect();
        match self {
            Self::Contract(p) => p.get_propose_txs_blob(params),
        }
    }

    pub async fn get_propose_txs_calldata(
        &self,
        requests: Vec<ProposerRequest>,
    ) -> eyre::Result<CalldataFields> {
        let params = requests.into_iter().map(ProposeParams::from).collect();
        match self {
            Self::Contract(p) => p.get_propose_txs_calldata(params),
        }
    }
}

pub struct CalldataFields {
    pub to: Address,
    pub input: Bytes,
}

pub struct BlobFields {
    pub to: Address,
    pub input: Bytes,
    pub sidecar: BlobTransactionSidecar,
}
