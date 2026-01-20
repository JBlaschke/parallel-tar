// Filesystem-related error types (imported elsewhere)
use crate::archive::mutex::{TryRecvError, RecvTimeoutError, SendError};

// Stdlib
use std::fmt;
use std::sync::Arc;
use std::error::Error;
// Filesystem-related error types
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
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.into())
    }
}

impl<T: Clone> From<WdError> for ArchiverError<T> {
    fn from(e: WdError) -> Self {
        Self::WalkdirError(e.into())
    }
}

impl<T, S: Clone> From<std::sync::PoisonError<T>> for ArchiverError<S> {
    fn from(_: std::sync::PoisonError<T>) -> Self {
        ArchiverError::LockPoisoned
    }
}

impl<T: Clone> From<TryRecvError> for ArchiverError<T> {
    fn from(e: TryRecvError) -> Self {
        Self::TryRecvError(e)
    }
}

impl<T: Clone> From<RecvTimeoutError> for ArchiverError<T> {
    fn from(e: RecvTimeoutError) -> Self {
        Self::RecvTimeoutError(e)
    }
}

impl<T: Clone> From<SendError<T>> for ArchiverError<T> {
    fn from(e: SendError<T>) -> Self {
        Self::SendError(e)
    }
}

type Msg<T> = Result<T, ArchiverError<T>>;

impl<T: Clone> From<SendError<Msg<T>>> for ArchiverError<T> {
    fn from(SendError(msg): SendError<Msg<T>>) -> Self {
        match msg {
            // recover the original ArchiverError<T>
            Err(inner) => inner,
            // receiver dropped while sending an Ok(_)
            Ok(_) => ArchiverError::ChannelClosed, 
        }
    }
}
