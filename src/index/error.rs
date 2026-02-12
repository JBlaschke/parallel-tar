// SPDX-License-Identifier: AGPL-3.0-or-later
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum IndexerError {
    Json(serde_json::Error),
    IdxEncode(rmp_serde::encode::Error),
    IdxDecode(rmp_serde::decode::Error),
    Io(std::io::Error),
    InvalidPath(String),
    NotFound(String),
    LockPoisoned
}

impl fmt::Display for IndexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e)        => write!(f, "JSON error: {}",       e),
            Self::IdxEncode(e)   => write!(f, "RMP encode error: {}", e),
            Self::IdxDecode(e)   => write!(f, "RMP decode error: {}", e),
            Self::Io(e)          => write!(f, "IO error: {}",         e),
            Self::InvalidPath(e) => write!(f, "Invalid path: {}",     e),
            Self::NotFound(e)    => write!(f, "Node not found: {}",   e),
            Self::LockPoisoned   => write!(f, "Lock Poisoned")
        }
    }
}

impl Error for IndexerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for IndexerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for IndexerError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<rmp_serde::encode::Error> for IndexerError {
    fn from(e: rmp_serde::encode::Error) -> Self {
        Self::IdxEncode(e)
    }
}

impl From<rmp_serde::decode::Error> for IndexerError {
    fn from(e: rmp_serde::decode::Error) -> Self {
        Self::IdxDecode(e)
    }
}

impl<T> From<std::sync::PoisonError<T>> for IndexerError {
    fn from(_: std::sync::PoisonError<T>) -> Self {
        IndexerError::LockPoisoned
    }
}

