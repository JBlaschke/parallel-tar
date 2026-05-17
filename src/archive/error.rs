// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::archive::mutex::{TryRecvError, RecvTimeoutError, SendError};

use std::fmt;
use std::sync::Arc;
use std::error::Error;
use walkdir::Error as WdError;

#[derive(Debug, Clone)]
pub enum ArchiverError<T> where T: Clone {
    Io(Arc<std::io::Error>),
    WalkdirError(Arc<WdError>),
    TryRecvError(TryRecvError),
    RecvTimeoutError(RecvTimeoutError),
    SendError(SendError<T>),
    LockPoisoned,
    ChannelClosed
}

impl<T: Clone> fmt::Display for ArchiverError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e)               => write!(f, "IO error: {}",          e),
            Self::WalkdirError(e)     => write!(f, "Walkdir error: {}",     e),
            Self::TryRecvError(e)     => write!(f, "TryRecv Error: {}",     e),
            Self::RecvTimeoutError(e) => write!(f, "RecvTimeout Error: {}", e),
            Self::SendError(e)        => write!(f, "Send Error: {}",        e),
            Self::LockPoisoned        => write!(f, "Lock Poisoned"           ),
            Self::ChannelClosed       => write!(f, "Channel Closed"          )
        }
    }
}

impl<T: std::fmt::Debug + Clone> Error for ArchiverError<T> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl<T: Clone> From<std::io::Error> for ArchiverError<T> {
    fn from(e: std::io::Error) -> Self { Self::Io(e.into()) }
}

impl<T: Clone> From<WdError> for ArchiverError<T> {
    fn from(e: WdError) -> Self { Self::WalkdirError(e.into()) }
}

impl<T, S: Clone> From<std::sync::PoisonError<T>> for ArchiverError<S> {
    fn from(_: std::sync::PoisonError<T>) -> Self { ArchiverError::LockPoisoned }
}

impl<T: Clone> From<TryRecvError> for ArchiverError<T> {
    fn from(e: TryRecvError) -> Self { Self::TryRecvError(e) }
}

impl<T: Clone> From<RecvTimeoutError> for ArchiverError<T> {
    fn from(e: RecvTimeoutError) -> Self { Self::RecvTimeoutError(e) }
}

impl<T: Clone> From<SendError<T>> for ArchiverError<T> {
    fn from(e: SendError<T>) -> Self { Self::SendError(e) }
}

type RTAET<T> = Result<T, ArchiverError<T>>;

impl<T: Clone> From<SendError<RTAET<T>>> for ArchiverError<T> {
    fn from(SendError(msg): SendError<RTAET<T>>) -> Self {
        match msg {
            Err(inner) => inner,
            Ok(_) => ArchiverError::ChannelClosed,
        }
    }
}

impl<T: Clone> From<ArchiverError<RTAET<T>>> for ArchiverError<T> {
    fn from(item: ArchiverError<RTAET<T>>) -> Self {
        match item {
            ArchiverError::Io(e) => Self::Io(e),
            ArchiverError::WalkdirError(e) => Self::WalkdirError(e),
            ArchiverError::TryRecvError(e) => Self::TryRecvError(e),
            ArchiverError::RecvTimeoutError(e) => Self::RecvTimeoutError(e),
            ArchiverError::SendError(e) => {
                match e.into_inner() {
                    Ok(value) => ArchiverError::SendError(SendError(value)),
                    Err(inner_error) => inner_error
                }
            }
            ArchiverError::LockPoisoned => Self::LockPoisoned,
            ArchiverError::ChannelClosed => Self::ChannelClosed
        }
    }
}

// ─── Cross-payload conversion trait ──────────────────────────────────────
//
// Globally available like `From`, but does NOT collide with the std
// reflexive `From<T> for T` because it's a different trait. The cost
// is that `?` won't trigger it automatically — you need to call
// `.relabel()` at the boundary. This is the closest we can get to
// Option B on stable Rust.
//
// The blanket below covers EVERY pair of payload types, including
// `T == S` (which is just an identity conversion through the match).

pub trait Relabel<S: Clone> {
    fn relabel(self) -> ArchiverError<S>;
}

impl<T: Clone, S: Clone> Relabel<S> for ArchiverError<T> {
    fn relabel(self) -> ArchiverError<S> {
        match self {
            ArchiverError::Io(e)               => ArchiverError::Io(e),
            ArchiverError::WalkdirError(e)     => ArchiverError::WalkdirError(e),
            ArchiverError::TryRecvError(e)     => ArchiverError::TryRecvError(e),
            ArchiverError::RecvTimeoutError(e) => ArchiverError::RecvTimeoutError(e),
            ArchiverError::SendError(_)        => ArchiverError::ChannelClosed,
            ArchiverError::LockPoisoned        => ArchiverError::LockPoisoned,
            ArchiverError::ChannelClosed       => ArchiverError::ChannelClosed,
        }
    }
}
