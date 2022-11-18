use std::{
    io,
    os::unix::io::{FromRawFd, IntoRawFd},
};

use socket2::{Domain, Protocol, Type};
use std::net::SocketAddr;
use tokio::net::UdpSocket;

pub struct Socket {
    pub(crate) socket: UdpSocket,
}

impl Socket {
    pub fn new(domain: Domain, type_: Type, protocol: Protocol) -> io::Result<Self> {
        let socket = socket2::Socket::new(domain, type_, Some(protocol))?;
        socket.set_nonblocking(true)?;
        #[cfg(windows)]
        let socket = UdpSocket::from_std(unsafe {
            std::net::UdpSocket::from_raw_socket(socket.into_raw_socket())
        })?;
        #[cfg(unix)]
        let socket =
            UdpSocket::from_std(unsafe { std::net::UdpSocket::from_raw_fd(socket.into_raw_fd()) })?;

        Ok(Self { socket })
    }

    pub async fn send_to(&self, buf: &[u8], target: &SocketAddr) -> io::Result<usize> {
        self.socket.send_to(buf, target).await
    }

    pub async fn recv(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf).await
    }
}
