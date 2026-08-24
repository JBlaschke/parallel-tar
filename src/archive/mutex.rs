// SPDX-License-Identifier: AGPL-3.0-or-later

//! [`Pipe`]: the clonable channel that connects the main thread to archive
//! workers.
//!
//! A `Pipe` bundles a multi-producer channel with a shared `completed` flag,
//! so consumers can distinguish "no data *yet*" from "no data will ever
//! come". Collection ends either when the flag is set or when the channel
//! disconnects (every sender closed or dropped) and is drained.
//!
//! Two interchangeable back ends are selected at compile time: the default
//! is [`flume`] (multi-consumer, lock-free receive); building with
//! `--features std` swaps in `std::sync::mpsc` with the single receiver
//! shared behind a mutex.

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

/// Set the value behind a shared mutex, holding the lock only for the
/// assignment.
pub fn set_mutex<T: Copy, S: Clone>(
            mutex: &Arc<Mutex<T>>, val: T
        ) -> Result<(), ArchiverError<S>> {
    let mut lock = mutex.lock()?;
    * lock = val;
    drop(lock);
    Ok(())
}

/// Copy the value out from behind a shared mutex, holding the lock only for
/// the read.
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

/// A clonable multi-producer channel with a shared "completed" flag.
///
/// Clones share the underlying channel and flag; each clone owns its own
/// sending handle. The channel disconnects once every clone (and every
/// `Sender` handed out by [`input`](Self::input)) has been closed or
/// dropped — the usual pattern is: hand clones to workers, [`send`](Self::send)
/// all the work, [`close`](Self::close) the producing handle, then collect
/// results with [`collect_expected`](Self::collect_expected) or
/// [`collect_until_finished`](Self::collect_until_finished).
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
    /// Create a fresh, open pipe with the `completed` flag unset.
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

    /// Get a handle to the receiving end of the pipe (mutex-guarded in
    /// `std` mode, a plain multi-consumer receiver in flume mode).
    #[cfg(feature = "std")]
    pub fn output(&self) -> Arc<Mutex<Receiver<T>>> { self.rx.clone() }
    /// Get a handle to the receiving end of the pipe (mutex-guarded in
    /// `std` mode, a plain multi-consumer receiver in flume mode).
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

    /// Receive one item, retrying (with a short sleep) on an empty channel.
    /// Gives up with `TryRecvError` after ~100 retries or once the
    /// `completed` flag is set; returns `ChannelClosed` if the channel
    /// disconnects.
    pub fn take_try_many(&self) -> Result<T, ArchiverError<T>> {
        return take_mutex_try_many(
            &self.rx, 100, Duration::from_millis(128), &self.completed
        );
    }

    /// Set the shared `completed` flag, signalling consumers (on this pipe
    /// and every clone) that no further data should be waited for.
    pub fn set_completed(&self) -> Result<(), ArchiverError<T>> {
        set_mutex(&self.completed, true)
    }

    /// Read the shared `completed` flag.
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

    /// Block until `ct_expect` items have been received, the channel
    /// disconnects and drains, or the `completed` flag is observed on a
    /// receive timeout. Returns whatever was collected; a short count is
    /// logged as a warning, not an error.
    pub fn collect_expected(&self, ct_expect: usize) -> Vec<T> {
        return collect_expected(
            ct_expect, &self.rx, &self.completed, Duration::from_millis(4000)
        );
    }

    /// Drain the channel of an unknown number of items, blocking until the
    /// channel disconnects (and is drained) or the `completed` flag is set.
    /// Meant to be called after the producers have finished.
    pub fn collect_until_finished(&self) -> Vec<T> {
        return collect_until_finished(
            &self.rx, &self.completed, Duration::from_millis(4000)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Everything below uses only the public `Pipe` API, so these tests are
    // valid for both the flume and the `feature = "std"` (mpsc) back ends.

    #[test]
    fn send_and_take_roundtrip() {
        let pipe = Pipe::<u32>::new();
        pipe.send(42).unwrap();
        assert_eq!(pipe.take_try_many().unwrap(), 42);
    }

    #[test]
    fn close_disables_this_handles_sender() {
        let mut pipe = Pipe::<u32>::new();
        pipe.close();
        // Both ways of reaching the sending end must report closure
        assert!(matches!(pipe.send(1), Err(ArchiverError::ChannelClosed)));
        assert!(matches!(pipe.input(), Err(ArchiverError::ChannelClosed)));
    }

    #[test]
    fn clone_keeps_channel_open_until_all_handles_closed() {
        let mut pipe = Pipe::<u32>::new();
        let mut clone = pipe.clone();

        // Closing one handle must not close the channel: the clone's sender
        // is still alive and data still flows
        pipe.close();
        assert!(pipe.send(1).is_err());
        clone.send(2).unwrap();
        assert_eq!(pipe.take_try_many().unwrap(), 2);

        // Once the last handle closes, receivers must see the disconnect
        clone.close();
        assert!(matches!(
            pipe.take_try_many(), Err(ArchiverError::ChannelClosed)
        ));
    }

    #[test]
    fn input_sender_keeps_channel_alive() {
        let mut pipe = Pipe::<u32>::new();
        let tx = pipe.input().unwrap();

        // The `Sender` handed out by `input` counts as an open handle
        pipe.close();
        tx.send(7).unwrap();
        assert_eq!(pipe.take_try_many().unwrap(), 7);

        // ... and dropping it disconnects the channel
        drop(tx);
        assert!(matches!(
            pipe.take_try_many(), Err(ArchiverError::ChannelClosed)
        ));
    }

    #[test]
    fn buffered_data_survives_close() {
        // Closing must not lose data that is already in the channel
        let mut pipe = Pipe::<u32>::new();
        pipe.send(1).unwrap();
        pipe.send(2).unwrap();
        pipe.close();
        assert_eq!(pipe.take_try_many().unwrap(), 1);
        assert_eq!(pipe.take_try_many().unwrap(), 2);
        assert!(matches!(
            pipe.take_try_many(), Err(ArchiverError::ChannelClosed)
        ));
    }

    #[test]
    fn collect_until_finished_ends_on_disconnect() {
        // The producers never touch the `completed` semaphore -- the channel
        // disconnect alone must end the collection. Before the `close` fix
        // this would hang forever.
        let mut pipe = Pipe::<u32>::new();

        let mut producers = Vec::new();
        for t in 0..4u32 {
            let loc = pipe.clone();
            producers.push(thread::spawn(move || {
                for i in 0..25u32 {
                    loc.send(t * 25 + i).unwrap();
                }
                // `loc` (and its sender) dropped here
            }));
        }
        for p in producers {
            p.join().unwrap();
        }

        pipe.close(); // last handle => channel disconnects
        let items = pipe.collect_until_finished();
        assert_eq!(items.len(), 100);
    }

    #[test]
    fn collect_until_finished_ends_on_completed_flag() {
        // The original semaphore-based termination must keep working even
        // while the pipe still holds an open sender
        let pipe = Pipe::<u32>::new();
        pipe.send(1).unwrap();
        pipe.send(2).unwrap();
        pipe.set_completed().unwrap();
        let items = pipe.collect_until_finished();
        assert_eq!(items, vec![1, 2]);
    }

    #[test]
    fn collect_expected_receives_from_concurrent_producers() {
        let pipe = Pipe::<u32>::new();

        let mut producers = Vec::new();
        for t in 0..4u32 {
            let loc = pipe.clone();
            producers.push(thread::spawn(move || {
                for i in 0..10u32 {
                    loc.send(t * 10 + i).unwrap();
                }
            }));
        }

        // Blocks until all 40 items have arrived -- `completed` is never set
        let items = pipe.collect_expected(40);
        assert_eq!(items.len(), 40);

        for p in producers {
            p.join().unwrap();
        }
    }

    #[test]
    fn collect_expected_stops_early_when_channel_closed() {
        // Expecting more items than the (closed) channel will ever deliver
        // must return the drained items instead of blocking forever
        let mut pipe = Pipe::<u32>::new();
        pipe.send(1).unwrap();
        pipe.send(2).unwrap();
        pipe.send(3).unwrap();
        pipe.close();
        let items = pipe.collect_expected(5);
        assert_eq!(items, vec![1, 2, 3]);
    }

    #[test]
    fn completed_flag_roundtrip() {
        let pipe = Pipe::<u32>::new();
        assert!(! pipe.get_completed().unwrap());
        assert!(! pipe.check_completed());
        pipe.set_completed().unwrap();
        assert!(pipe.get_completed().unwrap());
        assert!(pipe.check_completed());
        // The flag is shared state => visible through clones
        assert!(pipe.clone().check_completed());
    }
}
