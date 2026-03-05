// TSI (Transparent Socket Impersonation) Windows implementation
// Phase 1: Windows Socket abstraction layer

pub mod socket_wrapper;

pub use socket_wrapper::{WindowsSocket, AddressFamily, SockType};
