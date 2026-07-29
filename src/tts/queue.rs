//! A bounded, drop-oldest work queue.
//!
//! A chat flood is the normal failure mode for speech: messages arrive faster
//! than any engine can render them, and a paid engine bills for every one. The
//! reference implementation queues them without limit, so a busy stream leaves
//! the speaker minutes deep in stale messages.
//!
//! Dropping the *oldest* is what keeps speech current, and it is why this
//! isn't a `crossbeam_channel::bounded`: a channel sender cannot discard what
//! is already queued, only refuse to add more, which keeps the stale messages
//! and throws away the fresh ones.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

pub struct Queue<T> {
    inner: Arc<(Mutex<State<T>>, Condvar)>,
    capacity: usize,
}

struct State<T> {
    items: VecDeque<T>,
    closed: bool,
    /// Counts messages discarded to make room, for logging.
    dropped: u64,
}

impl<T> Clone for Queue<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            capacity: self.capacity,
        }
    }
}

impl<T> Queue<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new((
                Mutex::new(State {
                    items: VecDeque::with_capacity(capacity),
                    closed: false,
                    dropped: 0,
                }),
                Condvar::new(),
            )),
            capacity: capacity.max(1),
        }
    }

    /// Adds `item`, discarding the oldest queued item if the queue is full.
    /// Never blocks — this is called from the UI thread's event pump.
    pub fn push(&self, item: T) {
        let (lock, ready) = &*self.inner;
        let Ok(mut state) = lock.lock() else {
            return;
        };
        if state.closed {
            return;
        }
        while state.items.len() >= self.capacity {
            state.items.pop_front();
            state.dropped += 1;
        }
        state.items.push_back(item);
        ready.notify_one();
    }

    /// Blocks until an item is available, or the queue closes.
    pub fn pop(&self) -> Option<T> {
        let (lock, ready) = &*self.inner;
        let mut state = lock.lock().ok()?;
        loop {
            if let Some(item) = state.items.pop_front() {
                return Some(item);
            }
            if state.closed {
                return None;
            }
            state = ready.wait(state).ok()?;
        }
    }

    /// Wakes every waiting consumer and refuses further pushes. Pending items
    /// are still drained, so an utterance already queued at shutdown is not
    /// silently lost.
    pub fn close(&self) {
        let (lock, ready) = &*self.inner;
        if let Ok(mut state) = lock.lock() {
            state.closed = true;
            ready.notify_all();
        }
    }

    /// How many items have been discarded to make room, in total.
    pub fn dropped(&self) -> u64 {
        let (lock, _) = &*self.inner;
        lock.lock().map(|state| state.dropped).unwrap_or(0)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        let (lock, _) = &*self.inner;
        lock.lock().map(|state| state.items.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn items_come_back_in_order() {
        let queue = Queue::new(4);
        queue.push(1);
        queue.push(2);
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
    }

    /// The point of the whole module: under overload the *newest* messages
    /// survive, so speech stays current.
    #[test]
    fn overflow_discards_the_oldest_and_keeps_the_newest() {
        let queue = Queue::new(3);
        for value in 0..10 {
            queue.push(value);
        }
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.pop(), Some(7));
        assert_eq!(queue.pop(), Some(8));
        assert_eq!(queue.pop(), Some(9));
        assert_eq!(queue.dropped(), 7);
    }

    #[test]
    fn pushing_never_blocks_however_full_it_is() {
        let queue = Queue::new(2);
        // No consumer at all; this must still return promptly.
        for value in 0..10_000 {
            queue.push(value);
        }
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn closing_wakes_a_blocked_consumer() {
        let queue = Queue::<u32>::new(4);
        let consumer = {
            let queue = queue.clone();
            std::thread::spawn(move || queue.pop())
        };
        std::thread::sleep(Duration::from_millis(50));
        queue.close();
        assert_eq!(consumer.join().unwrap(), None);
    }

    /// Anything already queued at shutdown should still be spoken.
    #[test]
    fn closing_drains_what_is_already_queued_before_ending() {
        let queue = Queue::new(4);
        queue.push(1);
        queue.push(2);
        queue.close();
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn pushing_after_close_is_ignored() {
        let queue = Queue::new(4);
        queue.close();
        queue.push(1);
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn a_consumer_thread_sees_every_item_a_producer_sends() {
        let queue = Queue::new(1024);
        let consumer = {
            let queue = queue.clone();
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                while let Some(value) = queue.pop() {
                    seen.push(value);
                }
                seen
            })
        };
        for value in 0..500u32 {
            queue.push(value);
        }
        queue.close();
        let seen = consumer.join().unwrap();
        assert_eq!(seen, (0..500).collect::<Vec<_>>());
        assert_eq!(queue.dropped(), 0);
    }
}
