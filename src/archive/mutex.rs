// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::archive::error::ArchiverError;

// Multi-threading
use std::sync::{Arc, Mutex};
cfg_if::cfg_if! {
    if #[cfg(feature = "std")] {
        pub use std::sync::mpsc::{Sender, Receiver};
            use std::sync::mpsc::channel;
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

/// Same as `get_mutex` but returns "default" on a failure to get a mutex lock.
/// The assumption is that a poisoned lock => broken muxt => in some cases we
/// can assume a default value. Ideally one would use the `get_mutex` function
/// and properly handle errors, but this might not work for functions like
/// `collect_expected` which are expected to return a Vector.
fn check_mutex<T: Copy, S: Clone>(mutex: &Arc<Mutex<T>>, default: T) -> T {
    match get_mutex::<T, S>(mutex) {
        Ok(result) => result,
        Err(error) => {
            debug!("Failed to get lock: '{}' => assuming defaul", error);
            default
        }
    }
}

/// Non-blocking (but patient -- i.e. thread sleeps when try_recv fails) attempt
/// to take (try_recv) operation, which aborts when the `completed` semaphore is
/// set to `true`, or when the channel has been disconnected (i.e. every sender
/// has been closed or dropped => no more data can ever arrive).
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
                let datum = {
                    let data = rx.lock()?;
                    data.try_recv()
                };
            } else {
                let datum = rx.try_recv();
            }
        }

        match datum {
            Ok(input) => {
                return Ok(input);
            }
            Err(TryRecvError::Disconnected) => {
                // Every sender has been closed or dropped => no more data can
                // ever arrive, regardless of the `completed` semaphore
                return Err(ArchiverError::ChannelClosed);
            }
            Err(error) => { // TryRecvError::Empty
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
fn collect_expected<T: Clone>(
            ct_expect: usize,
            #[cfg(feature = "std")]
            rx: &Arc<Mutex<Receiver<T>>>,
            #[cfg(not(feature = "std"))]
            rx: &Receiver<T>,
            completed: &Arc<Mutex<bool>>,
            wait: Duration
        ) -> Vec<T> {
    let mut items: Vec<T> = Vec::new();
    let mut ct_recv = 0;
    loop {
        if ct_recv >= ct_expect {
            break;
        }

        cfg_if::cfg_if! {
            if #[cfg(feature = "std")] {
                let datum = match rx.lock() {
                    Ok(data) => data.recv_timeout(wait),
                    Err(error) => {
                        warn!("Failed to get lock: '{}' => stopping", error);
                        break;
                    }
                };
            } else {
                let datum = rx.recv_timeout(wait);
            }
        }

        match datum {
            Ok(result) => {
                debug!("Received {} out of {}", ct_recv, ct_expect);
                items.push(result);
                ct_recv +=1;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Every sender has been closed or dropped, and the channel has
                // been drained => the remaining expected items can never
                // arrive
                warn!(
                    "Channel closed after receiving {} out of {} items => \
                    stopping", ct_recv, ct_expect
                );
                break;
            }
            Err(error) => { // RecvTimeoutError::Timeout
                debug!("recv_timeout failed with: '{}', retrying", error);
                if check_mutex::<bool, ArchiverError<T>>(completed, true) {
                    warn!(
                        "Received premature signal that channel has been \
                        completed => stopping"
                    );
                    break;
                }
            }
        }
    }
    return items;
}

/// Blocking data collection of an unknown number of elements. This function
/// will block until the 'completed' semaphore is set to 'true', or until the
/// channel has been disconnected (every sender closed or dropped) and drained.
/// This is blocking, and will only unblock when all producers are done, at
/// which point 'completed' can be set to 'true'.
fn collect_until_finished<T: Clone>(
            #[cfg(feature = "std")]
            rx: &Arc<Mutex<Receiver<T>>>,
            #[cfg(not(feature = "std"))]
            rx: &Receiver<T>,
            completed: &Arc<Mutex<bool>>,
            wait: Duration
        ) -> Vec<T> {
    let mut items: Vec<T> = Vec::new();
    let mut ct_recv = 0;
    loop {
        // Using try_recv instead of recv_timeout in case data is taking longer
        // to read -- this is meant to be used after the producers have finished
        // (unlike collect_expected, which is meant to be used while producers
        // are running.)

        cfg_if::cfg_if! {
            if #[cfg(feature = "std")] {
                let datum = match rx.lock() {
                    Ok(data) => data.try_recv(),
                    Err(error) => {
                        warn!("Failed to get lock: '{}' => stopping", error);
                        break;
                    }
                };
            } else {
                let datum = rx.try_recv();
            }
        }

        match datum {
            Ok(result) => {
                debug!("Received {}", ct_recv);
                items.push(result);
                ct_recv +=1;
            }
            Err(TryRecvError::Disconnected) => {
                // Every sender has been closed or dropped, and the channel has
                // been drained => all data has been collected
                debug!("Channel closed and drained => all data collected");
                break;
            }
            Err(error) => { // TryRecvError::Empty
                // Wait for the channel to drain first => ensures that there
                // are no more data in the pipe
                debug!(
                    "try_recv failed with: '{}', checking for completion",
                    error
                );
                if check_mutex::<bool, ArchiverError<T>>(completed, true) {
                    debug!(
                        "Received signal that channel has been completed"
                    );
                    break;
                }
                thread::sleep(wait);
            }
        }
    }
    return items;
}

