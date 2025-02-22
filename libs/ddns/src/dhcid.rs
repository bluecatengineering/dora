use dora_core::dhcproto::{Name, NameError, v4::HType};
use ring::digest::{Context, SHA256};
use trust_dns_client::serialize::binary::BinEncoder;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DhcId {
    ty: IdType,
    id: Vec<u8>,
}

impl DhcId {
    /// if created with type Chaddr, only significant bytes (up to hlen) should be provided
    pub fn new<T: Into<Vec<u8>>>(ty: IdType, id: T) -> Self {
        Self { ty, id: id.into() }
    }
    pub fn chaddr<T: Into<Vec<u8>>>(id: T) -> Self {
        Self {
            ty: IdType::Chaddr,
            id: id.into(),
        }
    }
    pub fn client_id<T: Into<Vec<u8>>>(id: T) -> Self {
        Self {
            ty: IdType::ClientId,
            id: id.into(),
        }
    }
    pub fn duid<T: Into<Vec<u8>>>(id: T) -> Self {
        Self {
            ty: IdType::Duid,
            id: id.into(),
        }
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

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
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

    //      A DHCP server allocates the IPv4 address 192.0.2.2 to a client that
    //    included the DHCP client-identifier option data 01:07:08:09:0a:0b:0c
    //    in its DHCP request.  The server updates the name "chi.example.com"
    //    on the client's behalf and uses the DHCP client identifier option
    //    data as input in forming a DHCID RR.  The DHCID RDATA is formed by
    //    setting the two type octets to the value 0x0001, the 1-octet digest
    //    type to 1 for SHA-256, and performing a SHA-256 hash computation
    //    across a buffer containing the seven octets from the client-id option
    //    and the FQDN (represented as specified in Section 3.5).

    //      chi.example.com.      A       192.0.2.2
    //      chi.example.com.      DHCID   ( AAEBOSD+XR3Os/0LozeXVqcNc7FwCfQdW
    //                                      L3b/NaiUDlW2No= )
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

    //    A DHCP server allocating the IPv4 address 192.0.2.3 to a client with
    //    the Ethernet MAC address 01:02:03:04:05:06 using domain name
    //    "client.example.com" uses the client's link-layer address to identify
    //    the client.  The DHCID RDATA is composed by setting the two type
    //    octets to zero, the 1-octet digest type to 1 for SHA-256, and
    //    performing an SHA-256 hash computation across a buffer containing the
    //    1-octet 'htype' value for Ethernet, 0x01, followed by the six octets
    //    of the Ethernet MAC address, and the domain name (represented as
    //    specified in Section 3.5).

    //      client.example.com.   A       192.0.2.3
    //      client.example.com.   DHCID   ( AAABxLmlskllE0MVjd57zHcWmEH3pCQ6V
    //                                      ytcKD//7es/deY= )
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
