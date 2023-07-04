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
use tracing::{error, trace, warn};

use crate::wire;
pub use client_classification;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientClasses {
    /// list of classes, order is topologically sorted based on use of `member` dependencies in the expression
    pub(crate) classes: HashMap<String, ClientClass>,
    pub(crate) original_order: Vec<String>,
    pub(crate) topo_order: Vec<String>,
}

impl ClientClasses {
    pub fn find(&self, name: &str) -> Option<&ClientClass> {
        self.classes.get(name)
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

    fn try_from(cfg: wire::client_classes::ClientClasses) -> Result<Self, Self::Error> {
        // save original order for option precedence
        let original_order = cfg.v4.iter().map(|c| c.name.clone()).collect();
        let mut dep_tree = DependencyTree::new();
        let mut classes = HashMap::new();
        for class in cfg.v4.into_iter() {
            let assert = ast::parse(&class.assert)
                .with_context(|| format!("failed to parse client class {}", class.name))?;
            let deps = client_classification::get_class_dependencies(&assert);
            let name = class.name.clone();
            dep_tree.add(name.clone(), name, deps);
            classes.insert(
                class.name.clone(),
                ClientClass {
                    name: class.name,
                    assert,
                    options: class.options.get(),
                },
            );
        }

        Ok(Self {
            classes,
            original_order,
            topo_order: dep_tree.topological_sort()?,
        })
    }
}

impl ClientClasses {
    /// evaluate all client classes, returning a list of classes that match
    pub fn eval(&self, req: &dhcproto::v4::Message, bootp_enabled: bool) -> Result<Vec<String>> {
        let (chaddr, opts) = to_unknown_opts(req)?;
        let vendor_builtin = client_classification::create_builtin_vendor(req);
        // if msg-type is not Discover/Offer/Request/Inform/etc then the msg is BOOTP
        let is_bootp = bootp_enabled
            && req.opts().msg_type().is_none()
            && req.opcode() == v4::Opcode::BootRequest;

        if let Err(err) = vendor_builtin {
            // log error but don't stop evaluation
            warn!(
                ?err,
                "error converting opt 60 (vendor class) to string for VENDOR_CLASS_"
            );
        }
        let mut args = Args {
            chaddr,
            member: {
                // all packets are member of "ALL"
                let mut set = HashSet::new();
                set.insert(client_classification::ALL_CLASS.to_owned());
                // add "VENDOR_CLASS_*" built-in
                if let Ok(Some(vendor)) = vendor_builtin {
                    set.insert(vendor);
                }
                // add "BOOTP"
                if is_bootp {
                    set.insert(client_classification::BOOTP_CLASS.to_owned());
                }
                set
            },
            msg: req,
            opts,
        };
        // eval all client classes in topological order
        for name in &self.topo_order {
            // this should never fail
            let class = self.classes.get(name).context("class not found")?;
            // eval class, passing args
            if class.eval(&args) {
                // add class name to dependencies set, for future evals
                // classes are always eval'd in topological order, so
                // future evals know what prior evals were
                args.member.insert(class.name.to_owned());
            }
        }

        Ok(args.member.into_iter().collect())
    }
    /// take matched client classes, return merge DhcpOptions that contains all classes options merged
    /// together with precedence given based on original position in client_classes list (lower index == higher precedence)
    pub fn collect_opts(&self, matched_classes: Option<&[String]>) -> Option<v4::DhcpOptions> {
        self.original_order
            .iter()
            .filter(|name| matched_classes.map(|m| m.contains(name)).unwrap_or(false))
            .fold(None, |ret, name| {
                let class = self.find(name)?;
                merge_opts(&class.options, ret)
            })
    }
}

impl ClientClass {
    pub fn eval(&self, args: &Args) -> bool {
        trace!(name = ?self.name, expr = ?self.assert, chaddr = ?args.chaddr, "evaluating expression");
        match client_classification::eval(&self.assert, args) {
            Ok(Val::Bool(true)) => true,
            Ok(Val::Bool(false)) => false,
            res => {
                error!(name = ?self.name, ?res, "expression didn't evaluate to true/false");
                false
            }
        }
    }
}

fn to_unknown_opts(
    req: &dhcproto::v4::Message,
) -> Result<(&[u8], HashMap<OptionCode, UnknownOption>)> {
    // TODO: find a better way to do this so we don't have to convert to Unknown on every eval
    // possibly, add better methods to dhcproto so we can pull the data section out?
    Ok((
        req.chaddr(),
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
            original_order: ["foo", "bar", "baz"]
                .iter()
                .map(|&n| n.to_owned())
                .collect(),
            topo_order: ["bar", "baz", "foo"]
                .iter()
                .map(|&n| n.to_owned())
                .collect(),
            classes: [
                (
                    "foo".to_owned(),
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
                ),
                (
                    "bar".to_owned(),
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
                ),
                (
                    "baz".to_owned(),
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
                ),
            ]
            .into_iter()
            .collect(),
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
