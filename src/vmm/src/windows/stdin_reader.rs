// Copyright 2024 The libkrun Authors.
// SPDX-License-Identifier: Apache-2.0

//! Windows stdin reader for legacy serial (COM1) input.
//!
//! Spawns a background thread that blocks on stdin and feeds bytes into a
//! ring buffer. The ring buffer is paired with an EventFd so that the
//! EventManager can wake the serial Subscriber when data is available.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::thread;

use utils::eventfd::{EventFd, EFD_NONBLOCK};

/// Implements `io::Read` and `devices::legacy::ReadableFd` for Windows stdin.
///
/// A background thread reads from `std::io::stdin()` (blocking) and places
/// bytes into the ring buffer. The paired `EventFd` is signalled whenever new
/// bytes arrive, allowing the EventManager to call the serial device's
/// `Subscriber::process()` without blocking the event loop.
pub struct WindowsStdinInput {
    buffer: Arc<Mutex<VecDeque<u8>>>,
    event: Arc<EventFd>,
}

impl WindowsStdinInput {
    /// Create a new `WindowsStdinInput`, spawning the background reader thread.
    pub fn new() -> io::Result<Self> {
        let buffer = Arc::new(Mutex::new(VecDeque::new()));
        let event = Arc::new(EventFd::new(EFD_NONBLOCK)?);

        let buf_clone = Arc::clone(&buffer);
        let evt_clone = Arc::clone(&event);

        thread::spawn(move || {
            let mut stdin = io::stdin();
            let mut tmp = [0u8; 64];
            loop {
                match stdin.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
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
        });

        Ok(Self { buffer, event })
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
