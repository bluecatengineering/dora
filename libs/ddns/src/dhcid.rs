use dora_core::dhcproto::{v4::HType, Name, NameError};
use ring::digest::{Context, SHA256};
use trust_dns_client::serialize::binary::BinEncoder;

#[derive(Debug, PartialEq, Eq)]
pub struct DhcId {
    ty: IdType,
    id: Vec<u8>,
}

impl DhcId {
    /// if created with type Chaddr, only significant bytes (up to hlen) should be provided
    pub fn new<T: Into<Vec<u8>>>(ty: IdType, id: T) -> Self {
        Self { ty, id: id.into() }
    }
    pub fn id(&self) -> Vec<u8> {
        if self.ty == IdType::Chaddr {
            // https://www.rfc-editor.org/rfc/rfc4701#section-3.5.3
            let mut d = vec![0; self.id.len() + 1];
            d[0] = HType::Eth.into();
            d[1..].copy_from_slice(&self.id);
            d
        } else {
            self.id.clone()
        }
    }
    /// The DHCID RDATA has the following structure:
    ///
    ///    < identifier-type > < digest-type > < digest >
    ///
    /// identifier-type:
    ///      chaddr      0x0000
    ///      client id   0x0001
    ///      duid        0x0002
    /// the digest-type code is 0x01 for SHA256
    ///    The input to the digest hash function is defined to be:
    ///        digest = SHA-256(< identifier > < FQDN >)
    pub fn rdata(&self, fqdn: &Name) -> Result<Vec<u8>, NameError> {
        let mut cx = Context::new(&SHA256);
        // create new encoder
        let mut name_buf = Vec::new();
        let mut enc = BinEncoder::new(&mut name_buf);
        fqdn.emit_as_canonical(&mut enc, true)?;
        // create digest
        let mut data = self.id();

        data.extend_from_slice(&name_buf);
        cx.update(&data);
        let digest = cx.finish();

        let mut buf: Vec<u8> = vec![0; 3 + digest.as_ref().len()];
        buf[0] = 0x00;
        match self.ty {
            IdType::Chaddr => {
                buf[1] = 0x00;
            }
            IdType::ClientId => {
                buf[1] = 0x01;
            }
            IdType::Duid => {
                buf[1] = 0x02;
            }
        }
        buf[2] = 0x01;
        buf[3..].copy_from_slice(digest.as_ref());

        Ok(buf)
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum IdType {
    Chaddr = 0x0000,
    ClientId = 0x0001,
    Duid = 0x0002,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn test_dhcid_client_id() {
        let dhcid = DhcId::new(IdType::ClientId, hex::decode("010708090a0b0c").unwrap());
        let out = dhcid
            .rdata(&Name::from_str("chi.example.com.").unwrap())
            .unwrap();
        assert_eq!(
            base64::encode(out),
            "AAEBOSD+XR3Os/0LozeXVqcNc7FwCfQdWL3b/NaiUDlW2No=".to_owned()
        );
    }

    #[test]
    fn test_dhcid_chaddr() {
        let dhcid = DhcId::new(IdType::Chaddr, hex::decode("010203040506").unwrap());
        let out = dhcid
            .rdata(&Name::from_str("client.example.com.").unwrap())
            .unwrap();
        assert_eq!(
            base64::encode(out),
            "AAABxLmlskllE0MVjd57zHcWmEH3pCQ6VytcKD//7es/deY=".to_owned()
        );
    }
}
