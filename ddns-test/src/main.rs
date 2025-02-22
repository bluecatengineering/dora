use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, UdpSocket};

/// @code
///     {
///      "change-type" : <integer>,
///      "forward-change" : <boolean>,
///      "reverse-change" : <boolean>,
///      "fqdn" : "<fqdn>",
///      "ip-address" : "<address>",
///      "dhcid" : "<hex_string>",
///      "lease-expires-on" : "<yyyymmddHHMMSS>",
///      "lease-length" : <secs>,
///      "use-conflict-resolution": <boolean>
///     }
/// @endcode
///      - change-type - indicates whether this request is to add or update
///   DNS entries or to remove them.  The value is an integer and is
///   0 for add/update and 1 for remove.
/// - forward-change - indicates whether the forward (name to
///   address) DNS zone should be updated.  The value is a string
///   representing a boolean.  It is "true" if the zone should be updated
///   and "false" if not. (Unlike the keyword, the boolean value is
///   case-insensitive.)
/// - reverse-change - indicates whether the reverse (address to
///   name) DNS zone should be updated.  The value is a string
///   representing a boolean.  It is "true" if the zone should be updated
///   and "false" if not. (Unlike the keyword, the boolean value is
///   case-insensitive.)
/// - fqdn - fully qualified domain name such as "myhost.example.com.".
///   (Note that a trailing dot will be appended if not supplied.)
/// - ip-address - the IPv4 or IPv6 address of the client.  The value
///   is a string representing the IP address (e.g. "192.168.0.1" or
///   "2001:db8:1::2").
/// - dhcid - identification of the DHCP client to whom the IP address has
///   been leased.  The value is a string containing an even number of
///   hexadecimal digits without delimiters such as "2C010203040A7F8E3D"
///   (case insensitive).
/// - lease-expires-on - the date and time on which the lease expires.
///   The value is a string of the form "yyyymmddHHMMSS" where:
///     - yyyy - four digit year
///     - mm - month of year (1-12),
///     - dd - day of the month (1-31),
///     - HH - hour of the day (0-23)
///     - MM - minutes of the hour (0-59)
///     - SS - seconds of the minute (0-59)
/// - lease-length - the length of the lease in seconds.  This is an
///   integer and may range between 1 and 4294967295 (2^32 - 1) inclusive.
/// - use-conflict-resolution - when true, follow RFC 4703 which uses
///   DHCID records to prohibit multiple clients from updating an FQDN
///
fn main() -> Result<()> {
    let soc = UdpSocket::bind("0.0.0.0:0")?;
    let update = NcrUpdate {
        change_type: 0,
        forward_change: true,
        reverse_change: false,
        fqdn: "example.com.".to_owned(),
        ip_address: Ipv4Addr::from([192, 168, 2, 1]),
        dhcid: "0102030405060708".to_owned(),
        lease_expires_on: "20130121132405".to_owned(),
        lease_length: 1300,
        use_conflict_resolution: true,
    };
    let s = serde_json::to_string(&update)?;
    let len = s.len() as u16;
    println!("sending {s} {len}");
    // expects two-byte len prepended
    let mut buf = vec![];
    buf.extend(len.to_be_bytes());
    buf.extend(s.as_bytes());
    println!("{buf:#?}");
    let r = soc.send_to(&buf, "127.0.0.1:53001")?;
    println!("sent size {r}");
    let mut buf = vec![0; 1024];
    let (len, from) = soc.recv_from(&mut buf)?;
    // response has buf len prepended also
    let buf_len = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    println!("recvd len {len} from {from} buf_len {buf_len}");
    let decoded: serde_json::Value = serde_json::from_slice(&buf[2..buf_len])?;
    println!("response {decoded}");
    Ok(())
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct NcrUpdate {
    change_type: u32,
    forward_change: bool,
    reverse_change: bool,
    fqdn: String,
    ip_address: Ipv4Addr,
    dhcid: String,
    lease_expires_on: String,
    lease_length: u32,
    use_conflict_resolution: bool,
}
