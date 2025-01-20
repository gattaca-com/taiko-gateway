use jsonrpsee::types::{ErrorCode, ErrorObject, ErrorObjectOwned};

fn rpc_err_static(code: i32, msg: &'static str) -> ErrorObjectOwned {
    ErrorObject::borrowed(code, msg, None)
}
fn rpc_err_owned(code: i32, msg: String) -> ErrorObjectOwned {
    ErrorObject::owned(code, msg, None::<()>)
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("failed parsing raw transaction")]
    FailedParsing,
    #[error("{0}")]
    ClientError(#[from] jsonrpsee::core::ClientError),
}

impl From<RpcError> for ErrorObjectOwned {
    fn from(err: RpcError) -> Self {
        match err {
            RpcError::FailedParsing => {
                rpc_err_static(ErrorCode::ParseError.code(), "failed parsing raw transaction")
            }
            RpcError::ClientError(err) => {
                rpc_err_owned(ErrorCode::InternalError.code(), err.to_string())
            }
        }
    }
}
