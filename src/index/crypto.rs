// SPDX-License-Identifier: AGPL-3.0-or-later

//! Content hashing: fill in file hashes, then aggregate them up the tree.
//!
//! Hashing an index is a two-pass affair, exposed by the [`HashedNodes`]
//! trait:
//!
//! 1. [`HashedNodes::fill_hashes`] reads file contents (from disk, in
//!    parallel) and caches a hash on every `File` node.
//! 2. [`HashedNodes::compute_hashes`] aggregates purely in memory: a
//!    directory's hash is the hash of its children's sorted
//!    `name || hash` concatenation, so the root hash summarizes the whole
//!    tree.
//!
//! The streaming helpers [`hash_reader_md5`] / [`hash_reader_sha256`] are
//! shared with archive verification, which hashes tar entry streams instead
//! of on-disk files (see [`crate::archive::verify`]).

use crate::index::tree::{TreeNode, NodeType};
use crate::index::error::IndexerError;

use md5;
use sha2::{Sha256, Digest};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use log::warn;
use rayon::prelude::*;

// ─── Streaming hash helpers (reader-based) ───────────────────────────────

// sha2 >= 0.11 no longer implements LowerHex on the digest output; encode
// the bytes ourselves.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Stream `r` to completion and return its MD5 digest as lowercase hex.
/// Reads in 1 MiB chunks; never buffers the full content.
pub fn hash_reader_md5<R: Read>(mut r: R) -> std::io::Result<String> {
    let mut context = md5::Context::new();
    let mut buffer = vec![0u8; 1048576];
    loop {
        let n = r.read(&mut buffer)?;
        if n == 0 { break; }
        context.consume(&buffer[..n]);
    }
    Ok(format!("{:x}", context.finalize()))
}

