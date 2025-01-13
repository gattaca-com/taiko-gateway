use std::time::Instant;

use alloy_consensus::{Transaction, TxEnvelope};
use alloy_primitives::utils::format_ether;
use eyre::bail;
use pc_common::{sequencer::StateId, types::AnchorData};
use tracing::debug;

/// Sequencing state
/// For now keep it naive with no branching to sequence FCFS
pub struct SequencerContext {
    /// Highest of chain head and preconf head
    pub head_block_number: u64,
    /// Env for block being built, only created after a successful anchor sim
    pub anchor_env: Option<AnchorEnv>,
    /// Next anchor data to simulate and apply, if missing we cannot sequence
    /// This needs to be refreshed at every block
    pub anchor_next: Option<(TxEnvelope, AnchorData)>,
    /// Time to stop sequencing, this is periodically reset in the proposer loop
    pub stop_by: Instant,
    /// Whether we have a "simulation" in flight
    pub is_simulating: bool,
    /// Whether we are waiting for an anchor tx to be sent from driver
    pub waiting_for_anchor: bool,
}

impl SequencerContext {
    pub fn new() -> Self {
        Self {
            head_block_number: 0,
            anchor_env: None,
            anchor_next: None,
            stop_by: Instant::now(),
            is_simulating: false,
            waiting_for_anchor: false,
        }
    }

    /// Add user transaction to the live block env, returns error if transaction was simulated on
    /// wrong state or if env is missing
    pub fn sequence_tx(
        &mut self,
        origin_state_id: StateId,
        new_state_id: StateId,
        gas_used: u128,
        builder_payment: u128,
    ) -> eyre::Result<()> {
        if let Some(anchor_env) = self.anchor_env.as_mut() {
            if anchor_env.tip_state_id != origin_state_id {
                bail!(
                    "tx simulated on wrong state id, expected={}, got={}",
                    anchor_env.tip_state_id,
                    origin_state_id
                );
            }

            // advance local state
            anchor_env.tip_state_id = new_state_id;
            anchor_env.running_gas_used += gas_used;
            anchor_env.running_builder_payment += builder_payment;
            if anchor_env.first_sequenced.is_none() {
                anchor_env.first_sequenced = Some(Instant::now())
            }
            anchor_env.user_txs += 1;

            debug!(
                %origin_state_id,
                %new_state_id,
                gas_used,
                payment = format_ether(builder_payment),
                "sequenced user tx"
            );

            Ok(())
        } else {
            bail!("missing anchor env")
        }
    }

    pub fn is_anchor_for_this_block(&self, tx: &TxEnvelope) -> bool {
        // anchor nonce is always the parent block number
        self.head_block_number == tx.nonce()
    }
}

/// Block env with extra anchor data for the block being built, created only after a successful
/// anchor sim
#[derive(Debug, Clone)]
pub struct AnchorEnv {
    /// Block number (head + 1)
    pub block_number: u64,
    /// Data of anchor for this block
    pub data: AnchorData,
    /// Time when the first user tx was sequenced in the block
    pub first_sequenced: Option<Instant>,
    /// Running total of gas used in the block
    pub running_gas_used: u128,
    /// Running total of builder payment in the block
    pub running_builder_payment: u128,
    /// State to sequence txs on
    pub tip_state_id: StateId,
    /// How many user txs were sequenced in this block
    pub user_txs: u64,
}

impl AnchorEnv {
    /// State id after a successful anchor sim
    pub fn new(block_number: u64, anchor_data: AnchorData, state_id: StateId) -> Self {
        Self {
            block_number,
            data: anchor_data,
            tip_state_id: state_id,
            first_sequenced: None,
            running_gas_used: 0,
            running_builder_payment: 0,
            user_txs: 0,
        }
    }

    pub fn is_building(&self) -> bool {
        self.first_sequenced.is_some()
    }

    /// Returns true if self has a lower (older) anchor block id
    pub fn is_older_anchor(&self, other: &Self) -> bool {
        self.data.block_id < other.data.block_id
    }
}
