#![allow(clippy::too_many_arguments)]

use std::{net::Ipv4Addr, str::FromStr};

use config::v4::Ddns;
use dora_core::{
    dhcproto::{
        v4::{
            self,
            fqdn::{ClientFQDN, FqdnFlags},
            DhcpOption, OptionCode,
        },
        Name, NameError,
    },
    prelude::MsgContext,
    tracing::{debug, error, info},
};
use trust_dns_client::rr::dnssec::tsig::TSigner;

pub mod dhcid;
pub mod update;

use dhcid::DhcId;

use crate::update::Updater;

pub struct DdnsUpdateV4;

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
}

pub enum Action<'a> {
    DontUpdate(ClientFQDN),
    Update((ClientFQDN, bool, bool, &'a Ddns)),
}

impl DdnsUpdateV4 {
    pub async fn do_update(
        &self,
        ctx: &mut MsgContext<v4::Message>,
        duid: DhcId,
        cfg: Option<&Ddns>,
        leased: Ipv4Addr,
    ) -> Result<(), DdnsError> {
        match self.get_fqdn(ctx, cfg) {
            Ok(Action::Update((resp_fqdn, forward, reverse, cfg))) => {
                let domain = resp_fqdn.domain().clone();
                ctx.decoded_resp_msg_mut()
                    .map(|msg| msg.opts_mut().insert(DhcpOption::ClientFQDN(resp_fqdn)));
                self.send_ddns(ctx, cfg, duid, leased, domain, forward, reverse)
                    .await?;
            }
            Ok(Action::DontUpdate(mut resp_fqdn)) => {
                resp_fqdn.set_flags(resp_fqdn.flags().set_n(true));
                ctx.decoded_resp_msg_mut()
                    .map(|msg| msg.opts_mut().insert(DhcpOption::ClientFQDN(resp_fqdn)));
                return Ok(());
            }
            Err(err) => return Err(err),
        }
        Ok(())
    }
    pub fn get_fqdn<'a, 'b, 'c>(
        &'a self,
        ctx: &'b mut MsgContext<v4::Message>,
        cfg: Option<&'c Ddns>,
    ) -> Result<Action<'c>, DdnsError> {
        let req = ctx.decoded_msg();
        let fqdn = req.opts().get(OptionCode::ClientFQDN);
        let hostname = req.opts().get(OptionCode::Hostname);
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
                if domain.is_empty() {
                    error!(?domain, "client FQDN domain was empty. No update performed");
                    return Ok(Action::DontUpdate(resp_fqdn));
                }
                let Some(ddns_config) = cfg else {
                    info!("got client FQDN but no DDNS config is present. No update performed");
                    return Ok(Action::DontUpdate(resp_fqdn));
                };

                let Some((resp_flags, forward, reverse)) = handle_flags(fqdn.flags(), ddns_config, resp_flags) else {
                    error!(flags = ?fqdn.flags(), "got impossible client flag combination");
                    return Err(DdnsError::FlagConfig(fqdn.flags()))
                };
                resp_fqdn.set_flags(resp_flags);
                // TODO: allow modifying fqdn
                // if let Some(replace_name) = ddns_config.replace_client_name() {
                // }
                Ok(Action::Update((resp_fqdn, forward, reverse, ddns_config)))
            }
            (_, Some(DhcpOption::Hostname(hostname))) => {
                debug!(?fqdn, ?hostname, "received hostname but no FQDN option");

                let resp_flags = FqdnFlags::default().set_e(true);
                // TODO: Not sure if this empty Name is valid
                let mut resp_fqdn = ClientFQDN::new(resp_flags, Name::new());
                let Some(ddns_config) = cfg else {
                    info!("got hostname but no DDNS config is present. No update performed"); 
                    return Ok(Action::DontUpdate(resp_fqdn));
                };
                if !ddns_config.enable_updates() {
                    info!("got hostname but DDNS updates are disabled. No update performed");
                    return Ok(Action::DontUpdate(resp_fqdn));
                }
                if let Some(suffix) = &ddns_config.hostname_suffix {
                    let Ok(suffix) = Name::from_str(suffix) else {
                        error!(?suffix, "failed to parse hostname_suffix. No update performed");
                        return Ok(Action::DontUpdate(resp_fqdn));
                    };
                    // append the suffix
                    let new_domain = Name::from_str(hostname)?.append_name(&suffix)?;
                    resp_fqdn.set_domain(new_domain);
                    // set update to true
                    resp_fqdn.set_flags(resp_fqdn.flags().set_s(true));
                    Ok(Action::Update((resp_fqdn, true, true, ddns_config)))
                } else {
                    error!("No DDNS name configured. No update performed");
                    resp_fqdn.set_flags(resp_fqdn.flags().set_n(true));
                    Ok(Action::DontUpdate(resp_fqdn))
                }
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

    async fn send_ddns(
        &self,
        ctx: &mut MsgContext<v4::Message>,
        config: &Ddns,
        duid: DhcId,
        leased: Ipv4Addr,
        domain: Name,
        forward: bool,
        reverse: bool,
    ) -> Result<(), DdnsError> {
        let Some(DhcpOption::AddressLeaseTime(lease_length)) = ctx.decoded_msg().opts().get(OptionCode::AddressLeaseTime) else {
            error!("address lease time not available for DDNS update");
            return Err(DdnsError::SendFailed)
        };
        if forward {
            for srv in config.forward() {
                let tsig = if let Some(key_name) = &srv.key {
                    let tsig = match tsigner(key_name, config) {
                        Err(err) => {
                            error!(?err, "failed to create tsigner");
                            continue;
                        }
                        Ok(t) => t,
                    };
                    Some(tsig)
                } else {
                    None
                };
                // todo: likely re-creating the same client for each update
                // should cache this in parent type
                let mut client = Updater::new(srv.ip, tsig).await?;

                // todo: zone origin same as domain?
                match client
                    .forward(
                        domain.clone(),
                        domain.clone(),
                        duid.clone(),
                        leased,
                        *lease_length,
                    )
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
        if reverse {
            for srv in config.reverse() {
                let tsig = if let Some(key_name) = &srv.key {
                    let tsig = match tsigner(key_name, config) {
                        Err(err) => {
                            error!(?err, "failed to create tsigner");
                            continue;
                        }
                        Ok(t) => t,
                    };
                    Some(tsig)
                } else {
                    None
                };
                // todo: likely re-creating the same client for each update
                // should cache this in parent type
                let mut client = Updater::new(srv.ip, tsig).await?;

                // todo: zone origin same as domain?
                match client
                    .reverse(domain.clone(), duid.clone(), leased, *lease_length)
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
        // self.dns.
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TsigError {
    #[error("key not found {key_name:?}")]
    KeyNotFound { key_name: String },
    #[error("failed to create TSigner {0:?}")]
    TSignerFailed(#[from] NameError),
}

pub fn tsigner(key_name: &str, config: &Ddns) -> Result<TSigner, TsigError> {
    // get the key data from the tsig hashmap
    let Some(key) = config.key(key_name) else {
        return Err(TsigError::KeyNotFound { key_name: key_name.to_owned() });
    };
    // create new tsigner
    Ok(TSigner::new(
        key.data.as_bytes().to_owned(),
        key.algorithm.into(),
        Name::from_ascii(key_name).unwrap(), // TODO: remove unwrap
        // ??
        300,
    )?)
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
                debug!("got client FQDN but DDNS config set to allow client update. No update performed");
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
    // N 0 S 1
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
            FqdnFlags::default().set_s(false).set_n(false),
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
