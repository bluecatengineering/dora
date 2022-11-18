//! functions generated to interact with ioctl
//!
#![allow(missing_docs)]

use std::{io, net::Ipv4Addr, os::unix::prelude::AsRawFd};

use dhcproto::v4;
use socket2::SockRef;

/// calls ioctl(fd, SIOCSARP, arpreq) to set `arpreq` in ARP cache
///
/// # Safety
/// fd must be a valid v4 socket.
///
pub fn arp_set(
    soc: SockRef<'_>,
    yiaddr: Ipv4Addr,
    htype: v4::HType,
    chaddr: &[u8],
) -> io::Result<()> {
    let addr_in = libc::sockaddr_in {
        sin_family: libc::AF_INET as _,
        sin_port: v4::CLIENT_PORT.to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(yiaddr.octets()),
        },
        ..unsafe { std::mem::zeroed() }
    };
    // memcpy to sockaddr for arp_req. sockaddr_in and sockaddr both 16 bytes
    let arp_pa: libc::sockaddr = unsafe { std::mem::transmute(addr_in) };
    // create arp_ha (for hardware addr)
    let arp_ha = libc::sockaddr {
        sa_family: u8::from(htype) as _,
        sa_data: unsafe { super::ioctl::cpy_bytes::<14>(chaddr) },
    };

    let arp_req = libc::arpreq {
        arp_pa,
        arp_ha,
        arp_flags: libc::ATF_COM,
        // this line may or may not be necessary? dnsmasq does it but it seems to work without
        // arp_dev: unsafe { super::ioctl::cpy_bytes::<16>(device.as_bytes()) },
        ..unsafe { std::mem::zeroed() }
    };

    let res = unsafe {
        libc::ioctl(
            soc.as_raw_fd(),
            libc::SIOCSARP,
            &arp_req as *const libc::arpreq,
        )
    };
    if res == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Returns
/// A zeroed out array of size `N` with all the `bytes` copied in.
///
/// # Safety
/// will create a new slice of `&[libc::c_char]` from the bytes.
///
/// # Panics
/// if `bytes.len() > N`
pub unsafe fn cpy_bytes<const N: usize>(bytes: &[u8]) -> [libc::c_char; N] {
    let mut sa_data = [0; N];
    let len = bytes.len();

    sa_data[..len].copy_from_slice(std::slice::from_raw_parts(
        bytes.as_ptr() as *const libc::c_char,
        len,
    ));
    sa_data
}
