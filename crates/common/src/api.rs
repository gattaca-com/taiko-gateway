use alloy_eips::BlockId;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rpc_types::{Block, BlockNumberOrTag, TransactionReceipt};
use jsonrpsee::{core::RpcResult, proc_macros::rpc};

use crate::{
    sequencer::{CommitStateResponse, SealBlockResponse, SimulateTxResponse, StateId},
    types::BlockEnv,
};

/// Minimal subset of the Ethereum JSON-RPC API implemented by the gateway to receive
/// transactions and query preconf state. Other methods are expected to be routed to full nodes
/// before reaching the gateway. Note that some methods, e.g. `blockByNumber` or `getBalance` may
/// access state that can be served by a full node
/// TODO: evaluate if we need some smart routing logic to route such requests away from the gateway
#[rpc(client, server, namespace = "eth")]
pub trait EthApi {
    /// Sends signed transaction, returning its hash
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(&self, bytes: Bytes) -> RpcResult<B256>;

    /// Returns the receipt of a transaction by transaction hash.
    #[method(name = "getTransactionReceipt")]
    async fn transaction_receipt(&self, hash: B256) -> RpcResult<Option<TransactionReceipt>>;

    /// Returns the number of most recent block.
    /// Note that this returns the preconf head block, which can be >= than the latest confirmed
    /// block
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> RpcResult<U256>;

    /// Returns information about a block by number.
    /// Note that this may include previous block numbers which could be served outside of the
    /// gateway so we could have some logic to route this away from the gateway
    #[method(name = "getBlockByNumber")]
    async fn block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<Block>>;

    /// Returns information about a block by hash.
    /// See Note on `getBlockByNumber`
    #[method(name = "getBlockByHash")]
    async fn block_by_hash(&self, hash: B256, full: bool) -> RpcResult<Option<Block>>;

    /// Returns the number of transactions sent from an address at given block number.
    /// See Note on `getBlockByNumber`
    #[method(name = "getTransactionCount")]
    async fn transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
    ) -> RpcResult<U256>;

    /// Returns the balance of the account of given address.
    #[method(name = "getBalance")]
    async fn balance(&self, address: Address, block_number: Option<BlockId>) -> RpcResult<U256>;
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
    /// Update the state at the head of the chain
    #[method(name = "commitState")]
    async fn commit_state(&self, state_id: StateId) -> RpcResult<CommitStateResponse>;
    /// Seal a block with from the latest committed state
    #[method(name = "sealBlock")]
    async fn seal_block(&self) -> RpcResult<SealBlockResponse>;
}
