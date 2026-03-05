// Windows Socket abstraction layer
// Wraps Winsock2 APIs in a Rust-friendly interface

use std::io;
use std::mem;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ptr;

use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Networking::WinSock::{
    accept, bind, closesocket, connect, ioctlsocket, listen, recv, send, socket,
    getsockname, getpeername, setsockopt, shutdown,
    AF_INET, AF_INET6, AF_UNSPEC,
    FIONBIO, INVALID_SOCKET,
    IN_ADDR, IN6_ADDR, IPPROTO_TCP, IPPROTO_UDP,
    SD_BOTH, SD_RECEIVE, SD_SEND,
    SOCKADDR, SOCKADDR_IN, SOCKADDR_IN6, SOCKADDR_STORAGE,
    SOCKET, SOCKET_ERROR,
    SOCK_DGRAM, SOCK_STREAM,
    SOL_SOCKET, SO_REUSEADDR,
    WSAGetLastError, WSAStartup, WSADATA,
};

/// Address family for sockets
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
    Inet,   // IPv4
    Inet6,  // IPv6
}

impl AddressFamily {
    fn to_windows(&self) -> i32 {
        match self {
            AddressFamily::Inet => AF_INET.0 as i32,
            AddressFamily::Inet6 => AF_INET6.0 as i32,
        }
    }
}

/// Socket type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SockType {
    Stream,  // TCP
    Dgram,   // UDP
}

impl SockType {
    fn to_windows(&self) -> i32 {
        match self {
            SockType::Stream => SOCK_STREAM.0 as i32,
            SockType::Dgram => SOCK_DGRAM.0 as i32,
        }
    }

    fn protocol(&self) -> i32 {
        match self {
            SockType::Stream => IPPROTO_TCP.0 as i32,
            SockType::Dgram => IPPROTO_UDP.0 as i32,
        }
    }
}

/// Shutdown mode
#[derive(Debug, Clone, Copy)]
pub enum ShutdownMode {
    Read,
    Write,
    Both,
}

impl ShutdownMode {
    fn to_windows(&self) -> i32 {
        match self {
            ShutdownMode::Read => SD_RECEIVE.0 as i32,
            ShutdownMode::Write => SD_SEND.0 as i32,
            ShutdownMode::Both => SD_BOTH.0 as i32,
        }
    }
}

/// Windows Socket wrapper
pub struct WindowsSocket {
    socket: SOCKET,
    family: AddressFamily,
    sock_type: SockType,
}

impl WindowsSocket {
    /// Initialize Winsock (call once at startup)
    pub fn init_winsock() -> io::Result<()> {
        unsafe {
            let mut wsa_data: WSADATA = mem::zeroed();
            let result = WSAStartup(0x0202, &mut wsa_data); // Request Winsock 2.2
            if result != 0 {
                return Err(io::Error::from_raw_os_error(result));
            }
        }
        Ok(())
    }

    /// Create a new socket
    pub fn new(family: AddressFamily, sock_type: SockType) -> io::Result<Self> {
        unsafe {
            let socket = socket(
                family.to_windows(),
                sock_type.to_windows(),
                sock_type.protocol(),
            );

            if socket == INVALID_SOCKET {
                return Err(io::Error::last_os_error());
            }

            Ok(Self {
                socket,
                family,
                sock_type,
            })
        }
    }

    /// Get the raw socket handle
    pub fn as_raw_socket(&self) -> SOCKET {
        self.socket
    }

    /// Set socket to non-blocking mode
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        unsafe {
            let mut mode: u32 = if nonblocking { 1 } else { 0 };
            let result = ioctlsocket(self.socket, FIONBIO, &mut mode as *mut u32);

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Set SO_REUSEADDR option
    pub fn set_reuseaddr(&self, reuse: bool) -> io::Result<()> {
        unsafe {
            let optval: i32 = if reuse { 1 } else { 0 };
            let result = setsockopt(
                self.socket,
                SOL_SOCKET,
                SO_REUSEADDR,
                &optval as *const i32 as *const u8,
                mem::size_of::<i32>() as i32,
            );

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Bind socket to an address
    pub fn bind(&self, addr: &SocketAddr) -> io::Result<()> {
        unsafe {
            let (sockaddr_ptr, sockaddr_len) = socket_addr_to_sockaddr(addr)?;

            let result = bind(self.socket, sockaddr_ptr, sockaddr_len);

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Connect to a remote address
    pub fn connect(&self, addr: &SocketAddr) -> io::Result<()> {
        unsafe {
            let (sockaddr_ptr, sockaddr_len) = socket_addr_to_sockaddr(addr)?;

            let result = connect(self.socket, sockaddr_ptr, sockaddr_len);

            if result == SOCKET_ERROR {
                let err = WSAGetLastError();
                // WSAEWOULDBLOCK (10035) is expected for non-blocking sockets
                if err.0 != 10035 {
                    return Err(io::Error::from_raw_os_error(err.0));
                }
            }
        }
        Ok(())
    }

    /// Listen for incoming connections
    pub fn listen(&self, backlog: i32) -> io::Result<()> {
        unsafe {
            let result = listen(self.socket, backlog);

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Accept an incoming connection
    pub fn accept(&self) -> io::Result<(Self, SocketAddr)> {
        unsafe {
            let mut storage: SOCKADDR_STORAGE = mem::zeroed();
            let mut addrlen = mem::size_of::<SOCKADDR_STORAGE>() as i32;

            let new_socket = accept(
                self.socket,
                &mut storage as *mut SOCKADDR_STORAGE as *mut SOCKADDR,
                &mut addrlen,
            );

            if new_socket == INVALID_SOCKET {
                return Err(io::Error::last_os_error());
            }

            let addr = sockaddr_to_socket_addr(&storage, addrlen)?;

            Ok((
                Self {
                    socket: new_socket,
                    family: self.family,
                    sock_type: self.sock_type,
                },
                addr,
            ))
        }
    }

    /// Send data
    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        unsafe {
            let result = send(
                self.socket,
                buf.as_ptr() as *const u8,
                buf.len() as i32,
                0,
            );

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }

            Ok(result as usize)
        }
    }

    /// Receive data
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            let result = recv(
                self.socket,
                buf.as_mut_ptr() as *mut u8,
                buf.len() as i32,
                0,
            );

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }

            Ok(result as usize)
        }
    }

    /// Get local address
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        unsafe {
            let mut storage: SOCKADDR_STORAGE = mem::zeroed();
            let mut addrlen = mem::size_of::<SOCKADDR_STORAGE>() as i32;

            let result = getsockname(
                self.socket,
                &mut storage as *mut SOCKADDR_STORAGE as *mut SOCKADDR,
                &mut addrlen,
            );

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }

            sockaddr_to_socket_addr(&storage, addrlen)
        }
    }

