use std::sync::Arc;

use alloy_consensus::TxEnvelope;
use alloy_rpc_types::Block;

use crate::types::AnchorData;

pub enum DriverRequest {
    /// Sequencer has just sealed a block with this header, need an anchor to keep going
    AnchorV2 { block: Arc<Block> },
    /// Refresh on last block
    Refresh,
}

pub enum DriverResponse {
    AnchorV2 { tx: TxEnvelope, anchor_data: AnchorData },
    // FIXME: this should come from a single place
    LastBlockNumber(u64),
}
