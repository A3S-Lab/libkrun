// TSI Stream Proxy for Windows
// Handles TCP socket operations (connect, listen, accept) for guest

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use super::socket_wrapper::{AddressFamily, ShutdownMode, SockType, WindowsSocket};
use crate::virtio::vsock::defs;
use crate::virtio::vsock::packet::{TsiAcceptReq, TsiConnectReq, TsiListenReq, VsockPacket};
use crate::virtio::Queue as VirtQueue;
use vm_memory::GuestMemoryMmap;

/// Proxy status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyStatus {
    Init,
    Connecting,
    Connected,
    Listening,
    Closed,
}

/// Proxy error types
#[derive(Debug)]
pub enum ProxyError {
    InvalidFamily,
    CreatingSocket(io::Error),
    SettingNonBlocking(io::Error),
    SettingReuseAddr(io::Error),
    Binding(io::Error),
    Connecting(io::Error),
    Listening(io::Error),
    Accepting(io::Error),
    Sending(io::Error),
    Receiving(io::Error),
    InvalidState,
    InvalidAddress,
}

impl From<ProxyError> for io::Error {
    fn from(err: ProxyError) -> io::Error {
        match err {
            ProxyError::CreatingSocket(e) => e,
            ProxyError::SettingNonBlocking(e) => e,
            ProxyError::SettingReuseAddr(e) => e,
            ProxyError::Binding(e) => e,
            ProxyError::Connecting(e) => e,
            ProxyError::Listening(e) => e,
            ProxyError::Accepting(e) => e,
            ProxyError::Sending(e) => e,
            ProxyError::Receiving(e) => e,
            _ => io::Error::new(io::ErrorKind::Other, format!("{:?}", err)),
        }
    }
}

/// TSI Stream Proxy for Windows
pub struct TsiStreamProxyWindows {
    id: u64,
    cid: u64,
    family: AddressFamily,
    local_port: u32,
    peer_port: u32,
    control_port: u32,
    socket: WindowsSocket,
    pub status: ProxyStatus,
    mem: GuestMemoryMmap,
    queue: Arc<Mutex<VirtQueue>>,
    // Pending accept connections for listening sockets
    pending_accepts: Vec<(WindowsSocket, SocketAddr)>,
}

impl TsiStreamProxyWindows {
    /// Create a new TSI Stream Proxy
    pub fn new(
        id: u64,
        cid: u64,
        family: u16,
        local_port: u32,
        peer_port: u32,
        control_port: u32,
        mem: GuestMemoryMmap,
        queue: Arc<Mutex<VirtQueue>>,
    ) -> Result<Self, ProxyError> {
        // Convert Linux address family to Windows
        let family = match family {
            defs::LINUX_AF_INET => AddressFamily::Inet,
            defs::LINUX_AF_INET6 => AddressFamily::Inet6,
            _ => return Err(ProxyError::InvalidFamily),
        };

        // Create socket
        let socket = WindowsSocket::new(family, SockType::Stream)
            .map_err(ProxyError::CreatingSocket)?;

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
            control_port,
            socket,
            status: ProxyStatus::Init,
            mem,
            queue,
            pending_accepts: Vec::new(),
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

    /// Process TSI_CONNECT request
    pub fn process_connect(&mut self, req: &TsiConnectReq) -> Result<(), ProxyError> {
        if self.status != ProxyStatus::Init {
            return Err(ProxyError::InvalidState);
        }

        // Parse address from request
        let addr = parse_address(req.family, &req.addr, req.port)
            .ok_or(ProxyError::InvalidAddress)?;

        // Connect to remote address
        self.socket
            .connect(&addr)
            .map_err(ProxyError::Connecting)?;

        self.status = ProxyStatus::Connecting;

        // Note: Connection may complete asynchronously
        // Caller should check socket status later

        Ok(())
    }

    /// Process TSI_LISTEN request
    pub fn process_listen(&mut self, req: &TsiListenReq) -> Result<(), ProxyError> {
        if self.status != ProxyStatus::Init {
            return Err(ProxyError::InvalidState);
        }

        // Parse bind address from request
        let addr = parse_address(req.family, &req.addr, req.port)
            .ok_or(ProxyError::InvalidAddress)?;

        // Bind to address
        self.socket.bind(&addr).map_err(ProxyError::Binding)?;

        // Listen with specified backlog
        self.socket
            .listen(req.backlog as i32)
            .map_err(ProxyError::Listening)?;

        self.status = ProxyStatus::Listening;

        Ok(())
    }

    /// Process TSI_ACCEPT request
    pub fn process_accept(&mut self) -> Result<Option<(u64, SocketAddr)>, ProxyError> {
        if self.status != ProxyStatus::Listening {
            return Err(ProxyError::InvalidState);
        }

        // Try to accept a connection
        match self.socket.accept() {
            Ok((client_socket, client_addr)) => {
                // Set non-blocking mode for client socket
                client_socket
                    .set_nonblocking(true)
                    .map_err(ProxyError::SettingNonBlocking)?;

                // Store pending accept
                self.pending_accepts.push((client_socket, client_addr));

                // Generate new connection ID
                let conn_id = self.id + self.pending_accepts.len() as u64;

                Ok(Some((conn_id, client_addr)))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No pending connections
                Ok(None)
            }
            Err(e) => Err(ProxyError::Accepting(e)),
        }
    }

    /// Send data to remote peer
    pub fn send_data(&mut self, data: &[u8]) -> Result<usize, ProxyError> {
        if self.status != ProxyStatus::Connected {
            return Err(ProxyError::InvalidState);
        }

        self.socket.send(data).map_err(ProxyError::Sending)
    }

    /// Receive data from remote peer
    pub fn recv_data(&mut self, buf: &mut [u8]) -> Result<usize, ProxyError> {
        if self.status != ProxyStatus::Connected {
            return Err(ProxyError::InvalidState);
        }

        match self.socket.recv(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            Err(e) => Err(ProxyError::Receiving(e)),
        }
    }

    /// Check if connection is established (for async connect)
    pub fn check_connected(&mut self) -> Result<bool, ProxyError> {
        if self.status != ProxyStatus::Connecting {
            return Ok(false);
        }

        // Try to get peer address to check if connected
        match self.socket.peer_addr() {
            Ok(_) => {
                self.status = ProxyStatus::Connected;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotConnected => Ok(false),
            Err(e) => Err(ProxyError::Connecting(e)),
        }
    }

    /// Shutdown the connection
    pub fn shutdown(&mut self, mode: ShutdownMode) -> Result<(), ProxyError> {
        self.socket
            .shutdown(mode)
            .map_err(|e| ProxyError::Sending(e))
    }

    /// Close the proxy
    pub fn close(&mut self) {
        self.status = ProxyStatus::Closed;
        // Socket will be closed automatically by Drop
    }
}

/// Parse address from TSI request
fn parse_address(family: u16, addr_bytes: &[u8], port: u16) -> Option<SocketAddr> {
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
    fn test_parse_ipv4_address() {
        let addr_bytes = [127, 0, 0, 1];
        let addr = parse_address(defs::LINUX_AF_INET, &addr_bytes, 8080);
        assert!(addr.is_some());
        let addr = addr.unwrap();
        assert_eq!(addr.port(), 8080);
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
    }

    #[test]
    fn test_parse_ipv6_address() {
        let addr_bytes = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        let addr = parse_address(defs::LINUX_AF_INET6, &addr_bytes, 8080);
        assert!(addr.is_some());
        let addr = addr.unwrap();
        assert_eq!(addr.port(), 8080);
        assert_eq!(addr.ip().to_string(), "::1");
    }
}