/// Stream `r` to completion and return its SHA-256 digest as lowercase hex.
/// Reads in 1 MiB chunks; never buffers the full content.
pub fn hash_reader_sha256<R: Read>(mut r: R) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1048576];
    loop {
        let n = r.read(&mut buffer)?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

fn hash_file_md5(path: &Path) -> std::io::Result<String> {
    hash_reader_md5(BufReader::new(File::open(path)?))
}

fn hash_file_sha256(path: &Path) -> std::io::Result<String> {
    hash_reader_sha256(BufReader::new(File::open(path)?))
}

fn hash_string_md5(s: &str) -> String {
    format!("{:x}", md5::compute(s.as_bytes()))
}

fn hash_string_sha256(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    to_hex(&hasher.finalize())
}

// ─── Trait ───────────────────────────────────────────────────────────────

/// The two hashing passes over an index tree: `fill_hashes` (disk I/O) and
/// `compute_hashes` (in-memory aggregation). See the module docs for how
/// they compose.
pub trait HashedNodes {
    /// Aggregate hashes throughout the tree, propagating `None` upward where
    /// leaves are unhashed. NEVER reads from the filesystem.
    ///
    /// Returns `Some(hash)` if every leaf descendant has a hash, `None`
    /// otherwise. Symlinks, sockets, fifos, devices, and Unknown nodes are
    /// hashed from their in-tree data (target string or name) — they never need
    /// the filesystem and always produce a hash.
    ///
    /// Cached hashes are preserved; only missing directory hashes are
    /// (re)computed. A directory's hash is `None` if any of its children has
    /// hash `None`.
    fn compute_hashes(&self, use_md5: bool) -> Result<Option<String>, IndexerError>;

    /// Walk the tree in parallel, reading file contents from disk and caching
    /// hashes for every `NodeType::File` that doesn't already have one. Does
    /// NOT update directory hashes — run `compute_hashes` afterwards to
    /// propagate.
    ///
    /// Returns the number of files newly hashed. Files whose paths can't be
    /// opened produce a warning and leave `hash == None`; the tree remains
    /// partial in that case.
    fn fill_hashes(&self, use_md5: bool) -> Result<usize, IndexerError>;
}

impl HashedNodes for TreeNode {
    fn compute_hashes(&self, use_md5: bool) -> Result<Option<String>, IndexerError> {
        // Cached → return it. (None means "computed and known to be None" is
        // NOT a thing we cache; None always means "not yet computed".)
        if let Some(v) = self.hash.read()?.as_ref() {
            return Ok(Some(v.clone()));
        }

        let hash_string = |data: &str| -> String {
            if use_md5 { hash_string_md5(data) } else { hash_string_sha256(data) }
        };

        let hash_opt: Option<String> = match &self.node_type {
            // Files: never compute from disk here. If hash is None, propagate
            // None upward.
            NodeType::File { .. } => None,

            NodeType::Symlink { target } => {
                Some(hash_string(&target.to_string_lossy()))
            }

            NodeType::Directory { children } => {
                // Compute child hashes in parallel. Each child returns
                // Option<String>; if any is None, the directory is None.
                let child_hashes: Vec<_> = children
                    .par_iter()
                    .map(|child| {
                        let h = child.compute_hashes(use_md5)?;
                        Ok((child.name.clone(), h))
                    })
                    .collect::<Result<Vec<_>, IndexerError>>()?;

                // If any child has no hash, this directory has no hash.
                if child_hashes.iter().any(|(_, h)| h.is_none()) {
                    None
                } else {
                    // All children have hashes — combine deterministically.
                    let mut pairs: Vec<(String, String)> = child_hashes
                        .into_iter()
                        .map(|(n, h)| (n, h.unwrap()))
                        .collect();
                    pairs.sort_by(|a, b| a.0.cmp(&b.0));
                    let combined: String = pairs
                        .iter()
                        .flat_map(|(name, hash)| [name.as_str(), hash.as_str()])
                        .collect();
                    Some(hash_string(&combined))
                }
            }

            NodeType::Socket {}     => Some(hash_string(&self.name)),
            NodeType::Fifo {}       => Some(hash_string(&self.name)),
            NodeType::Device {}     => Some(hash_string(&self.name)),
            NodeType::Unknown { .. } => Some(hash_string(&self.name)),
        };

        // Only cache when we actually have a hash. None is not cached — that
        // way a future fill_hashes + compute_hashes can fill it in.
        if let Some(ref h) = hash_opt {
            let mut guard = self.hash.write()?;
            *guard = Some(h.clone());
        }
        Ok(hash_opt)
    }

    fn fill_hashes(&self, use_md5: bool) -> Result<usize, IndexerError> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let total = AtomicUsize::new(0);
        fill_recursive(self, use_md5, &total)?;
        Ok(total.load(Ordering::Relaxed))
    }
}

