//! The codec thread pool (design §9.2).
//!
//! Decoding, composing and encoding never run on tokio workers: they are
//! CPU-bound and take milliseconds, and a stalled tokio worker stalls
//! every call on the node. Jobs run on a fixed set of plain threads
//! instead. The pool's queue is unbounded on purpose — its callers bound
//! themselves: a room has at most one compose job in flight (the clock
//! waits for it), and a source has at most a few decodes queued (it drops
//! frames and asks for a keyframe beyond that), so the queue length is a
//! small multiple of the room count by construction.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

type Job = Box<dyn FnOnce() + Send + 'static>;

enum Message {
    Run(Job),
    Stop,
}

/// A fixed-size pool of codec threads shared by every room on the node.
pub struct CodecPool {
    tx: Mutex<Sender<Message>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    size: usize,
}

impl std::fmt::Debug for CodecPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodecPool")
            .field("size", &self.size)
            .finish()
    }
}

impl CodecPool {
    /// The default size: two cores fewer than the machine has, so tokio
    /// and the kernel keep breathing room, and never fewer than one.
    pub fn default_size() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .saturating_sub(2)
            .max(1)
    }

    /// A pool of `size` threads (at least one).
    pub fn new(size: usize) -> Arc<Self> {
        let size = size.max(1);
        let (tx, rx) = channel::<Message>();
        let rx = Arc::new(Mutex::new(rx));
        let workers = (0..size)
            .map(|i| {
                let rx = Arc::clone(&rx);
                std::thread::Builder::new()
                    .name(format!("forge-codec-{i}"))
                    .spawn(move || worker(rx))
                    .expect("spawn codec thread")
            })
            .collect();
        Arc::new(Self {
            tx: Mutex::new(tx),
            workers: Mutex::new(workers),
            size,
        })
    }

    /// A pool sized by [`default_size`](Self::default_size).
    pub fn with_default_size() -> Arc<Self> {
        Self::new(Self::default_size())
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Queue a job. Returns `false` only after the pool has been shut
    /// down, in which case the job is dropped.
    pub fn submit<F>(&self, job: F) -> bool
    where
        F: FnOnce() + Send + 'static,
    {
        let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        tx.send(Message::Run(Box::new(job))).is_ok()
    }

    /// Stop every worker after the queued jobs finish and wait for them.
    pub fn shutdown(&self) {
        {
            let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner());
            for _ in 0..self.size {
                let _ = tx.send(Message::Stop);
            }
        }
        let workers: Vec<_> = self
            .workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        for w in workers {
            let _ = w.join();
        }
    }
}

impl Drop for CodecPool {
    fn drop(&mut self) {
        // Workers hold only the receiver; telling them to stop lets the
        // process exit cleanly instead of leaking parked threads.
        let tx = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        for _ in 0..self.size {
            let _ = tx.send(Message::Stop);
        }
    }
}

fn worker(rx: Arc<Mutex<Receiver<Message>>>) {
    loop {
        let msg = {
            let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
            guard.recv()
        };
        match msg {
            Ok(Message::Run(job)) => {
                // A panicking codec must not take the thread with it: the
                // job's owner sees the missing result and disables that
                // stream (§13), the pool keeps serving everyone else.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            }
            Ok(Message::Stop) | Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn jobs_run_on_pool_threads_and_a_panic_does_not_kill_a_worker() {
        let pool = CodecPool::new(2);
        let ran = Arc::new(AtomicUsize::new(0));
        assert!(pool.submit(|| panic!("codec blew up")));
        for _ in 0..8 {
            let ran = Arc::clone(&ran);
            assert!(pool.submit(move || {
                assert!(std::thread::current()
                    .name()
                    .unwrap_or("")
                    .starts_with("forge-codec-"));
                ran.fetch_add(1, Ordering::SeqCst);
            }));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while ran.load(Ordering::SeqCst) < 8 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(ran.load(Ordering::SeqCst), 8);
        pool.shutdown();
        assert!(!pool.submit(|| {}), "a stopped pool refuses work");
    }

    #[test]
    fn default_size_leaves_two_cores_and_is_at_least_one() {
        let n = CodecPool::default_size();
        assert!(n >= 1);
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        assert!(n <= cores);
    }
}
