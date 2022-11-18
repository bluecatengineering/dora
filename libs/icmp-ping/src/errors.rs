use crate::Token;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("io error: {0:?}")]
    IoError(#[from] std::io::Error),
    #[error("timeout reached on seq_cnt: {seq_cnt:?} ident: {ident:?}")]
    Timeout { seq_cnt: u16, ident: u16 },
    #[error("recv error on seq_cnt: {seq_cnt:?} ident: {ident:?}")]
    RecvError {
        seq_cnt: u16,
        ident: u16,
        #[source]
        err: tokio::sync::oneshot::error::RecvError,
    },
    #[error("recieved mismatched reply for request: {seq_cnt:?} {payload:?}")]
    WrongReply { seq_cnt: u16, payload: Token },
}

pub type Result<T> = std::result::Result<T, Error>;
