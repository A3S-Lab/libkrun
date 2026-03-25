// TSI (Transparent Socket Impersonation) Windows implementation
// Phase 1: Windows Socket abstraction layer
// Phase 2: TSI Stream Proxy (TCP)
// Phase 3: TSI DGRAM Proxy (UDP)
// Phase 4: TSI Named Pipes Proxy

pub mod dgram_proxy;
pub mod pipe_proxy;
pub mod socket_wrapper;
pub mod stream_proxy;

pub use dgram_proxy::TsiDgramProxyWindows;
pub use pipe_proxy::{PipeStatus, TsiPipeProxyWindows};
pub use socket_wrapper::{AddressFamily, ShutdownMode, SockType, WindowsSocket};
pub use stream_proxy::{ProxyError, ProxyStatus, TsiStreamProxyWindows};
