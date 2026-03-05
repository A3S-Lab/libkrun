use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use bitflags::bitflags;
use log::error;
use windows_sys::Win32::Foundation::{HANDLE, WAIT_FAILED, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{WaitForMultipleObjects, INFINITE};

use super::eventfd;

pub type RawFd = i32;

#[repr(i32)]
pub enum ControlOperation {
    Add,
    Modify,
    Delete,
}

bitflags! {
    pub struct EventSet: u32 {
        const IN = 0b0000_0001;
        const OUT = 0b0000_0010;
        const ERROR = 0b0000_0100;
        const READ_HANG_UP = 0b0000_1000;
        const EDGE_TRIGGERED = 0b0001_0000;
        const HANG_UP = 0b0010_0000;
        const PRIORITY = 0b0100_0000;
        const WAKE_UP = 0b1000_0000;
        const ONE_SHOT = 0b0001_0000_0000;
        const EXCLUSIVE = 0b0010_0000_0000;
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct EpollEvent {
    events: u32,
    data: u64,
}

impl EpollEvent {
    pub fn new(events: EventSet, data: u64) -> Self {
        Self {
            events: events.bits(),
            data,
        }
    }

    pub fn events(&self) -> u32 {
        self.events
    }

    pub fn event_set(&self) -> EventSet {
        EventSet::from_bits_truncate(self.events)
    }

    pub fn data(&self) -> u64 {
        self.data
    }

    pub fn fd(&self) -> RawFd {
        self.data as RawFd
    }
}

#[derive(Debug)]
struct Registration {
    event: EpollEvent,
    handle: HANDLE,
}

// SAFETY: Windows HANDLE is safe to send between threads (it's an opaque kernel object handle)
unsafe impl Send for Registration {}
unsafe impl Sync for Registration {}

#[derive(Debug)]
struct EpollInner {
    registrations: HashMap<RawFd, Registration>,
}

#[derive(Clone, Debug)]
pub struct Epoll {
    inner: Arc<Mutex<EpollInner>>,
}

impl Epoll {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            inner: Arc::new(Mutex::new(EpollInner {
                registrations: HashMap::new(),
            })),
        })
    }

    pub fn ctl(
        &self,
        operation: ControlOperation,
        fd: RawFd,
        event: &EpollEvent,
    ) -> io::Result<()> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;

        match operation {
            ControlOperation::Add => {
                if !eventfd::is_eventfd(fd) {
                    return Err(io::Error::from(io::ErrorKind::InvalidInput));
                }
                if inner.registrations.contains_key(&fd) {
                    return Err(io::Error::from(io::ErrorKind::AlreadyExists));
                }

                let handle = eventfd::get_event_handle(fd)?;
                inner.registrations.insert(
                    fd,
                    Registration {
                        event: *event,
                        handle,
                    },
                );
            }
            ControlOperation::Modify => {
                if let Some(reg) = inner.registrations.get_mut(&fd) {
                    reg.event = *event;
                } else {
                    return Err(io::Error::from(io::ErrorKind::NotFound));
                }
            }
            ControlOperation::Delete => {
                if inner.registrations.remove(&fd).is_none() {
                    return Err(io::Error::from(io::ErrorKind::NotFound));
                }
            }
        }

        Ok(())
    }

    pub fn wait(
        &self,
        max_events: usize,
        timeout: i32,
        events: &mut [EpollEvent],
    ) -> io::Result<usize> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;

        if inner.registrations.is_empty() {
            return Ok(0);
        }

        let mut handles = Vec::with_capacity(inner.registrations.len());
        for reg in inner.registrations.values() {
            handles.push(reg.handle);
        }

        if handles.len() > 64 {
            error!(
                "epoll(windows): Too many registered fds ({} > 64). \
                 Windows WaitForMultipleObjects has a hard limit of 64 handles. \
                 Consider reducing the number of concurrent devices or connections.",
                handles.len()
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Too many registered fds ({} > 64)", handles.len()),
            ));
        }

        drop(inner);

        let timeout_ms = if timeout < 0 {
            INFINITE
        } else {
            timeout as u32
        };

        let wait_result = unsafe {
            WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout_ms)
        };

        if wait_result == WAIT_FAILED {
            let err = io::Error::last_os_error();
            error!("epoll(windows): WaitForMultipleObjects failed: {}", err);
            return Err(err);
        }

        if wait_result == WAIT_TIMEOUT {
            // Timeout is not an error - return 0 events
            return Ok(0);
        }

        let inner = self
            .inner
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;

        let mut count = 0;
        for (&fd, reg) in inner.registrations.iter() {
            if count >= max_events || count >= events.len() {
                break;
            }

            if eventfd::is_readable(fd)? {
                events[count] = reg.event;
                count += 1;
            }
        }

        Ok(count)
    }
}

impl Default for Epoll {
    fn default() -> Self {
        Self::new().expect("Failed to create Epoll")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventfd::{EventFd, EFD_NONBLOCK};

    #[test]
    fn test_event_ops() {
        let mut event = EpollEvent::default();
        assert_eq!(event.events(), 0);
        assert_eq!(event.data(), 0);

        event = EpollEvent::new(EventSet::IN, 2);
        assert_eq!(event.events(), EventSet::IN.bits());
        assert_eq!(event.event_set(), EventSet::IN);
        assert_eq!(event.data(), 2);
        assert_eq!(event.fd(), 2);
    }

    #[test]
    fn test_ctl_add_non_eventfd_fails() {
        let epoll = Epoll::new().unwrap();
        let event = EpollEvent::new(EventSet::IN, 123);
        let res = epoll.ctl(ControlOperation::Add, 123, &event);
        assert!(matches!(res, Err(err) if err.kind() == io::ErrorKind::InvalidInput));
    }

    #[test]
    fn test_wait_timeout() {
        let epoll = Epoll::new().unwrap();
        let event_fd = EventFd::new(EFD_NONBLOCK).unwrap();
        let event = EpollEvent::new(EventSet::IN, event_fd.as_raw_fd() as u64);

        epoll
            .ctl(ControlOperation::Add, event_fd.as_raw_fd(), &event)
            .unwrap();

        let mut ready_events = vec![EpollEvent::default(); 8];
        let count = epoll.wait(8, 0, &mut ready_events).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_wait_readable_eventfd() {
        let epoll = Epoll::new().unwrap();
        let event_fd = EventFd::new(EFD_NONBLOCK).unwrap();
        event_fd.write(1).unwrap();

        let event = EpollEvent::new(EventSet::IN, event_fd.as_raw_fd() as u64);
        epoll
            .ctl(ControlOperation::Add, event_fd.as_raw_fd(), &event)
            .unwrap();

        let mut ready_events = vec![EpollEvent::default(); 8];
        let count = epoll.wait(8, 10, &mut ready_events).unwrap();
        assert_eq!(count, 1);
        assert_eq!(ready_events[0].data(), event_fd.as_raw_fd() as u64);
        assert_eq!(ready_events[0].event_set(), EventSet::IN);
    }
}
