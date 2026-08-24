// SPDX-License-Identifier: AGPL-3.0-or-later

//! The index: an in-memory directory tree with sizes and content hashes.
//!
//! An index records the structure of a directory tree together with
//! per-node metadata (size, file/dir counts) and content hashes (MD5 or
//! SHA-256). A directory's hash is derived from its children's names and
//! hashes, so the root hash summarizes the entire tree — two trees with the
//! same root hash are byte-for-byte identical.
//!
//! * [`tree`] — the [`tree::TreeNode`] data structure and its iterators.
//! * [`fs`] — build a tree by walking the filesystem ([`fs::Filesystem`]).
//! * [`crypto`] — fill in file hashes and aggregate directory hashes
//!   ([`crypto::HashedNodes`]).
//! * [`serialize`] — save/load trees as `.idx` (MessagePack) or `.json`
//!   files ([`serialize::save_tree`], [`serialize::load_tree`]).
//! * [`display`] — pretty-print trees ([`display::Display`]).
//! * [`path`] — resolve user-supplied sub-paths by child name
//!   ([`path::resolve_chain`]).
//! * [`error`] — the [`error::IndexerError`] type shared by this module.

// Definitions and iterators for the tree itself
pub mod tree;

// functions to serialzie and deserialize tree -- note that the struct
// definitions need to reflect those in `tree` above
pub mod serialize;
pub use serialize::Serializeable;

// error handling
pub mod error;

// functions to help display the tree
pub mod display;
pub use display::Display;

// build tree from the file system
pub mod fs;
pub use fs::Filesystem;

// cryptographic functions for computing hashes
pub mod crypto;
pub use crypto::HashedNodes;

// path utils for indexer
pub mod path;
