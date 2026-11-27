// SPDX-License-Identifier: AGPL-3.0-or-later

//! Multi-threaded, sharded tar archives.
//!
//! An archive is a directory of shards named `<name>.<worker>.tar[.gz]`,
//! one per worker thread. Work is distributed over the workers through the
//! clonable channel abstraction in [`mutex`].
//!
//! * [`tar`] — create ([`tar::create`]) and extract ([`tar::extract`])
//!   sharded archives.
//! * [`verify`] — rebuild an index directly from the shard streams, without
//!   extracting to disk ([`verify::verify`]).
//! * [`mutex`] — the [`mutex::Pipe`] channel used to fan work out to (and
//!   collect results from) worker threads.
//! * [`fs`] — filesystem helpers for enumeration and tar-header modes.
//! * [`error`] — the [`error::ArchiverError`] type shared by this module.

pub mod fs;
pub mod tar;
pub mod mutex;
pub mod error;
pub mod verify;
mod entry;

/// Join every worker before returning an error so none continues to write in the background.
fn join_workers<T, E>(workers: Vec<std::thread::JoinHandle<Result<T, E>>>) -> Result<Vec<T>, E>
where
    E: From<std::io::Error>,
{
    let results: Vec<_> = workers.into_iter().map(|worker| {
        worker.join().unwrap_or_else(|_| Err(std::io::Error::other("archive worker panicked").into()))
    }).collect();
    results.into_iter().collect()
}
