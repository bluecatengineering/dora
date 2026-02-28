use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientIdentity {
    pub client_index: usize,
    pub mac: [u8; 6],
    pub duid: Vec<u8>,
    pub iaid: u32,
}

impl ClientIdentity {
    pub fn mac_string(&self) -> String {
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.mac[0], self.mac[1], self.mac[2], self.mac[3], self.mac[4], self.mac[5]
        )
    }

    pub fn duid_hex(&self) -> String {
        bytes_to_hex(&self.duid)
    }
}

#[derive(Debug, Clone)]
pub struct IdentityGenerator {
    seed: u64,
}

impl IdentityGenerator {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    pub fn identity(&self, client_index: usize) -> ClientIdentity {
        let mac = mac_for(client_index as u64, self.seed);
        let iaid = iaid_for(client_index as u64, self.seed);

        // DUID-LL: type 3, hardware type 1 (ethernet), then MAC
        let mut duid = vec![0x00, 0x03, 0x00, 0x01];
        duid.extend_from_slice(&mac);

        ClientIdentity {
            client_index,
            mac,
            duid,
            iaid,
        }
    }
}

fn mac_for(index: u64, seed: u64) -> [u8; 6] {
    let mixed = index.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed.rotate_left(17);
    [
        0x02, // locally administered, unicast
        ((mixed >> 32) & 0xff) as u8,
        ((mixed >> 24) & 0xff) as u8,
        ((mixed >> 16) & 0xff) as u8,
        ((mixed >> 8) & 0xff) as u8,
        (mixed & 0xff) as u8,
    ]
}

fn iaid_for(index: u64, seed: u64) -> u32 {
    let value = (index as u32).wrapping_add(1) ^ (seed as u32).rotate_left(9);
    if value == 0 { 1 } else { value }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(hex_digit((b >> 4) & 0x0f));
        out.push(hex_digit(b & 0x0f));
    }
    out
}

const fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + (value - 10)) as char,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::IdentityGenerator;

    #[test]
    fn deterministic_for_same_seed() {
        let gen_a = IdentityGenerator::new(42);
        let gen_b = IdentityGenerator::new(42);

        let id_a = gen_a.identity(12);
        let id_b = gen_b.identity(12);

        assert_eq!(id_a.mac, id_b.mac);
        assert_eq!(id_a.duid, id_b.duid);
        assert_eq!(id_a.iaid, id_b.iaid);
    }

    #[test]
    fn unique_mac_for_first_thousand() {
        let generator = IdentityGenerator::new(7);
        let mut seen = HashSet::new();

        for i in 0..1000 {
            let id = generator.identity(i);
            assert!(seen.insert(id.mac), "duplicate mac for index {i}");
        }
    }

    #[test]
    fn unique_iaid_for_first_thousand() {
        let generator = IdentityGenerator::new(19);
        let mut seen = HashSet::new();

        for i in 0..1000 {
            let id = generator.identity(i);
            assert!(seen.insert(id.iaid), "duplicate iaid for index {i}");
        }
    }
}