#[derive(Debug, Clone)]
pub struct Pipe<T> where T: Clone{
    /// `None` once this pipe (clone) has been closed. Kept private so that the
    /// sending end can only be reached via `input`/`send`, which check for
    /// closure.
    tx: Option<Sender<T>>,
    #[cfg(feature = "std")]
    rx: Arc<Mutex<Receiver<T>>>,
    #[cfg(not(feature = "std"))]
    rx: Receiver<T>,
    completed: Arc<Mutex<bool>>
}

impl<T: Clone> Pipe<T> {
    pub fn new() -> Self {
        cfg_if::cfg_if! {
            if #[cfg(feature = "std")] {
                let (tx, rx) = channel();
                let rx = Arc::new(Mutex::new(rx));
            } else {
                let (tx, rx) = unbounded();
            }
        }
        Self {
            tx: Some(tx), rx: rx, completed: Arc::new(Mutex::new(false))
        }
    }

    /// Get a handle to the sending end of the pipe. Fails with
    /// `ChannelClosed` if this pipe (clone) has already been closed.
    pub fn input(&self) -> Result<Sender<T>, ArchiverError<T>> {
        match & self.tx {
            Some(tx) => Ok(tx.clone()),
            None => Err(ArchiverError::ChannelClosed)
        }
    }

    #[cfg(feature = "std")]
    pub fn output(&self) -> Arc<Mutex<Receiver<T>>> { self.rx.clone() }
    #[cfg(not(feature = "std"))]
    pub fn output(&self) -> Receiver<T> { self.rx.clone() }

    /// Send `item` down the pipe. Fails with `ChannelClosed` if this pipe
    /// (clone) has already been closed, or with `SendError` if the receiving
    /// end is gone.
    pub fn send(&self, item: T) -> Result<(), ArchiverError<T>> {
        match & self.tx {
            Some(tx) => Ok(tx.send(item)?),
            None => Err(ArchiverError::ChannelClosed)
        }
    }

    /// Close the sending end of _this_ pipe (clone). The channel disconnects
    /// once every clone of this pipe -- and every `Sender` handed out by
    /// `input` -- has been closed or dropped. At that point receivers see
    /// `Disconnected` as soon as the remaining buffered data has been drained.
    pub fn close(&mut self) { self.tx = None; }

    pub fn take_try_many(&self) -> Result<T, ArchiverError<T>> {
        return take_mutex_try_many(
            &self.rx, 100, Duration::from_millis(128), &self.completed
        );
    }

    pub fn set_completed(&self) -> Result<(), ArchiverError<T>> {
        set_mutex(&self.completed, true)
    }

    pub fn get_completed(&self) -> Result<bool, ArchiverError<T>> {
        get_mutex(&self.completed)
    }

    /// Same as `get_completed` but returns "true" on a failure to get a mutex
    /// lock. The assumption is that a poisoned lock => broken channel => as
    /// good as a "completed" channel. Ideally one would use the `get_completed`
    /// function and properly handle errors, but this might not work for
    /// functions like `collect_expected` which are expected to return a Vector.
    pub fn check_completed(&self) -> bool {
        return check_mutex::<bool, ArchiverError<T>>(&self.completed, true);
    }

    pub fn collect_expected(&self, ct_expect: usize) -> Vec<T> {
        return collect_expected(
            ct_expect, &self.rx, &self.completed, Duration::from_millis(4000)
        );
    }

    pub fn collect_until_finished(&self) -> Vec<T> {
        return collect_until_finished(
            &self.rx, &self.completed, Duration::from_millis(4000)
        );
    }
}
