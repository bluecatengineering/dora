//! # Client Classes

use std::collections::HashMap;

use anyhow::{Context, Result};
use client_classification::{ast, Expr};
use dora_core::dhcproto::v4::{self, OptionCode, UnknownOption};
use tracing::error;

use crate::wire;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientClasses {
    pub(crate) classes: Vec<ClientClass>,
}

impl ClientClasses {
    pub fn find(&self, name: &str) -> Option<&ClientClass> {
        self.classes.iter().find(|class| class.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientClass {
    pub(crate) name: String,
    // TODO: client classes assertion won't work with sub-options right now
    pub(crate) assert: Expr,
    pub(crate) options: v4::DhcpOptions,
}

impl ClientClasses {
    pub fn from_wire(cfg: wire::client_classes::ClientClasses) -> Result<Self> {
        let mut classes = Vec::with_capacity(cfg.v4.capacity());
        for class in cfg.v4.into_iter() {
            let assert = ast::parse(&class.assert)
                .with_context(|| format!("failed to parse client class {}", class.name))?;
            classes.push(ClientClass {
                name: class.name,
                assert,
                options: class.options.get(),
            });
        }
        Ok(Self { classes })
    }
}

impl ClientClass {
    pub fn eval(self, chaddr: &str, unknown_opts: &HashMap<OptionCode, UnknownOption>) -> bool {
        match client_classification::ast::eval_ast(self.assert, chaddr, unknown_opts) {
            Ok(ast::Val::Bool(true)) => true,
            Ok(ast::Val::Bool(false)) => false,
            res => {
                error!(?res, class_name = ?self.name, "didn't evaluate to true/false");
                false
            }
        }
    }
}
