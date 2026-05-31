// SPDX-License-Identifier: AGPL-3.0-or-later
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

pub fn hash_reader_sha256<R: Read>(mut r: R) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1048576];
    loop {
        let n = r.read(&mut buffer)?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
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
    format!("{:x}", hasher.finalize())
}

// ─── Trait ───────────────────────────────────────────────────────────────

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
