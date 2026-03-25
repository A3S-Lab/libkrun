// TSI DGRAM Proxy for Windows
// Handles UDP socket operations (sendto, recvfrom) for guest

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use super::socket_wrapper::{AddressFamily, SockType, WindowsSocket};
use super::stream_proxy::{ProxyError, ProxyStatus};
use crate::virtio::vsock::defs;
use crate::virtio::Queue as VirtQueue;
use vm_memory::GuestMemoryMmap;

/// TSI DGRAM Proxy for Windows (UDP)
pub struct TsiDgramProxyWindows {
    id: u64,
    cid: u64,
    family: AddressFamily,
    local_port: u32,
    peer_port: u32,
    socket: WindowsSocket,
    pub status: ProxyStatus,
    mem: GuestMemoryMmap,
    queue: Arc<Mutex<VirtQueue>>,
    // Cache of remote addresses for connectionless UDP
    remote_addrs: HashMap<u32, SocketAddr>, // guest_port -> remote_addr
    bound_addr: Option<SocketAddr>,
}

impl TsiDgramProxyWindows {
    /// Create a new TSI DGRAM Proxy
    pub fn new(
        id: u64,
        cid: u64,
        family: u16,
        local_port: u32,
        peer_port: u32,
        mem: GuestMemoryMmap,
        queue: Arc<Mutex<VirtQueue>>,
    ) -> Result<Self, ProxyError> {
        // Convert Linux address family to Windows
        let family = match family {
            defs::LINUX_AF_INET => AddressFamily::Inet,
            defs::LINUX_AF_INET6 => AddressFamily::Inet6,
            _ => return Err(ProxyError::InvalidFamily),
        };

        // Create UDP socket
        let socket =
            WindowsSocket::new(family, SockType::Dgram).map_err(ProxyError::CreatingSocket)?;

        // Set non-blocking mode
        socket
            .set_nonblocking(true)
            .map_err(ProxyError::SettingNonBlocking)?;

        // Set SO_REUSEADDR
        socket
            .set_reuseaddr(true)
            .map_err(ProxyError::SettingReuseAddr)?;

        Ok(Self {
            id,
            cid,
            family,
            local_port,
            peer_port,
            socket,
            status: ProxyStatus::Init,
            mem,
            queue,
            remote_addrs: HashMap::new(),
            bound_addr: None,
        })
    }

    /// Get proxy ID
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Get local port
    pub fn local_port(&self) -> u32 {
        self.local_port
    }

    /// Bind to a local address
    pub fn bind(&mut self, addr: &SocketAddr) -> Result<(), ProxyError> {
        if self.status != ProxyStatus::Init {
            return Err(ProxyError::InvalidState);
        }

        self.socket.bind(addr).map_err(ProxyError::Binding)?;
        self.bound_addr = Some(*addr);
        self.status = ProxyStatus::Connected; // UDP is "connected" after bind

        Ok(())
    }

    /// Send datagram to a specific address
    pub fn sendto(&mut self, data: &[u8], addr: &SocketAddr) -> Result<usize, ProxyError> {
        // For UDP, we need to use sendto with address
        // Windows socket wrapper doesn't have sendto yet, so we'll use send after connecting

        // Store the remote address for this port
        self.remote_addrs.insert(self.peer_port, *addr);

        // For now, use send (which requires connect first)
        // In a full implementation, we'd add sendto to WindowsSocket
        self.socket.send(data).map_err(ProxyError::Sending)
    }

    /// Receive datagram
    pub fn recvfrom(&mut self, buf: &mut [u8]) -> Result<(usize, Option<SocketAddr>), ProxyError> {
        match self.socket.recv(buf) {
            Ok(n) => {
                // For UDP, we should also return the source address
                // In a full implementation, we'd use recvfrom
                let addr = self.remote_addrs.get(&self.peer_port).copied();
                Ok((n, addr))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok((0, None)),
            Err(e) => Err(ProxyError::Receiving(e)),
        }
    }

    /// Get bound address
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.bound_addr
    }

    /// Close the proxy
    pub fn close(&mut self) {
        self.status = ProxyStatus::Closed;
        // Socket will be closed automatically by Drop
    }
}

/// Parse address from TSI request (same as stream_proxy)
pub fn parse_address(family: u16, addr_bytes: &[u8], port: u16) -> Option<SocketAddr> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    match family {
        defs::LINUX_AF_INET => {
            if addr_bytes.len() < 4 {
                return None;
            }
            let ip = Ipv4Addr::new(addr_bytes[0], addr_bytes[1], addr_bytes[2], addr_bytes[3]);
            Some(SocketAddr::new(IpAddr::V4(ip), port))
        }
        defs::LINUX_AF_INET6 => {
            if addr_bytes.len() < 16 {
                return None;
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&addr_bytes[0..16]);
            let ip = Ipv6Addr::from(octets);
            Some(SocketAddr::new(IpAddr::V6(ip), port))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dgram_proxy_creation() {
        use vm_memory::{GuestAddress, GuestMemoryMmap};

        WindowsSocket::init_winsock().unwrap();

        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x1000)]).unwrap();
        let queue = Arc::new(Mutex::new(VirtQueue::new(256)));

        let proxy = TsiDgramProxyWindows::new(1, 2, defs::LINUX_AF_INET, 8080, 9090, mem, queue);

        assert!(proxy.is_ok());
        let proxy = proxy.unwrap();
        assert_eq!(proxy.id(), 1);
        assert_eq!(proxy.local_port(), 8080);
    }

    #[test]
    fn test_dgram_bind() {
        use vm_memory::{GuestAddress, GuestMemoryMmap};

        WindowsSocket::init_winsock().unwrap();

        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x1000)]).unwrap();
        let queue = Arc::new(Mutex::new(VirtQueue::new(256)));

        let mut proxy = TsiDgramProxyWindows::new(
            1,
            2,
            defs::LINUX_AF_INET,
            0, // Let OS assign port
            9090,
            mem,
            queue,
        )
        .unwrap();

        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        assert!(proxy.bind(&addr).is_ok());
        assert!(proxy.local_addr().is_some());
    }
}
