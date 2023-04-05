//! # Client Classes

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use client_classification::{ast, Args, Expr, Val};
use dora_core::dhcproto::{
    self,
    v4::{self, OptionCode, UnknownOption},
    Decodable, Decoder, Encodable,
};
use topo_sort::DependencyTree;
use tracing::{error, trace};

use crate::wire;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientClasses {
    /// list of classes, order is topologically sorted based on use of `member` dependencies in the expression
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

impl TryFrom<wire::client_classes::ClientClasses> for ClientClasses {
    type Error = anyhow::Error;

    fn try_from(
        cfg: wire::client_classes::ClientClasses,
    ) -> std::result::Result<Self, Self::Error> {
        let mut dep_tree = DependencyTree::new();
        for class in cfg.v4.into_iter() {
            let assert = ast::parse(&class.assert)
                .with_context(|| format!("failed to parse client class {}", class.name))?;
            let deps = client_classification::get_class_dependencies(&assert);
            let name = class.name.clone();
            let class = ClientClass {
                name: class.name,
                assert,
                options: class.options.get(),
            };
            dep_tree.add(name, class, deps);
        }

        Ok(Self {
            classes: dep_tree.topological_sort()?,
        })
    }
}

impl ClientClasses {
    /// evaluate all client classes, returning a list of classes that match
    pub fn eval(&self, req: &dhcproto::v4::Message) -> Result<Vec<String>> {
        let (chaddr, opts) = to_unknown_opts(req)?;
        let mut args = Args {
            chaddr,
            deps: HashSet::new(),
            msg: req,
            opts,
        };
        for class in &self.classes {
            if class.eval(&args) {
                args.deps.insert(class.name.to_owned());
            }
        }

        Ok(args.deps.into_iter().collect())
    }
    /// take matched client classes, return merge DhcpOptions that contains all classes options merged
    /// together with precedence given based on class position in client_classes list (lower index == higher precedence)
    pub fn collect_opts(&self, matched_classes: Option<&[String]>) -> Option<v4::DhcpOptions> {
        self.classes
            .iter()
            .filter(|class| {
                matched_classes
                    .map(|m| m.contains(&class.name))
                    .unwrap_or(false)
            })
            .fold(None, |ret, class| merge_opts(&class.options, ret))
    }
}

impl ClientClass {
    pub fn eval(&self, args: &Args) -> bool {
        trace!(expr = ?self.assert, chaddr = ?args.chaddr, "evaluating expression");
        match client_classification::eval(&self.assert, args) {
            Ok(Val::Bool(true)) => true,
            Ok(Val::Bool(false)) => false,
            res => {
                error!(?res, class_name = ?self.name, "didn't evaluate to true/false");
                false
            }
        }
    }
}

fn to_unknown_opts(
    req: &dhcproto::v4::Message,
) -> Result<(String, HashMap<OptionCode, UnknownOption>)> {
    // TODO: find a better way to do this so we don't have to convert to Unknown on every eval
    // possibly, add better methods to dhcproto so we can pull the data section out?
    Ok((
        hex::encode(req.chaddr()),
        req.opts()
            .iter()
            .map(|(k, v)| {
                Ok((*k, {
                    // using UnknownOption here so that the data section is easy to get
                    let opt = v.to_vec()?;
                    let mut d = Decoder::new(&opt);
                    UnknownOption::decode(&mut d)?
                }))
            })
            .collect::<Result<HashMap<_, _>>>()
            .context("failed to convert options in client_classes")?,
    ))
}

/// merge `b` into `a`, favoring `b` where there are duplicates
fn merge_opts(a: &v4::DhcpOptions, b: Option<v4::DhcpOptions>) -> Option<v4::DhcpOptions> {
    match b {
        Some(mut b) => {
            for (code, opt) in a.iter() {
                if b.get(*code).is_none() {
                    b.insert(opt.clone());
                }
            }
            Some(b)
        }
        None => Some(a.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_opts() {
        let classes = ClientClasses {
            classes: vec![
                ClientClass {
                    name: "foo".to_owned(),
                    assert: client_classification::Expr::Bool(true),
                    options: {
                        let mut opts = v4::DhcpOptions::new();
                        opts.insert(v4::DhcpOption::Router(vec![[8, 8, 8, 8].into()]));
                        opts.insert(v4::DhcpOption::AddressLeaseTime(10));
                        opts
                    },
                },
                ClientClass {
                    name: "bar".to_owned(),
                    assert: client_classification::Expr::Bool(true),
                    options: {
                        let mut opts = v4::DhcpOptions::new();
                        opts.insert(v4::DhcpOption::Router(vec![[1, 1, 1, 1].into()]));
                        opts.insert(v4::DhcpOption::SubnetMask([1, 1, 1, 1].into()));
                        opts.insert(v4::DhcpOption::TimeOffset(50));
                        opts
                    },
                },
                ClientClass {
                    name: "baz".to_owned(),
                    assert: client_classification::Expr::Bool(true),
                    options: {
                        let mut opts = v4::DhcpOptions::new();
                        opts.insert(v4::DhcpOption::ServerIdentifier([1, 1, 1, 1].into()));
                        opts.insert(v4::DhcpOption::ArpCacheTimeout(1));
                        opts
                    },
                },
            ],
        };
        let opts = classes.collect_opts(Some(&["foo".to_owned(), "bar".to_owned()]));
        // includes opts from "foo" and "bar", favouring "foo" for duplicates because it shows up earlier in the `client_classes` list
        assert_eq!(opts.unwrap(), {
            let mut opts = v4::DhcpOptions::new();
            opts.insert(v4::DhcpOption::Router(vec![[8, 8, 8, 8].into()]));
            opts.insert(v4::DhcpOption::AddressLeaseTime(10));
            opts.insert(v4::DhcpOption::SubnetMask([1, 1, 1, 1].into()));
            opts.insert(v4::DhcpOption::TimeOffset(50));
            opts
        });
    }
}