fn fill_recursive(
    node: &TreeNode,
    use_md5: bool,
    total: &std::sync::atomic::AtomicUsize,
) -> Result<(), IndexerError> {
    use std::sync::atomic::Ordering;
    match &node.node_type {
        NodeType::File { .. } => {
            if node.hash.read()?.is_some() {
                return Ok(());
            }
            let result = if use_md5 {
                hash_file_md5(&node.path)
            } else {
                hash_file_sha256(&node.path)
            };
            match result {
                Ok(h) => {
                    let mut guard = node.hash.write()?;
                    *guard = Some(h);
                    total.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    warn!("fill_hashes: could not hash {:?}: {}", node.path, e);
                }
            }
        }
        NodeType::Directory { children } => {
            children
                .par_iter()
                .try_for_each(|child| fill_recursive(child, use_md5, total))?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::tree::{TreeNode, NodeType};
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    fn node(name: &str, node_type: NodeType) -> Arc<TreeNode> {
        Arc::new(TreeNode {
            name: name.to_string(),
            path: PathBuf::from(name),
            node_type,
            metadata: RwLock::new(None),
            hash: RwLock::new(None),
        })
    }

    fn file(name: &str, size: u64, hash: Option<&str>) -> Arc<TreeNode> {
        let n = node(name, NodeType::File { size });
        * n.hash.write().unwrap() = hash.map(str::to_string);
        n
    }

    fn dir(name: &str, children: Vec<Arc<TreeNode>>) -> Arc<TreeNode> {
        node(name, NodeType::Directory { children })
    }

    // Index hashes are the product's core promise (byte-for-byte archive
    // validation) => pin the primitives to known test vectors so a silent
    // change in encoding or algorithm cannot slip through.

    #[test]
    fn hash_primitives_match_known_vectors() {
        assert_eq!(
            hash_string_md5("abc"),
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert_eq!(
            hash_string_sha256("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn directory_hash_is_deterministic() {
        let build = || dir("root", vec![
            file("a", 1, Some("hash-a")),
            file("b", 2, Some("hash-b")),
        ]);
        let h1 = build().compute_hashes(false).unwrap();
        let h2 = build().compute_hashes(false).unwrap();
        assert!(h1.is_some());
        assert_eq!(h1, h2);
    }

    #[test]
    fn directory_hash_is_independent_of_child_order() {
        // Children are combined sorted by name => the same set of children
        // must hash identically regardless of scan/insertion order
        let fwd = dir("root", vec![
            file("a", 1, Some("hash-a")),
            file("b", 2, Some("hash-b")),
        ]);
        let rev = dir("root", vec![
            file("b", 2, Some("hash-b")),
            file("a", 1, Some("hash-a")),
        ]);
        assert_eq!(
            fwd.compute_hashes(false).unwrap(),
            rev.compute_hashes(false).unwrap()
        );
    }

    #[test]
    fn directory_hash_depends_on_child_names_and_hashes() {
        let base = dir("root", vec![file("a", 1, Some("hash-a"))]);
        let renamed = dir("root", vec![file("b", 1, Some("hash-a"))]);
        let rehashed = dir("root", vec![file("a", 1, Some("hash-x"))]);

        let h_base = base.compute_hashes(false).unwrap();
        assert_ne!(h_base, renamed.compute_hashes(false).unwrap());
        assert_ne!(h_base, rehashed.compute_hashes(false).unwrap());
    }

    #[test]
    fn md5_and_sha256_trees_hash_differently() {
        let build = || dir("root", vec![file("a", 1, Some("hash-a"))]);
        assert_ne!(
            build().compute_hashes(false).unwrap(),
            build().compute_hashes(true).unwrap()
        );
    }

    #[test]
    fn unhashed_file_propagates_none_to_all_ancestors() {
        let tree = dir("root", vec![
            dir("full", vec![file("a", 1, Some("hash-a"))]),
            dir("partial", vec![file("b", 2, None)]),
        ]);
        assert_eq!(tree.compute_hashes(false).unwrap(), None);

        // None is not cached => filling in the missing leaf hash later must
        // let a recompute succeed (the fill_hashes + compute_hashes flow)
        if let NodeType::Directory { children } = &tree.node_type {
            if let NodeType::Directory { children } = &children[1].node_type {
                * children[0].hash.write().unwrap() =
                    Some("hash-b".to_string());
            }
        }
        assert!(tree.compute_hashes(false).unwrap().is_some());
    }

    #[test]
    fn cached_hash_is_returned_unchanged() {
        let tree = dir("root", vec![file("a", 1, Some("hash-a"))]);
        * tree.hash.write().unwrap() = Some("preset".to_string());
        assert_eq!(
            tree.compute_hashes(false).unwrap(),
            Some("preset".to_string())
        );
    }

    #[test]
    fn symlink_hash_derives_from_target() {
        let link = node("link", NodeType::Symlink {
            target: PathBuf::from("a.txt")
        });
        assert_eq!(
            link.compute_hashes(false).unwrap(),
            Some(hash_string_sha256("a.txt"))
        );
        // Same target, different link name => same hash
        let link2 = node("other-name", NodeType::Symlink {
            target: PathBuf::from("a.txt")
        });
        assert_eq!(
            link.compute_hashes(false).unwrap(),
            link2.compute_hashes(false).unwrap()
        );
    }
}
