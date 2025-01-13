use std::{collections::HashMap, sync::Arc};

use alloy_primitives::B256;
use alloy_rpc_types::{Block, Header};
use crossbeam_channel::{Receiver, Sender};
use pc_common::{
    config::{TaikoChainConfig, TaikoConfig},
    driver::{DriverRequest, DriverResponse},
    runtime::spawn,
    taiko::{assemble_anchor_v2, compute_next_base_fee},
    types::{AnchorData, DuplexChannel},
    utils::verify_and_log_block,
};
use tracing::{debug, error};
use url::Url;

use crate::fetcher::BlockFetcher;

/// Driver that manages fetching/block sync
/// Three main functions:
/// - handle block sync from L1 (TODO)
/// - generate anchor transactions
/// - verify that preconf blocks and L2 blocks are in sync
///
/// L2 blocks are anchored to recent (max 64 block old) blocks, so the driver produces an anchor
/// periodically to make sure that when a tx comes the block is ready
/// Alternatively it also produces an anchor when requested by the sequencer (eg. because a block
/// was sealed). Two L2 blocks can have the same timestamps/anchor id
pub struct TaikoDriver {
    config: TaikoConfig,
    chain_config: TaikoChainConfig,
    /// Receiver for new l1 block updates
    l1_blocks_rx: Receiver<Header>,
    /// Receiver for new l1 block updates
    l2_blocks_rx: Receiver<Header>,
    /// Send anchor transactions and receive anchor requests
    to_sequencer: DuplexChannel<DriverResponse, DriverRequest>,
    /// Last confirmed L1 header
    last_l1_header: Header,
    /// L1 header that can be used to generate anchor txs (1-64 blocks old, NOT the current one)
    anchor_l1_header: Header,
    /// Could be either a preconf or a "real" L2 header
    last_l2_header: Header,
    /// preconf L2 blocks that need to be verified
    l2_blocks_to_verify: HashMap<u64, Arc<Block>>,
}

impl TaikoDriver {
    pub fn new(
        l1_rpc_url: Url,
        l1_ws_url: Url,
        config: TaikoConfig,
        chain_config: TaikoChainConfig,
        to_sequencer: DuplexChannel<DriverResponse, DriverRequest>,
    ) -> Self {
        let (l1_blocks_tx, l1_blocks_rx) = crossbeam_channel::unbounded();
        let l1_block_fetcher = BlockFetcher::new(l1_rpc_url, l1_ws_url, l1_blocks_tx);
        spawn(l1_block_fetcher.run("l1", true));

        let (l2_blocks_tx, l2_blocks_rx) = crossbeam_channel::unbounded();
        let taiko_block_fetcher =
            BlockFetcher::new(config.rpc_url.clone(), config.ws_url.clone(), l2_blocks_tx);
        spawn(taiko_block_fetcher.run("l2", false));

        Self {
            config,
            chain_config,
            l1_blocks_rx,
            l2_blocks_rx,
            to_sequencer,

            last_l1_header: Header::default(),
            anchor_l1_header: Header::default(),
            last_l2_header: Header::default(),
            l2_blocks_to_verify: HashMap::new(),
        }
    }

    #[tracing::instrument(skip_all, name = "driver")]
    pub fn run(mut self) {
        loop {
            crossbeam_channel::select! {
                recv(self.l1_blocks_rx) -> maybe_block => {
                    match maybe_block {
                        Ok(block) => self.handle_new_l1_block(block),
                        Err(err) => error!(%err, "l1 fetcher->driver is closed")
                    }
                }

                recv(self.l2_blocks_rx) -> maybe_block => {
                    match maybe_block {
                        Ok(block) => self.handle_new_l2_block(block),
                        Err(err) => error!(%err, "l2 fetcher->driver is closed")
                    }
                }

                recv(self.to_sequencer.rx) -> maybe_request => {
                    match maybe_request {
                        Ok(request) => self.handle_sequencer_request(request),
                        Err(err) => error!(%err, "sequencer->driver is closed")
                    }
                }

            }
        }
    }

    fn handle_new_l1_block(&mut self, new_header: Header) {
        debug!(bn = new_header.number, hash = %new_header.hash, "received new l1 block");

        if new_header.number >= self.last_l1_header.number &&
            new_header.hash != self.last_l1_header.hash
        {
            self.anchor_l1_header = std::mem::replace(&mut self.last_l1_header, new_header);

            self.assemble_and_send_anchor();
        }
    }

