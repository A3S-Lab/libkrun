// TSI Named Pipes Proxy for Windows
// Handles Windows Named Pipe connections for vsock communication

use super::stream_proxy::ProxyError;
use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
use std::ptr;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_ACCESS_DUPLEX,
    PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

/// Named Pipe proxy status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeStatus {
    Init,
    Listening,
    Connected,
    Closed,
}

/// TSI Named Pipe Proxy for Windows
pub struct TsiPipeProxyWindows {
    pipe_handle: HANDLE,
    status: PipeStatus,
    pipe_name: String,
}

impl TsiPipeProxyWindows {
    /// Create a new pipe proxy
    pub fn new() -> Self {
        Self {
            pipe_handle: INVALID_HANDLE_VALUE,
            status: PipeStatus::Init,
            pipe_name: String::new(),
        }
    }

    /// Create and listen on a named pipe
    pub fn listen(&mut self, pipe_name: &str) -> Result<(), ProxyError> {
        if self.status != PipeStatus::Init {
            return Err(ProxyError::InvalidState);
        }

        // Convert pipe name to Windows format: \\.\pipe\name
        let full_name = if pipe_name.starts_with("\\\\.\\pipe\\") {
            pipe_name.to_string()
        } else {
            format!("\\\\.\\pipe\\{}", pipe_name)
        };

        // Convert to wide string
        let wide_name: Vec<u16> = full_name.encode_utf16().chain(std::iter::once(0)).collect();

        // Create named pipe
        let handle = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096, // out buffer size
                4096, // in buffer size
                0,    // default timeout
                ptr::null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return Err(ProxyError::IoError(io::Error::last_os_error()));
        }

        self.pipe_handle = handle;
        self.pipe_name = full_name;
        self.status = PipeStatus::Listening;
        Ok(())
    }

    /// Accept a connection (blocking)
    pub fn accept(&mut self) -> Result<(), ProxyError> {
        if self.status != PipeStatus::Listening {
            return Err(ProxyError::InvalidState);
        }

        let result = unsafe { ConnectNamedPipe(self.pipe_handle, ptr::null_mut()) };

        if result == 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
                return Err(ProxyError::WouldBlock);
            }
            return Err(ProxyError::IoError(err));
        }

        self.status = PipeStatus::Connected;
        Ok(())
    }

    /// Connect to an existing named pipe (client mode)
    pub fn connect(&mut self, pipe_name: &str) -> Result<(), ProxyError> {
        if self.status != PipeStatus::Init {
            return Err(ProxyError::InvalidState);
        }

        // Convert pipe name to Windows format
        let full_name = if pipe_name.starts_with("\\\\.\\pipe\\") {
            pipe_name.to_string()
        } else {
            format!("\\\\.\\pipe\\{}", pipe_name)
        };

        let wide_name: Vec<u16> = full_name.encode_utf16().chain(std::iter::once(0)).collect();

        // Open existing pipe
        let handle = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                0x80000000 | 0x40000000, // GENERIC_READ | GENERIC_WRITE
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                0,
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return Err(ProxyError::IoError(io::Error::last_os_error()));
        }

        self.pipe_handle = handle;
        self.pipe_name = full_name;
        self.status = PipeStatus::Connected;
        Ok(())
    }

    /// Send data through the pipe
    pub fn send_data(&mut self, data: &[u8]) -> Result<usize, ProxyError> {
        if self.status != PipeStatus::Connected {
            return Err(ProxyError::InvalidState);
        }

        // Use std::fs::File wrapper for Write trait
        let mut file = unsafe { std::fs::File::from_raw_handle(self.pipe_handle as RawHandle) };
        let result = file.write(data);
        std::mem::forget(file); // Don't close the handle

        result.map_err(|e| {
            if e.kind() == io::ErrorKind::WouldBlock {
                ProxyError::WouldBlock
            } else {
                ProxyError::IoError(e)
            }
        })
    }

    /// Receive data from the pipe
    pub fn recv_data(&mut self, buf: &mut [u8]) -> Result<usize, ProxyError> {
        if self.status != PipeStatus::Connected {
            return Err(ProxyError::InvalidState);
        }

        let mut file = unsafe { std::fs::File::from_raw_handle(self.pipe_handle as RawHandle) };
        let result = file.read(buf);
        std::mem::forget(file);

        result.map_err(|e| {
            if e.kind() == io::ErrorKind::WouldBlock {
                ProxyError::WouldBlock
            } else {
                ProxyError::IoError(e)
            }
        })
    }

    /// Disconnect the pipe
    pub fn disconnect(&mut self) -> Result<(), ProxyError> {
        if self.status == PipeStatus::Connected && self.pipe_handle != INVALID_HANDLE_VALUE {
            unsafe {
                DisconnectNamedPipe(self.pipe_handle);
            }
            self.status = PipeStatus::Listening;
        }
        Ok(())
    }

    /// Get current status
    pub fn status(&self) -> PipeStatus {
        self.status
    }

    /// Get pipe name
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }
}

impl Drop for TsiPipeProxyWindows {
    fn drop(&mut self) {
        if self.pipe_handle != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.pipe_handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipe_proxy_creation() {
        let proxy = TsiPipeProxyWindows::new();
        assert_eq!(proxy.status(), PipeStatus::Init);
    }

    #[test]
    #[ignore] // Requires Windows Named Pipe support
    fn test_pipe_listen() {
        let mut proxy = TsiPipeProxyWindows::new();
        let result = proxy.listen("test_pipe_listen");
        assert!(result.is_ok());
        assert_eq!(proxy.status(), PipeStatus::Listening);
    }
}
