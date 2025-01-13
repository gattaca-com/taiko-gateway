use pc_common::{
    config::TaikoConfig,
    proposer::ProposeParams,
    taiko::{propose_block_v2_blob, propose_block_v2_calldata, propose_blocks_v2_calldata},
};

use crate::types::{BlobFields, CalldataFields, ProposeData};

#[derive(Clone)]
pub struct TaikoL1Proposer {
    config: TaikoConfig,
    context: ProposeData,
}

impl TaikoL1Proposer {
    pub fn new(config: TaikoConfig, context: ProposeData) -> Self {
        Self { config, context }
    }

    pub fn get_propose_tx_blob(&self, params: ProposeParams) -> eyre::Result<BlobFields> {
        let (input, sidecar) = propose_block_v2_blob(params, self.context.coinbase)?;
        let to = self.config.l1_contract_address;

        Ok(BlobFields { to, input: input.into(), sidecar })
    }

    pub fn get_propose_tx_calldata(&self, params: ProposeParams) -> eyre::Result<CalldataFields> {
        let input = propose_block_v2_calldata(params, self.context.coinbase);

        let to = self.config.l1_contract_address;

        Ok(CalldataFields { to, input: input.into() })
    }

    pub fn get_propose_txs_blob(&self, _params: Vec<ProposeParams>) -> eyre::Result<BlobFields> {
        unimplemented!("blocks and batch not supported yet")
    }

    pub fn get_propose_txs_calldata(
        &self,
        params: Vec<ProposeParams>,
    ) -> eyre::Result<CalldataFields> {
        let input = propose_blocks_v2_calldata(params, self.context.coinbase);

        let to = self.config.l1_contract_address;

        Ok(CalldataFields { to, input: input.into() })
    }
}
