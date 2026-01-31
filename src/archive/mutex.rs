use crate::archive::error::ArchiverError;

// Multi-threading
use std::sync::{Arc, Mutex};
cfg_if::cfg_if! {
    if #[cfg(feature = "std")] {
        pub use std::sync::mpsc::{Sender, Receiver};
            use std::sync::mpsc::channel
        pub use std::sync::mpsc::{TryRecvError, RecvTimeoutError, SendError};
    } else {
        pub use flume::{Sender, Receiver};
            use flume::unbounded;
        pub use flume::{TryRecvError, RecvTimeoutError, SendError};
    }
}
use std::{thread, time::Duration};
// Logging
use log::{debug, warn};

pub fn set_mutex<T: Copy, S: Clone>(
            mutex: &Arc<Mutex<T>>, val: T
        ) -> Result<(), ArchiverError<S>> {
    let mut lock = mutex.lock()?;
    * lock = val;
    drop(lock);
    Ok(())
}

pub fn get_mutex<T: Copy, S: Clone>(
            mutex: &Arc<Mutex<T>>
        ) -> Result<T, ArchiverError<S>> {
    let lock = mutex.lock()?;
    let val = * lock;
    drop(lock);
    return Ok(val);
}

/// Non-blocking (but patient -- i.e. thread sleeps when try_recv fails) attempt
/// to take (try_recv) operation, which aborts when the `completed` semaphore is
/// set to `true`
fn take_mutex_try_many<T: Clone>(
            #[cfg(feature = "std")]
            rx: &Arc<Mutex<Receiver<T>>>,
            #[cfg(not(feature = "std"))]
            rx: &Receiver<T>,
            max_try: u32, wait: Duration,
            completed: &Arc<Mutex<bool>>
        ) -> Result<T, ArchiverError<T>>  {
    let mut ct = 0;
    loop {
        // In std mode: grab lock the the guard mutex, and take data from
        // channel, in flume mode: just grab the data and let flume handle the
        // MC part

        cfg_if::cfg_if! {
            if #[cfg(feature = "std")] {
                let data = rx.lock()?;
                let datum = data.try_recv();
                drop(data);
            } else {
                let datum = rx;
            }
        }

        match datum.try_recv() {
            Ok(input) => {
                return Ok(input);
            }
            Err(error) => {
                if (ct > max_try) || get_mutex::<bool, T>(&completed)? {
                    return Err(error.into());
                }
                ct += 1;
                thread::sleep(wait);
            }
        }
    }
}

/// Blocking data collection of a known number of elements. This function will
/// block if expecting more data than there are.
fn collect_expected<T>(
            ct_expect: usize,
            #[cfg(feature = "std")]
            rx: &Arc<Mutex<Receiver<T>>>,
            #[cfg(not(feature = "std"))]
            rx: &Receiver<T>,
            wait: Duration
        ) -> Vec<T> {
    let mut items: Vec<T> = Vec::new();
    let mut ct_recv = 0;
    loop {
        if ct_recv >= ct_expect {
            break;
        }
        match rx.recv_timeout(wait) {
            Ok(result) => {
                debug!("Received {} out of {}", ct_recv, ct_expect);
                items.push(result);
                ct_recv +=1;
            }
            Err(error) => {
                warn!("recv_timeout failed with: '{}', retrying", error);
            }
        }
    }
    return items;
}

#[derive(Debug, Clone)]
pub struct Pipe<T> where T: Clone{
    pub tx: Sender<T>,
    #[cfg(feature = "std")]
    pub rx: Arc<Mutex<Receiver<T>>>,
    #[cfg(not(feature = "std"))]
    pub rx: Receiver<T>,
    pub completed: Arc<Mutex<bool>>
}

impl<T: Clone> Pipe<T> {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx: tx, rx: rx, completed: Arc::new(Mutex::new(false))
        }
    }

    pub fn input(&self) -> Sender<T> { self.tx.clone() }

    pub fn output(&self) -> Receiver<T> { self.rx.clone() }

    pub fn take_try_many(&self) -> Result<T, ArchiverError<T>> {
        return take_mutex_try_many(
            &self.output(), 100, Duration::from_millis(128), &self.completed
        );
    }

    pub fn set_completed(&self) -> Result<(), ArchiverError<T>> {
        set_mutex(&self.completed, true)
    }

    pub fn get_completed(&self) -> Result<bool, ArchiverError<T>>  {
        get_mutex(&self.completed)
    }

    pub fn collect_expected(&self, ct_expect: usize) -> Vec<T> {
        return collect_expected(
            ct_expect, &self.rx, Duration::from_millis(4000)
        );
    }
}
