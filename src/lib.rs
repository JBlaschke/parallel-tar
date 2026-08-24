// SPDX-License-Identifier: AGPL-3.0-or-later

//! # `ptar_lib` — the library behind Parallel Tar
//!
//! This crate implements the machinery shared by the `parallel-tar`,
//! `parallel-idx`, `view-idx`, `diff-idx`, and `edit-idx` command-line tools.
//! It is organized into three modules:
//!
//! * [`index`] — the in-memory directory tree ([`index::tree::TreeNode`]),
//!   plus everything needed to build it from the filesystem
//!   ([`index::fs::Filesystem`]), aggregate metadata and content hashes
//!   ([`index::crypto::HashedNodes`]), serialize it to `.idx` (MessagePack)
//!   or `.json` files ([`index::serialize`]), pretty-print it
//!   ([`index::display`]), and resolve user-supplied sub-paths within it
//!   ([`index::path`]).
//! * [`archive`] — multi-threaded creation ([`archive::tar::create`]),
//!   extraction ([`archive::tar::extract`]), and verification
//!   ([`archive::verify::verify`]) of sharded tar archives, built on a
//!   clonable multi-producer channel abstraction ([`archive::mutex::Pipe`]).
//! * [`files`] — path analysis and sanitization helpers ([`files::path`]),
//!   and the bridge that turns a saved index back into a work list for the
//!   archiver ([`files::tree`]).
//!
//! The rest of this page is the project README, which documents the
//! command-line workflow these modules implement.
//!
//! ---
#![doc = include_str!("../README.md")]

pub mod files;
pub mod index;
pub mod archive;
