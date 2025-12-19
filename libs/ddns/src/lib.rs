#![allow(clippy::too_many_arguments)]

use std::time::Duration;
use std::{net::Ipv4Addr, str::FromStr};

use base64::{Engine, prelude::BASE64_STANDARD};
use config::v4::{Ddns, NetRange};
use dora_core::tracing;
use dora_core::{
    dhcproto::{
        Name, NameError,
        v4::{
            self, DhcpOption, OptionCode,
            fqdn::{ClientFQDN, FqdnFlags},
        },
    },
    hickory_proto::{dnssec::DnsSecError, dnssec::tsig::TSigner},
    prelude::MsgContext,
    tracing::{debug, error, info, trace, warn},
};

pub mod dhcid;
pub mod update;

use crate::dhcid::DhcId;
use crate::update::Updater;

#[derive(Debug, Default)]
pub struct DdnsUpdate;

#[derive(thiserror::Error, Debug)]
pub enum DdnsError {
    #[error("client flag config: {0:?}")]
    FlagConfig(FqdnFlags),
    #[error("no update")]
    NoUpdate,
    #[error("send update failed")]
    SendFailed,
    #[error("error manipulating domain name {0:?}")]
    DomainError(#[from] NameError),
    #[error("update failed {0:?}")]
    UpdateError(#[from] crate::update::UpdateError),
    #[error("tsig error {0:?}")]
    TsigError(#[from] TsigError),
}

pub enum Action<'a> {
    DontUpdateFQDN(ClientFQDN),
    UpdateFQDN((ClientFQDN, bool, bool, &'a Ddns)),
    UpdateHostname((Name, bool, bool, &'a Ddns)),
}

impl DdnsUpdate {
    pub fn new() -> Self {
        Self
    }
    pub async fn update(
        &self,
        ctx: &mut MsgContext<v4::Message>,
        duid: DhcId,
        cfg: Option<&Ddns>,
        server_opts: &NetRange,
        leased: Ipv4Addr,
        lease_length: Duration,
    ) -> Result<(), DdnsError> {
        let Some(cfg) = cfg else {
            debug!("no DDNS config is present. No update performed");
            if let Some(DhcpOption::ClientFQDN(fqdn)) = ctx.msg().opts().get(OptionCode::ClientFQDN)
            {
                let domain = fqdn.domain().clone();
                let resp_flags = FqdnFlags::default().set_e(fqdn.flags().e()).set_n(true);
                ctx.resp_msg_mut().map(|msg| {
                    msg.opts_mut()
                        .insert(DhcpOption::ClientFQDN(ClientFQDN::new(resp_flags, domain)))
                });
            }
            return Ok(());
        };
        let lease_length = lease_length.as_secs() as u32;
        match self.get_fqdn(ctx, cfg, server_opts) {
            Ok(Action::UpdateFQDN((resp_fqdn, forward, reverse, cfg))) => {
                let domain = resp_fqdn.domain().clone();
                ctx.resp_msg_mut()
                    .map(|msg| msg.opts_mut().insert(DhcpOption::ClientFQDN(resp_fqdn)));
                self.send_dns(cfg, duid, leased, lease_length, domain, forward, reverse)
                    .await?;
            }
            Ok(Action::UpdateHostname((domain, forward, reverse, cfg))) => {
                self.send_dns(cfg, duid, leased, lease_length, domain, forward, reverse)
                    .await?;
            }
            Ok(Action::DontUpdateFQDN(mut resp_fqdn)) => {
                resp_fqdn.set_flags(resp_fqdn.flags().set_n(true));
                ctx.resp_msg_mut()
                    .map(|msg| msg.opts_mut().insert(DhcpOption::ClientFQDN(resp_fqdn)));
                return Err(DdnsError::NoUpdate);
            }
            Err(err) => return Err(err),
        }
        Ok(())
    }
    pub fn get_fqdn<'a>(
        &self,
        ctx: &mut MsgContext<v4::Message>,
        cfg: &'a Ddns,
        server_opts: &NetRange,
    ) -> Result<Action<'a>, DdnsError> {
        let req = ctx.msg();
        let fqdn = req.opts().get(OptionCode::ClientFQDN);
        let hostname = req.opts().get(OptionCode::Hostname);
        // will process fqdn first if available, if not then hostname
        match (fqdn, hostname) {
            (Some(DhcpOption::ClientFQDN(fqdn)), _) => {
                debug!(
                    ?fqdn,
                    ?hostname,
                    "FQDN option received, using it for ddns update. Ignoring any hostname."
                );
                let domain = fqdn.domain();
                let resp_flags = FqdnFlags::default().set_e(fqdn.flags().e());
                // RFC 4702 says the 2 1-byte RCODE flags should be set to 255
                let mut resp_fqdn = ClientFQDN::new(resp_flags, domain.clone());
                if !cfg.enable_updates() {
                    info!("got client FQDN but DDNS updates are disabled. No update performed");
                    return Ok(Action::DontUpdateFQDN(resp_fqdn));
                }
                if domain.is_empty() {
                    error!(?domain, "client FQDN domain was empty. No update performed");
                    return Ok(Action::DontUpdateFQDN(resp_fqdn));
                }
                let Some((resp_flags, forward, reverse)) =
                    handle_flags(fqdn.flags(), cfg, resp_flags)
                else {
                    error!(flags = ?fqdn.flags(), "got impossible client flag combination");
                    return Err(DdnsError::FlagConfig(fqdn.flags()));
                };
                resp_fqdn.set_flags(resp_flags);
                Ok(Action::UpdateFQDN((resp_fqdn, forward, reverse, cfg)))
            }
            (_, Some(DhcpOption::Hostname(hostname))) => {
                debug!(?fqdn, ?hostname, "received hostname but no FQDN option");
                if !cfg.enable_updates() {
                    info!("got hostname but DDNS updates are disabled. No update performed");
                    return Err(DdnsError::NoUpdate);
                }
                let Some(DhcpOption::DomainName(domain)) =
                    server_opts.opts().get(OptionCode::DomainName)
                else {
                    error!(
                        ?hostname,
                        "got hostname option but no domain name found, no update"
                    );
                    return Err(DdnsError::NoUpdate);
                };
                // got hostname & domain name config from server, combining with opt 15 to create FQDN
                let hostname = hostname.to_string() + "." + domain;
                let resp_hostname = Name::from_str(&hostname)?;
                Ok(Action::UpdateHostname((resp_hostname, true, true, cfg)))
            }
            (_, _) => {
                debug!(
                    ?fqdn,
                    ?hostname,
                    "Neither hostname or FQDN received, no DDNS update"
                );
                Err(DdnsError::NoUpdate)
            }
        }
    }

    #[tracing::instrument(skip_all, fields(domain = %domain, duid = ?duid, leased_ip = %leased))]
    async fn send_dns(
        &self,
        cfg: &Ddns,
        duid: DhcId,
        leased: Ipv4Addr,
        lease_length: u32,
        domain: Name,
        forward: bool,
        reverse: bool,
    ) -> Result<(), DdnsError> {
        if forward && let Some(srv) = cfg.match_longest_forward(&domain) {
            let tsig = if let Some(key_name) = &srv.key {
                trace!(?key_name, "using signing key");
                Some(tsigner(key_name, cfg)?)
            } else {
                warn!("no signing key found for domain");
                None
            };
            let zone = srv.name.clone();
            // todo: likely re-creating the same client for each update
            // should cache this in parent type
            let mut client = Updater::new(srv.ip, tsig).await?;

            // todo: zone origin same as domain?
            match client
                .forward(zone, domain.clone(), duid.clone(), leased, lease_length)
                .await
            {
                Ok(_) => {
                    debug!("updated DNS");
                }
                Err(err) => {
                    error!(?err, "failed to update DNS");
                }
            }
        }
        if reverse {
            let rev_ip = crate::update::reverse_ip(leased);
            let arpa_name = Name::from_str(&rev_ip).unwrap();
            if let Some(srv) = cfg.match_longest_reverse(&arpa_name) {
                let tsig = if let Some(key_name) = &srv.key {
                    Some(tsigner(key_name, cfg)?)
                } else {
                    None
                };
                let zone = srv.name.clone();
                // todo: should cache this in parent type
                let mut client = Updater::new(srv.ip, tsig).await?;

                match client
                    .reverse(zone, domain.clone(), duid.clone(), leased, lease_length)
                    .await
                {
                    Ok(_) => {
                        info!(?domain, "successfully updated DNS");
                    }
                    Err(err) => {
                        error!(?err, ?domain, "failed to update DNS");
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TsigError {
    #[error("key not found {key_name:?}")]
    KeyNotFound { key_name: String },
    #[error("key not base64 {0:?}")]
    KeyNotBase64(#[from] base64::DecodeError),
    #[error("failed to create TSigner {0:?}")]
    TSignerFailed(#[from] DnsSecError),
}

pub fn tsigner(key_name: &str, config: &Ddns) -> Result<TSigner, TsigError> {
    // get the key data from the tsig hashmap
    let Some(key) = config.key(key_name) else {
        return Err(TsigError::KeyNotFound {
            key_name: key_name.to_owned(),
        });
    };
    let key_bin = BASE64_STANDARD
        .decode(key.data.as_bytes())
        .map_err(TsigError::KeyNotBase64)?;

    // create new tsigner
    let signer = TSigner::new(
        key_bin,
        key.algorithm.clone(),
        Name::from_ascii(key_name).unwrap(), // TODO: remove unwrap
        // ??
        300,
    )
    .map_err(TsigError::TSignerFailed)?;
    Ok(signer)
}

fn handle_flags(
    client_flags: FqdnFlags,
    cfg: &Ddns,
    server_flags: FqdnFlags,
) -> Option<(FqdnFlags, bool, bool)> {
    let n = client_flags.n();
    let s = client_flags.s();
    // Per RFC 4702 & 4704, the client N and S flags allow the client to
    // request one of three options:
    //
    //  N flag  S flag   Option
    // ------------------------------------------------------------------
    //    0       0      client wants to do forward updates (section 3.2)
    //    0       1      client wants server to do forward updates (section 3.3)
    //    1       0      client wants no one to do updates (section 3.4)
    //    1       1      invalid combination
    // (Note section numbers cited are for 4702, for 4704 see 5.1, 5.2, and 5.3)

    let flags = match (n, s) {
        (false, false) => {
            Some(if !cfg.enable_updates() {
                debug!(
                    "got client FQDN but DDNS config set to allow client update. No update performed"
                );
                server_flags.set_s(false).set_n(true)
            } else {
                // override client updates
                server_flags
                    .set_s(cfg.override_client_updates())
                    .set_n(false)
            })
        }
        (false, true) => Some(
            server_flags
                .set_s(cfg.enable_updates())
                .set_n(!cfg.enable_updates()),
        ),
        (true, false) => {
            let s = cfg.enable_updates() && cfg.override_no_updates();
            if s {
                debug!("DDNS updates enabled and overriding FQDN No update flag");
            }
            Some(server_flags.set_s(s).set_n(!s))
        }
        // invalid combination S/N can't both be true
        (true, true) => None,
    };
    let forward = flags?.s();
    let reverse = !flags?.n();
    // set the override flag if server S is different from client
    Some((flags?.set_o(flags?.s() != s), forward, reverse))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness(
        cli: FqdnFlags,
        (enable, override_client, override_no_update): (bool, bool, bool),
        expected_server: FqdnFlags,
        expected_forward: bool,
        expected_reverse: bool,
    ) {
        let cfg = Ddns {
            enable_updates: enable,
            override_client_updates: override_client,
            override_no_updates: override_no_update,
            ..Default::default()
        };
        let server = FqdnFlags::default();
        let (server, forward, reverse) = handle_flags(cli, &cfg, server).unwrap();
        assert_eq!(server, expected_server);
        assert_eq!(forward, expected_forward, "forward");
        assert_eq!(reverse, expected_reverse, "reverse");
    }
    // N 0 S 0
    #[test]
    fn test_flags_first_case() {
        // test the client wants to do forward updates
        harness(
            FqdnFlags::default(),
            (false, false, false),
            FqdnFlags::default().set_s(false).set_n(true),
            false,
            false,
        );
        harness(
            FqdnFlags::default(),
            (true, false, false),
            FqdnFlags::default(),
            false,
            true,
        );
        // override
        harness(
            FqdnFlags::default(),
            (true, true, false),
            FqdnFlags::default().set_s(true).set_n(false).set_o(true),
            true,
            true,
        );
        harness(
            FqdnFlags::default(),
            (true, false, true),
            FqdnFlags::default(),
            false,
            true,
        );
    }
    // N 0 S 1
    #[test]
    fn test_flags_snd_case() {
        // test the client wants server to do updates
        harness(
            FqdnFlags::default().set_s(true),
            (false, false, false),
            FqdnFlags::default().set_s(false).set_n(true).set_o(true),
            false,
            false,
        );
        harness(
            FqdnFlags::default().set_s(true),
            (true, false, false),
            FqdnFlags::default().set_s(true).set_n(false),
            true,
            true,
        );
        // causes no change
        harness(
            FqdnFlags::default().set_s(true),
            (true, true, true), // if override is enabled
            FqdnFlags::default().set_s(true).set_n(false),
            true,
            true,
        );
    }
    // N 1 S 0
    #[test]
    fn test_flags_third_case() {
        // test the client wants nobody to update
        harness(
            FqdnFlags::default().set_n(true),
            (false, false, false),
            FqdnFlags::default().set_s(false).set_n(true),
            false,
            false,
        );
        // override no update
        harness(
            FqdnFlags::default().set_n(true),
            (true, false, true),
            FqdnFlags::default().set_s(true).set_n(false).set_o(true),
            true,
            true,
        );
        // other options have no effect
        harness(
            FqdnFlags::default().set_n(true),
            (true, true, true),
            FqdnFlags::default().set_s(true).set_n(false).set_o(true),
            true,
            true,
        );
    }
}
