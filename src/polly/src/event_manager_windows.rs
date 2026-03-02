use std::collections::HashMap;
use std::fmt::Formatter;
use std::io;
use std::sync::{Arc, Mutex};

use utils::epoll::{self, Epoll, EpollEvent};

pub type Result<T> = std::result::Result<T, Error>;
pub type Pollable = i32;

pub enum Error {
    EpollCreate(io::Error),
    Poll(io::Error),
    AlreadyExists(Pollable),
    NotFound(Pollable),
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Error::EpollCreate(err) => write!(f, "Unable to create polling backend: {err}"),
            Error::Poll(err) => write!(f, "Polling backend error: {err}"),
            Error::AlreadyExists(pollable) => {
                write!(f, "A handler for the pollable {pollable} already exists.")
            }
            Error::NotFound(pollable) => {
                write!(f, "A handler for the pollable {pollable} was not found.")
            }
        }
    }
}

pub trait Subscriber {
    fn process(&mut self, event: &EpollEvent, event_manager: &mut EventManager);
    fn interest_list(&self) -> Vec<EpollEvent>;
}

pub struct EventManager {
    epoll: Epoll,
    subscribers: HashMap<Pollable, Arc<Mutex<dyn Subscriber>>>,
    ready_events: Vec<EpollEvent>,
}

impl EventManager {
    const EVENT_BUFFER_SIZE: usize = 128;

    pub fn new() -> Result<EventManager> {
        let epoll = epoll::Epoll::new().map_err(Error::EpollCreate)?;
        Ok(EventManager {
            epoll,
            subscribers: HashMap::new(),
            ready_events: vec![epoll::EpollEvent::default(); EventManager::EVENT_BUFFER_SIZE],
        })
    }

    pub fn subscriber(&self, fd: Pollable) -> Result<Arc<Mutex<dyn Subscriber>>> {
        self.subscribers
            .get(&fd)
            .ok_or(Error::NotFound(fd))
            .cloned()
    }

    pub fn add_subscriber(&mut self, subscriber: Arc<Mutex<dyn Subscriber>>) -> Result<()> {
        let interest_list = subscriber.lock().unwrap().interest_list();
        for event in interest_list {
            self.register(event.data() as i32, event, subscriber.clone())?;
        }
        Ok(())
    }

    pub fn register(
        &mut self,
        pollable: Pollable,
        epoll_event: EpollEvent,
        subscriber: Arc<Mutex<dyn Subscriber>>,
    ) -> Result<()> {
        if self.subscribers.contains_key(&pollable) {
            return Err(Error::AlreadyExists(pollable));
        }

        self.epoll
            .ctl(epoll::ControlOperation::Add, pollable, &epoll_event)
            .map_err(Error::Poll)?;
        self.subscribers.insert(pollable, subscriber);
        Ok(())
    }

    pub fn unregister(&mut self, pollable: Pollable) -> Result<()> {
        match self.subscribers.remove(&pollable) {
            Some(_) => {
                self.epoll
                    .ctl(
                        epoll::ControlOperation::Delete,
                        pollable,
                        &epoll::EpollEvent::default(),
                    )
                    .map_err(Error::Poll)?;
                Ok(())
            }
            None => Err(Error::NotFound(pollable)),
        }
    }

    pub fn modify(&mut self, pollable: Pollable, epoll_event: EpollEvent) -> Result<()> {
        if !self.subscribers.contains_key(&pollable) {
            return Err(Error::NotFound(pollable));
        }

        self.epoll
            .ctl(epoll::ControlOperation::Modify, pollable, &epoll_event)
            .map_err(Error::Poll)?;
        Ok(())
    }

    pub fn is_pollable(&mut self, pollable: Pollable) -> bool {
        self.epoll
            .ctl(
                epoll::ControlOperation::Add,
                pollable,
                &epoll::EpollEvent::default(),
            )
            .is_ok_and(|_| {
                self.epoll
                    .ctl(
                        epoll::ControlOperation::Delete,
                        pollable,
                        &epoll::EpollEvent::default(),
                    )
                    .is_ok()
            })
    }

    pub fn run(&mut self) -> Result<usize> {
        self.run_with_timeout(-1)
    }

    pub fn run_with_timeout(&mut self, milliseconds: i32) -> Result<usize> {
        let event_count = self
            .epoll
            .wait(
                EventManager::EVENT_BUFFER_SIZE,
                milliseconds,
                &mut self.ready_events[..],
            )
            .map_err(Error::Poll)?;

        self.dispatch_events(event_count);
        Ok(event_count)
    }

    fn dispatch_events(&mut self, event_count: usize) {
        for ev_index in 0..event_count {
            let event = self.ready_events[ev_index];
            let pollable = event.fd();

            if let Some(subscriber) = self.subscribers.get(&pollable).cloned() {
                subscriber.lock().unwrap().process(&event, self);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use utils::epoll::EventSet;
    use utils::eventfd::{EventFd, EFD_NONBLOCK};

    struct DummySubscriber {
        event_fd: EventFd,
        processed_in: bool,
    }

    impl DummySubscriber {
        fn new() -> Self {
            Self {
                event_fd: EventFd::new(EFD_NONBLOCK).unwrap(),
                processed_in: false,
            }
        }
    }

    impl Subscriber for DummySubscriber {
        fn process(&mut self, event: &EpollEvent, _event_manager: &mut EventManager) {
            if EventSet::from_bits_truncate(event.events()) == EventSet::IN
                && event.fd() == self.event_fd.as_raw_fd()
            {
                self.processed_in = true;
                self.event_fd.read().unwrap();
            }
        }

        fn interest_list(&self) -> Vec<EpollEvent> {
            vec![EpollEvent::new(
                EventSet::IN,
                self.event_fd.as_raw_fd() as u64,
            )]
        }
    }

    #[test]
    fn test_dispatch_in_event() {
        let mut event_manager = EventManager::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));
        let event_fd = dummy_subscriber
            .lock()
            .unwrap()
            .event_fd
            .try_clone()
            .unwrap();

        event_manager
            .add_subscriber(dummy_subscriber.clone())
            .unwrap();

        event_fd.write(1).unwrap();
        let count = event_manager.run_with_timeout(100).unwrap();

        assert_eq!(count, 1);
        assert!(dummy_subscriber.lock().unwrap().processed_in);
    }

    #[test]
    fn test_unregister_stops_events() {
        let mut event_manager = EventManager::new().unwrap();
        let dummy_subscriber = Arc::new(Mutex::new(DummySubscriber::new()));
        let event_fd = dummy_subscriber
            .lock()
            .unwrap()
            .event_fd
            .try_clone()
            .unwrap();
        let pollable = dummy_subscriber.lock().unwrap().event_fd.as_raw_fd();

        event_manager
            .add_subscriber(dummy_subscriber.clone())
            .unwrap();

        event_manager.unregister(pollable).unwrap();
        event_fd.write(1).unwrap();

        let count = event_manager.run_with_timeout(10).unwrap();
        assert_eq!(count, 0);
    }
}
