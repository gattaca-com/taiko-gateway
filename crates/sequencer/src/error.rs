#[derive(Debug, thiserror::Error)]
pub enum SequencerError {
    #[error("rpc error: {0}")]
    Rpc(#[from] jsonrpsee::core::ClientError),

    #[error("soft block failed: code {0}, err: {1:?}")]
    SoftBlock(u16, String),

    #[error("status failed: code {0}, err: {1:?}")]
    Status(u16, String),

    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("serde_json error: {0}")]
    SerdeJson(#[from] serde_json::Error),
}

impl SequencerError {
    pub fn is_nil_block(&self) -> bool {
        matches!(self, SequencerError::Rpc(jsonrpsee::core::ClientError::Call(err)) if err.message().contains("block is nil"))
    }
}
