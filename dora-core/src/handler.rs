//! Plugins can register to various points in the request lifecycle
//! by implementing one of these traits.
use anyhow::Result;
use async_trait::async_trait;

pub(crate) use crate::server::{context::MsgContext, state::State};

/// Action for dora to take after the plugin returns
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum Action {
    /// Respond with `decoded_resp_msg` from `MsgContext`
    Respond,
    /// Don't respond
    NoResponse,
    /// Continue executing the next plugin
    Continue,
}

/// define a plugin which will mutate a MsgContext<T> where T is the Message type
#[async_trait]
pub trait Plugin<T>: Send + Sync + 'static {
    /// what to execute during this step in the message lifecycle
    ///
    /// CANCEL-SAFETY: everything in handle must be cancel-safe. A top-level timeout can possibly kill this
    /// method
    async fn handle(&self, ctx: &mut MsgContext<T>) -> Result<Action>;
}

/// A handler that is run after the response is returned. This moves the
/// `MsgContext` instead of borrowing it, and as such only one such handler can
/// be added.
#[async_trait]
pub trait PostResponse<T>: Send + Sync + 'static {
    /// what to execute during this step in the message lifecycle
    async fn handle(&self, ctx: MsgContext<T>);
}
