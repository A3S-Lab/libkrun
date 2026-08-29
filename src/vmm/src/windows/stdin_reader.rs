// Copyright 2024 The libkrun Authors.
// SPDX-License-Identifier: Apache-2.0

//! Windows stdin reader for legacy serial (COM1) input.
//!
//! Spawns a cancellable background thread that waits on stdin and feeds bytes
//! into a ring buffer. The ring buffer is paired with an EventFd so that the
//! EventManager can wake the serial Subscriber when data is available.

use std::collections::VecDeque;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::sync::{Arc, Mutex};
use std::thread;

use utils::eventfd::{EventFd, EFD_NONBLOCK};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::ReadFile;
use windows::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
use windows::Win32::System::Threading::{WaitForMultipleObjects, INFINITE};
use windows::Win32::System::IO::CancelSynchronousIo;

/// Implements `io::Read` and `devices::legacy::ReadableFd` for Windows stdin.
///
/// A background thread waits on the Win32 stdin handle and places bytes into
/// the ring buffer. The paired `EventFd` is signalled whenever new bytes
/// arrive, allowing the EventManager to call the serial device's
/// `Subscriber::process()` without blocking the event loop. Dropping the input
/// signals a stop event and cancels any in-flight synchronous read.
pub struct WindowsStdinInput {
    buffer: Arc<Mutex<VecDeque<u8>>>,
    event: Arc<EventFd>,
    stop: EventFd,
    reader_thread: Option<thread::JoinHandle<()>>,
}

impl WindowsStdinInput {
    /// Create a new `WindowsStdinInput`, spawning the background reader thread.
    pub fn new() -> io::Result<Self> {
        let buffer = Arc::new(Mutex::new(VecDeque::new()));
        let event = Arc::new(EventFd::new(EFD_NONBLOCK)?);
        let stop = EventFd::new(EFD_NONBLOCK)?;
        let thread_stop = stop.try_clone()?;

        let stdin_handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) }
            .map_err(|e| io::Error::other(format!("GetStdHandle failed: {e}")))?;
        if stdin_handle.is_invalid() || stdin_handle.0.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "stdin has no valid Windows handle",
            ));
        }
        let stdin_raw = stdin_handle.0 as isize;

        let buf_clone = Arc::clone(&buffer);
        let evt_clone = Arc::clone(&event);

        let reader_thread = thread::Builder::new()
            .name("windows-stdin-reader".into())
            .spawn(move || {
                let stdin_handle = HANDLE(stdin_raw as *mut _);
                let stop_handle = HANDLE(thread_stop.as_raw_handle());
                let handles = [stdin_handle, stop_handle];
                let mut tmp = [0u8; 64];
                loop {
                    let wait_result = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                    if wait_result.0 == 1 {
                        break;
                    }
                    if wait_result.0 != 0 {
                        break;
                    }

                    let mut bytes_read = 0u32;
                    match unsafe {
                        ReadFile(stdin_handle, Some(&mut tmp), Some(&mut bytes_read), None)
                    } {
                        Ok(()) if bytes_read == 0 => break,
                        Err(_) => break,
                        Ok(()) => {
                            let n = bytes_read as usize;
                            {
                                let mut q = buf_clone.lock().unwrap();
                                q.extend(&tmp[..n]);
                            }
                            // Signal the EventFd; ignore errors (e.g. if the VM has
                            // already shut down and the receiver is gone).
                            let _ = evt_clone.write(1);
                        }
                    }
                }
            })?;

        Ok(Self {
            buffer,
            event,
            stop,
            reader_thread: Some(reader_thread),
        })
    }
}

impl Drop for WindowsStdinInput {
    fn drop(&mut self) {
        let _ = self.stop.write(1);
        if let Some(reader_thread) = self.reader_thread.take() {
            unsafe {
                let _ = CancelSynchronousIo(HANDLE(reader_thread.as_raw_handle()));
            }
            let _ = reader_thread.join();
        }
    }
}

impl io::Read for WindowsStdinInput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut q = self.buffer.lock().unwrap();
        // Drain the ring buffer; return 0 if nothing is available yet.
        let n = q.len().min(buf.len());
        for b in &mut buf[..n] {
            *b = q.pop_front().unwrap();
        }
        // Reset the EventFd if the buffer is now empty so that the next write
        // to it increments from 0 → 1 again.
        if q.is_empty() {
            let _ = self.event.read(); // consume the pending count; ignore errors
        }
        Ok(n)
    }
}

impl devices::legacy::ReadableFd for WindowsStdinInput {
    /// Returns the synthetic fd (EventFd ID) used by the EventManager.
    fn as_raw_fd(&self) -> i32 {
        self.event.as_raw_fd()
    }
}