    /// Get peer address
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        unsafe {
            let mut storage: SOCKADDR_STORAGE = mem::zeroed();
            let mut addrlen = mem::size_of::<SOCKADDR_STORAGE>() as i32;

            let result = getpeername(
                self.socket,
                &mut storage as *mut SOCKADDR_STORAGE as *mut SOCKADDR,
                &mut addrlen,
            );

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }

            sockaddr_to_socket_addr(&storage, addrlen)
        }
    }

    /// Shutdown the socket
    pub fn shutdown(&self, mode: ShutdownMode) -> io::Result<()> {
        unsafe {
            let result = shutdown(self.socket, mode.to_windows());

            if result == SOCKET_ERROR {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

impl Drop for WindowsSocket {
    fn drop(&mut self) {
        unsafe {
            closesocket(self.socket);
        }
    }
}

// Helper functions for address conversion

unsafe fn socket_addr_to_sockaddr(addr: &SocketAddr) -> io::Result<(*const SOCKADDR, i32)> {
    match addr {
        SocketAddr::V4(addr_v4) => {
            let mut sockaddr: SOCKADDR_IN = mem::zeroed();
            sockaddr.sin_family = AF_INET;
            sockaddr.sin_port = addr_v4.port().to_be();
            sockaddr.sin_addr = IN_ADDR {
                S_un: windows::Win32::Networking::WinSock::IN_ADDR_0 {
                    S_addr: u32::from_ne_bytes(addr_v4.ip().octets()),
                },
            };

            // Leak the sockaddr to get a stable pointer
            let boxed = Box::new(sockaddr);
            let ptr = Box::into_raw(boxed);

            Ok((
                ptr as *const SOCKADDR,
                mem::size_of::<SOCKADDR_IN>() as i32,
            ))
        }
        SocketAddr::V6(addr_v6) => {
            let mut sockaddr: SOCKADDR_IN6 = mem::zeroed();
            sockaddr.sin6_family = AF_INET6;
            sockaddr.sin6_port = addr_v6.port().to_be();
            sockaddr.sin6_addr = IN6_ADDR {
                u: windows::Win32::Networking::WinSock::IN6_ADDR_0 {
                    Byte: addr_v6.ip().octets(),
                },
            };
            sockaddr.sin6_scope_id = addr_v6.scope_id();

            let boxed = Box::new(sockaddr);
            let ptr = Box::into_raw(boxed);

            Ok((
                ptr as *const SOCKADDR,
                mem::size_of::<SOCKADDR_IN6>() as i32,
            ))
        }
    }
}

unsafe fn sockaddr_to_socket_addr(
    storage: &SOCKADDR_STORAGE,
    _addrlen: i32,
) -> io::Result<SocketAddr> {
    let family = storage.ss_family;

    if family == AF_INET.0 {
        let sockaddr = &*(storage as *const SOCKADDR_STORAGE as *const SOCKADDR_IN);
        let ip = Ipv4Addr::from(u32::from_be(sockaddr.sin_addr.S_un.S_addr));
        let port = u16::from_be(sockaddr.sin_port);
        Ok(SocketAddr::new(IpAddr::V4(ip), port))
    } else if family == AF_INET6.0 {
        let sockaddr = &*(storage as *const SOCKADDR_STORAGE as *const SOCKADDR_IN6);
        let ip = Ipv6Addr::from(sockaddr.sin6_addr.u.Byte);
        let port = u16::from_be(sockaddr.sin6_port);
        Ok(SocketAddr::new(IpAddr::V6(ip), port))
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unsupported address family",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_creation() {
        WindowsSocket::init_winsock().unwrap();

        let socket = WindowsSocket::new(AddressFamily::Inet, SockType::Stream);
        assert!(socket.is_ok());
    }

    #[test]
    fn test_nonblocking() {
        WindowsSocket::init_winsock().unwrap();

        let socket = WindowsSocket::new(AddressFamily::Inet, SockType::Stream).unwrap();
        assert!(socket.set_nonblocking(true).is_ok());
        assert!(socket.set_nonblocking(false).is_ok());
    }

    #[test]
    fn test_bind_and_listen() {
        WindowsSocket::init_winsock().unwrap();

        let socket = WindowsSocket::new(AddressFamily::Inet, SockType::Stream).unwrap();
        let addr = "127.0.0.1:0".parse().unwrap();

        assert!(socket.bind(&addr).is_ok());
        assert!(socket.listen(5).is_ok());

        let local_addr = socket.local_addr().unwrap();
        assert_eq!(local_addr.ip(), "127.0.0.1".parse::<IpAddr>().unwrap());
    }
}
