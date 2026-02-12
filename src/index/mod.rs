// SPDX-License-Identifier: AGPL-3.0-or-later
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