    fn handle_new_l2_block(&mut self, new_header: Header) {
        debug!(bn = new_header.number, hash = %new_header.hash, "received new l2 block");

        if new_header.number >= self.last_l2_header.number &&
            new_header.hash != self.last_l2_header.hash
        {
            self.last_l2_header = new_header.clone();
            self.assemble_and_send_anchor();

            self.send_block_update();
        }

        if let Some(preconf_block) = self.l2_blocks_to_verify.remove(&new_header.number) {
            verify_and_log_block(&preconf_block.header, &new_header, true);
        }
    }

    fn handle_sequencer_request(&mut self, request: DriverRequest) {
        match request {
            DriverRequest::AnchorV2 { block } => {
                let header = block.header.clone();

                // just received a preconf block that needs to be verified
                debug!(bn = header.number, hash = %header.hash, to_verify = self.l2_blocks_to_verify.len() + 1, "received new preconf l2 block");

                let insert = self.l2_blocks_to_verify.insert(header.number, block);
                assert!(
                    insert.is_none(),
                    "trying to insert preconf block twice bn={}",
                    header.number
                );

                self.last_l2_header = header;
                self.assemble_and_send_anchor();
            }

            DriverRequest::Refresh => {
                let bn = self.last_l2_header.number;
                let hash = self.last_l2_header.hash;
                debug!(bn, %hash, to_verify = self.l2_blocks_to_verify.len(), "received anchor refresh request");

                self.assemble_and_send_anchor();
            }
        }
    }

    fn send_block_update(&self) {
        if let Err(err) =
            self.to_sequencer.tx.send(DriverResponse::LastBlockNumber(self.last_l2_header.number))
        {
            error!(%err, "failed driver->sequencer block");
        }
    }

    fn assemble_and_send_anchor(&self) {
        if self.anchor_l1_header.number == 0 || self.last_l2_header.number == 0 {
            return;
        }

        let res_tx = self.to_sequencer.tx.clone();
        let config = self.config.clone();
        let chain_config = self.chain_config.clone();

        // this could be anything >= previous timestamp
        let anchor_timestamp = if self.anchor_l1_header.timestamp >= self.last_l2_header.timestamp {
            self.anchor_l1_header.timestamp
        } else {
            self.last_l2_header.timestamp
        };

        // anchor
        let anchor = AnchorParams {
            block_id: self.anchor_l1_header.number,
            state_root: self.anchor_l1_header.state_root,
            timestamp: anchor_timestamp,
        };

        // parent
        let parent = ParentParams {
            timestamp: self.last_l2_header.timestamp,
            gas_used: self.last_l2_header.gas_used,
            block_number: self.last_l2_header.number,
        };

        debug!(
            l1_head = self.last_l1_header.number,
            l2_head = self.last_l2_header.number,
            ?anchor,
            ?parent,
            "assembling anchor"
        );

        spawn(async move {
            if let Err(err) =
                compute_and_send_anchor(res_tx, config, chain_config, anchor, parent).await
            {
                error!(%err, "failed assembling anchor tx");
            }
        });
    }
}

#[derive(Debug)]
struct AnchorParams {
    block_id: u64,
    state_root: B256,
    timestamp: u64,
}

#[derive(Debug)]
struct ParentParams {
    timestamp: u64,
    gas_used: u64,
    block_number: u64,
}

async fn compute_and_send_anchor(
    res_tx: Sender<DriverResponse>,
    config: TaikoConfig,
    chain_config: TaikoChainConfig,
    anchor: AnchorParams,
    parent: ParentParams,
) -> eyre::Result<()> {
    let base_fee = compute_next_base_fee(
        &config,
        &chain_config,
        parent.gas_used,
        parent.timestamp,
        anchor.timestamp,
    )
    .await?;

    let anchor_tx = assemble_anchor_v2(
        &config,
        &chain_config,
        anchor.block_id,
        anchor.state_root,
        parent.gas_used.try_into()?,
        parent.block_number,
        base_fee,
    )?;

    let anchor_data = AnchorData {
        block_id: anchor.block_id,
        timestamp: anchor.timestamp,
        base_fee,
        state_root: anchor.state_root,
    };

    if let Err(err) = res_tx.send(DriverResponse::AnchorV2 { tx: anchor_tx, anchor_data }) {
        error!(%err, "failed driver->sequencer");
    }

    Ok(())
}
