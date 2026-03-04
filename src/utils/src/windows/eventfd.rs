use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, ResetEvent, SetEvent, WaitForSingleObject, INFINITE,
};

pub const EFD_NONBLOCK: i32 = 1;
pub const EFD_SEMAPHORE: i32 = 2;

#[derive(Debug)]
struct EventState {
    value: u64,
    nonblock: bool,
    semaphore: bool,
}

#[derive(Debug)]
struct SharedEventFd {
    id: i32,
    state: Mutex<EventState>,
    event_handle: HANDLE,
}

impl Drop for SharedEventFd {
    fn drop(&mut self) {
        if !self.event_handle.is_null() {
            unsafe {
                CloseHandle(self.event_handle);
            }
        }
    }
}

// SAFETY: Windows HANDLEs for event objects are valid across threads.
unsafe impl Send for SharedEventFd {}
unsafe impl Sync for SharedEventFd {}

static NEXT_EVENTFD_ID: AtomicI32 = AtomicI32::new(1000);
static EVENTFD_REGISTRY: OnceLock<Mutex<HashMap<i32, Weak<SharedEventFd>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<i32, Weak<SharedEventFd>>> {
    EVENTFD_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register(shared: &Arc<SharedEventFd>) -> io::Result<()> {
    registry()
        .lock()
        .map_err(|_| io::Error::from(io::ErrorKind::Other))?
        .insert(shared.id, Arc::downgrade(shared));
    Ok(())
}

fn lookup(fd: i32) -> io::Result<Option<Arc<SharedEventFd>>> {
    let mut reg = registry()
        .lock()
        .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
    let Some(weak) = reg.get(&fd).cloned() else {
        return Ok(None);
    };
    if let Some(shared) = weak.upgrade() {
        Ok(Some(shared))
    } else {
        reg.remove(&fd);
        Ok(None)
    }
}

pub(crate) fn is_eventfd(fd: i32) -> bool {
    lookup(fd).ok().flatten().is_some()
}

pub(crate) fn is_readable(fd: i32) -> io::Result<bool> {
    let Some(shared) = lookup(fd)? else {
        return Ok(false);
    };
    let state = shared
        .state
        .lock()
        .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
    Ok(state.value > 0)
}

pub(crate) fn get_event_handle(fd: i32) -> io::Result<HANDLE> {
    let Some(shared) = lookup(fd)? else {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    };
    Ok(shared.event_handle)
}

#[derive(Debug)]
pub struct EventFd {
    shared: Arc<SharedEventFd>,
}

impl EventFd {
    pub fn new(flag: i32) -> io::Result<EventFd> {
        let event_handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event_handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        let id = NEXT_EVENTFD_ID.fetch_add(1, Ordering::Relaxed);
        let shared = Arc::new(SharedEventFd {
            id,
            state: Mutex::new(EventState {
                value: 0,
                nonblock: (flag & EFD_NONBLOCK) != 0,
                semaphore: (flag & EFD_SEMAPHORE) != 0,
            }),
            event_handle,
        });
        register(&shared)?;
        Ok(EventFd { shared })
    }

    pub fn write(&self, v: u64) -> io::Result<()> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
        state.value = state.value.saturating_add(v);

        unsafe {
            if SetEvent(self.shared.event_handle) == 0 {
                return Err(io::Error::last_os_error());
            }
        }

        Ok(())
    }

    pub fn read(&self) -> io::Result<u64> {
        loop {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| io::Error::from(io::ErrorKind::Other))?;

            if state.value > 0 {
                let result = if state.semaphore {
                    state.value -= 1;
                    1
                } else {
                    let value = state.value;
                    state.value = 0;
                    value
                };

                if state.value == 0 {
                    unsafe {
                        ResetEvent(self.shared.event_handle);
                    }
                }

                return Ok(result);
            }

            if state.nonblock {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }

            drop(state);

            let wait_result = unsafe { WaitForSingleObject(self.shared.event_handle, INFINITE) };

            if wait_result != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
        }
    }

    pub fn try_clone(&self) -> io::Result<EventFd> {
        Ok(EventFd {
            shared: self.shared.clone(),
        })
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.shared.id
    }

    /// Returns the underlying Win32 event HANDLE.
    ///
    /// The returned value has type `windows_sys::Win32::Foundation::HANDLE`
    /// (a `*mut c_void`).
    ///
    /// Callers using the `windows` crate can convert via:
    ///   `windows::Win32::Foundation::HANDLE(raw as isize)`
    pub fn as_raw_handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.shared.event_handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_write() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        evt.write(55).unwrap();
        assert_eq!(evt.read().unwrap(), 55);
    }

    #[test]
    fn test_read_nothing_nonblock() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        let res = evt.read();
        assert!(matches!(res, Err(err) if err.kind() == io::ErrorKind::WouldBlock));
    }

    #[test]
    fn test_semaphore_mode() {
        let evt = EventFd::new(EFD_NONBLOCK | EFD_SEMAPHORE).unwrap();
        evt.write(3).unwrap();

        assert_eq!(evt.read().unwrap(), 1);
        assert_eq!(evt.read().unwrap(), 1);
        assert_eq!(evt.read().unwrap(), 1);

        let res = evt.read();
        assert!(matches!(res, Err(err) if err.kind() == io::ErrorKind::WouldBlock));
    }

    #[test]
    fn test_clone() {
        let evt = EventFd::new(EFD_NONBLOCK).unwrap();
        let evt_clone = evt.try_clone().unwrap();

        evt.write(923).unwrap();
        assert_eq!(evt_clone.read().unwrap(), 923);
    }
}
