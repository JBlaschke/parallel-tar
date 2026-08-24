// SPDX-License-Identifier: AGPL-3.0-or-later

//! Filesystem path utilities shared by the archiver and the index tools.
//!
//! * [`path`] — lexical path analysis ([`path::analyze_path`]), tar entry
//!   sanitization ([`path::sanitize_rel_path`]), and the directory-permission
//!   plan used during extraction ([`path::DirPlan`]).
//! * [`tree`] — turns a saved index (`.idx`/`.etr`/`.json`) back into the
//!   flat list of paths the archiver consumes
//!   ([`tree::files_from_tree`]).

// Analyze paths
pub mod path;
// Paths from tree
pub mod tree;
