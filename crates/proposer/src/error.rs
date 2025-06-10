#[derive(Debug, thiserror::Error)]
pub enum ProposerError {
    #[error("tx not included in next blocks, try bumping fees")]
    TxNotIncludedInNextBlocks,
}
