//! # Client Classes

use anyhow::{Context, Result};
use client_classification::{ast, Expr};
use dora_core::dhcproto::v4;

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
                options: class.options.get(), // options
                                              //     .get()
                                              //     .into_iter()
                                              //     .map(|(k, v)| {
                                              //         Ok((k, {
                                              //             // using UnknownOption here so that the data section is easy to get
                                              //             let opt = v.to_vec()?;
                                              //             let mut d = Decoder::new(&opt);
                                              //             v4::UnknownOption::decode(&mut d)?
                                              //         }))
                                              //     })
                                              //     .collect::<Result<HashMap<_, _>>>()
                                              //     .context("failed to convert options in client_classes")?,
            });
        }
        Ok(Self { classes })
    }
}
