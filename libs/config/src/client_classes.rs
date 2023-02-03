//! # Client Classes

use client_classification::Expr;
use dora_core::dhcproto::v4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientClass {
    pub(crate) name: String,
    pub(crate) assert: Expr,
    pub(crate) options: v4::DhcpOptions,
}
