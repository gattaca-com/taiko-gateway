use alloy_primitives::{Bytes, B256};
use jsonrpsee::{core::RpcResult, proc_macros::rpc};

use crate::{
    sequencer::{SealBlockResponse, SimulateTxResponse, StateId},
    types::BlockEnv,
};

/// Eth API used by the gateway to receive transactions directly
#[rpc(client, server, namespace = "eth")]
pub trait EthApi {
    /// Sends signed transaction, returning its hash
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(&self, bytes: Bytes) -> RpcResult<B256>;
}

/// API used by the sequencer to simulate transactions and commit state changes
/// NOTE: this should be moved away from JSON-RPC
#[rpc(client, namespace = "eth")]
pub trait SimulateApi {
    /// Simulate a transaction against a given state
    #[method(name = "simulateTxAtState")]
    async fn simulate_tx_at_state(
        &self,
        bytes: Bytes,
        state_id: StateId,
    ) -> RpcResult<SimulateTxResponse>;
    /// Simulate an anchor transaction and initialize the new pending block (needs to be called
    /// after every `seal_block`), this is always simulated after the latest sealed block (chain
    /// head)
    #[method(name = "simulateAnchorTx")]
    async fn simulate_anchor_tx(
        &self,
        bytes: Bytes,
        block_env: BlockEnv,
        extra_data: Bytes,
    ) -> RpcResult<SimulateTxResponse>;
    /// Seal a block with the given state id
    #[method(name = "sealBlock")]
    async fn seal_block(&self, state_id: StateId) -> RpcResult<SealBlockResponse>;
}
