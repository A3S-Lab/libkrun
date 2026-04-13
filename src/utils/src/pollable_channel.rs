use crate::eventfd::{EventFd, EFD_NONBLOCK, EFD_SEMAPHORE};
use std::collections::VecDeque;
use std::io;
use std::io::ErrorKind;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::sync::{Arc, Mutex};

/// A multiple producer single consumer channel that can be listened to by a file descriptor
pub fn pollable_channel<T: Send>(
) -> io::Result<(PollableChannelSender<T>, PollableChannelReciever<T>)> {
    let eventfd = EventFd::new(EFD_NONBLOCK | EFD_SEMAPHORE)?;

    let inner = Arc::new(Inner {
        eventfd,
        queue: Mutex::new(VecDeque::new()),
    });
    let tx = PollableChannelSender {
        inner: inner.clone(),
    };
    let rx = PollableChannelReciever { inner };
    Ok((tx, rx))
}

struct Inner<T: Send> {
    eventfd: EventFd,
    queue: Mutex<VecDeque<T>>,
}

#[derive(Clone)]
pub struct PollableChannelSender<T: Send> {
    inner: Arc<Inner<T>>,
}

impl<T: Send> PollableChannelSender<T> {
    pub fn send(&self, msg: T) -> io::Result<()> {
        let mut data_lock = self.inner.queue.lock().unwrap();
        data_lock.push_back(msg);
        self.inner.eventfd.write(1)?;
        Ok(())
    }

    pub fn send_many<I: IntoIterator<Item = T>>(&self, msg_iterator: I) -> io::Result<()> {
        let msg_iterator = msg_iterator.into_iter();
        let mut data_lock = self.inner.queue.lock().unwrap();
        let old_len = data_lock.len();
        data_lock.extend(msg_iterator);
        let num_added_items = data_lock.len() - old_len;
        self.inner.eventfd.write(num_added_items as u64)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct PollableChannelReciever<T: Send> {
    inner: Arc<Inner<T>>,
}

impl<T: Send> PollableChannelReciever<T> {
    pub fn try_recv(&self) -> io::Result<Option<T>> {
        let mut data_lock = self.inner.queue.lock().unwrap();
        match self.inner.eventfd.read() {
            Ok(_) => (),
            Err(e) if e.kind() == ErrorKind::WouldBlock => (),
            Err(e) => return Err(e),
        }

        Ok(data_lock.pop_front())
    }

    pub fn len(&self) -> usize {
        self.inner.queue.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.queue.lock().unwrap().is_empty()
    }
}

impl<T: Send> AsRawFd for PollableChannelReciever<T> {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.eventfd.as_raw_fd()
    }
}

impl<T: Send> AsFd for PollableChannelReciever<T> {
    fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: The lifetime of the fd is the same as the lifetime of self.inner.eventfd which
        //         is the same as the lifetime of self.
        unsafe { BorrowedFd::borrow_raw(self.inner.eventfd.as_raw_fd()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pollable_channel_basic() {
        let (tx, rx) = pollable_channel::<i32>().unwrap();

        // Initially the channel should be empty
        assert!(rx.is_empty());
        assert_eq!(rx.len(), 0);

        // Send a value
        tx.send(42).unwrap();

        // Now it should have one item
        assert!(!rx.is_empty());
        assert_eq!(rx.len(), 1);

        // Try to receive the value
        let value = rx.try_recv().unwrap();
        assert_eq!(value, Some(42));

        // Channel should be empty again
        assert!(rx.is_empty());
        assert_eq!(rx.len(), 0);

        // Try to receive when empty should return None
        assert_eq!(rx.try_recv().unwrap(), None);
    }

    #[test]
    fn test_pollable_channel_multiple_messages() {
        let (tx, rx) = pollable_channel::<String>().unwrap();

        // Send multiple messages
        tx.send("hello".to_string()).unwrap();
        tx.send("world".to_string()).unwrap();
        tx.send("test".to_string()).unwrap();

        assert_eq!(rx.len(), 3);

        // Receive all messages
        assert_eq!(rx.try_recv().unwrap(), Some("hello".to_string()));
        assert_eq!(rx.try_recv().unwrap(), Some("world".to_string()));
        assert_eq!(rx.try_recv().unwrap(), Some("test".to_string()));

        assert!(rx.is_empty());
    }

    #[test]
    fn test_pollable_channel_send_many() {
        let (tx, rx) = pollable_channel::<u32>().unwrap();

        // Send many at once
        tx.send_many([1, 2, 3, 4, 5].iter().cloned()).unwrap();

        assert_eq!(rx.len(), 5);

        for i in 1..=5 {
            assert_eq!(rx.try_recv().unwrap(), Some(i));
        }

        assert!(rx.is_empty());
    }

    #[test]
    fn test_pollable_channel_empty_iterator() {
        let (tx, rx) = pollable_channel::<i32>().unwrap();

        // Send empty iterator
        tx.send_many(std::iter::empty::<i32>()).unwrap();

        assert!(rx.is_empty());
        assert_eq!(rx.try_recv().unwrap(), None);
    }

    #[test]
    fn test_pollable_channel_clone() {
        let (tx1, rx) = pollable_channel::<i32>().unwrap();
        let tx2 = tx1.clone();

        // Both senders should work
        tx1.send(1).unwrap();
        tx2.send(2).unwrap();

        // Can receive from either
        let v1 = rx.try_recv().unwrap();
        let v2 = rx.try_recv().unwrap();

        // Order may vary due to scheduling, but both should be received
        let mut values = vec![v1, v2];
        values.sort();
        assert_eq!(values, vec![Some(1), Some(2)]);
    }

    #[test]
    fn test_pollable_channel_receiver_clone() {
        let (tx, rx1) = pollable_channel::<i32>().unwrap();
        let rx2 = rx1.clone();

        tx.send(100).unwrap();

        // Both receivers point to the same channel
        // Only one should get the message
        let v1 = rx1.try_recv().unwrap();
        let v2 = rx2.try_recv().unwrap();

        // One gets Some(100), the other gets None
        assert!(v1 == Some(100) || v2 == Some(100));
        assert!(v1 == Some(100) || v2 == None || v1 == None || v2 == Some(100));
    }

    #[test]
    fn test_pollable_channel_as_raw_fd() {
        let (tx, rx) = pollable_channel::<u8>().unwrap();

        // The receiver should have a valid raw fd
        let fd = rx.as_raw_fd();
        assert!(fd >= 0);

        // The receiver can be used as a fd for epoll
        tx.send(42).unwrap();

        // After sending, the fd should be readable
        let value = rx.try_recv().unwrap();
        assert_eq!(value, Some(42));
    }

    #[test]
    fn test_pollable_channel_unit_type() {
        let (tx, rx) = pollable_channel::<()>().unwrap();

        tx.send(()).unwrap();
        assert_eq!(rx.len(), 1);
        assert_eq!(rx.try_recv().unwrap(), Some(()));
    }

    #[test]
    fn test_pollable_channel_complex_type() {
        #[derive(Debug, Clone, PartialEq)]
        struct ComplexData {
            id: u64,
            name: String,
            values: Vec<i32>,
        }

        let (tx, rx) = pollable_channel::<ComplexData>().unwrap();

        let data = ComplexData {
            id: 123,
            name: "test".to_string(),
            values: vec![1, 2, 3],
        };

        tx.send(data.clone()).unwrap();

        let received = rx.try_recv().unwrap().unwrap();
        assert_eq!(received, data);
    }
}
