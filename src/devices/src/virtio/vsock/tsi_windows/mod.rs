// TSI (Transparent Socket Impersonation) Windows implementation
// Phase 1: Windows Socket abstraction layer
// Phase 2: TSI Stream Proxy (TCP)
// Phase 3: TSI DGRAM Proxy (UDP)

pub mod socket_wrapper;
pub mod stream_proxy;
pub mod dgram_proxy;

pub use socket_wrapper::{WindowsSocket, AddressFamily, SockType, ShutdownMode};
pub use stream_proxy::{TsiStreamProxyWindows, ProxyStatus, ProxyError};
pub use dgram_proxy::TsiDgramProxyWindows;
