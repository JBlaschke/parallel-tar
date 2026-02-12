// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::index::tree::{TreeNode, NodeType};
use crate::index::error::IndexerError;

// Crypto functions (use MD5 or SHA256)
use md5;
use sha2::{Sha256, Digest};
// File I/O
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
// Logging
use log::warn;
// Used for parallel processing
use rayon::prelude::*;

fn hash_file_md5(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut context = md5::Context::new();

    // TODO: buffer size could be a setting -- right now we are using a
    // hard-coded 1MiB
    let mut buffer = [0u8; 1048576];
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 { break; }
        context.consume(&buffer[..bytes_read]);
    }

    // Ok(format!("{:x}", hasher.finalize()))
    Ok(format!("{:x}", context.finalize()))
}

fn hash_string_md5(s: &str) -> String {
    format!("{:x}", md5::compute(s.as_bytes()))
}

fn hash_file_sha256(path: &Path) -> std::io::Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();

    // TODO: buffer size could be a setting -- right now we are using a
    // hard-coded 1MiB
    let mut buffer = [0u8; 1048576];
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 { break; }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_string_sha256(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub trait HashedNodes {
    fn compute_hashes(&self, use_md5: bool) -> Result<String, IndexerError>;
}

impl HashedNodes for TreeNode {
    /// Compute hashes bottom-up: files hash their contents, symlinks are either
    /// treated as their target file (if the tree is constructed using
    /// `follow_symlinks = true`) or the target string, directories hash
    /// their children's (name, hash) pairs.
    ///
    /// Explanation of the algorithm for directories: Assume the hashes for all
    /// children {c1, c2, ..., cn} are known. Each node (cn) has a name
    /// (cn.name) and a hash (cn.hash) -- both are strings. Sort {c1, c2, ...,
    /// cn} alphabetically by cn.name. The directory's hash will be the hash of
    /// the string:
    /// "$(c1.name)$(c1.hash)$(c2.name)$(c2.hash)...$(cn.name)$(cn.hash)"
    ///
    /// `NodeType::Unknown` nodes are hashed by their names only.
    fn compute_hashes(&self, use_md5: bool) -> Result<String, IndexerError> {
        // Shortcut evaluation: if the node already has a hash, then don't need
        // to re-compute it. Note we're using the raw lock (and not read_hash)
        // so that we can correcly propagate any errors correcly.
        match self.hash.read()?.as_ref() {
            Some(v) => return Ok(v.clone()),
            None    => {}
        }

        let hash_file = |path: &Path| -> std::io::Result<String> {
            if use_md5 {
                hash_file_md5(path)
            } else {
                hash_file_sha256(path)
            }
        };
        let hash_string = |data: &str| -> String {
            if use_md5 {
                hash_string_md5(data)
            } else {
                hash_string_sha256(data)
            }
        };

        // If getting here: we don't have a current hash => compute it
        // recursively.
        let hash = match &self.node_type {
            NodeType::File { .. } => {
                // Note: hash_file will already be guaranteed to return an
                // std::io::Error (and not another error type) => we can capture
                // it using the `Err(e)` pattern, without needing to further
                // check the type of the error.
                match hash_file(&self.path) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            "'hash_file({:?})' failed with '{}'",
                            &self.path.to_string_lossy().into_owned(),
                            e.kind()
                        );
                        hash_string(&self.name.to_string())
                    }
                }

            },
            NodeType::Symlink { target } => {
                // Note that if the tree was constructed using `follow_symlinks
                // = true` then the node_type will not be a `NoteType::Symlink`
                hash_string(& target.to_string_lossy())
            },
            NodeType::Directory { children } => {
                // Compute child hashes in parallel
                let mut child_hashes: Vec<_> = children
                    .par_iter()
                    .map(|child| {
                        let hash = child.compute_hashes(use_md5)?;
                        Ok((child.name.clone(), hash))
                    })
                    .collect::<Result<Vec<_>, IndexerError>>()?;

                // Algorithm for directories: Assume the hashes for all children
                // {c1, c2, ..., cn} are known. Each node (cn) has a name
                // (cn.name) and a hash (cn.hash) -- both are strings. Sort {c1,
                // c2, ..., cn} alphabetically by cn.name. The directory's hash
                // will be the hash of the string:
                // "$(c1.name)$(c1.hash)$(c2.name)$(c2.hash)...$(cn.name)$(cn.hash)"

                // Sort by name (must be after parallel computation)
                child_hashes.sort_by(|a, b| a.0.cmp(&b.0));

                // Combine
                let combined: String = child_hashes
                    .iter()
                    .flat_map(|(name, hash)| [name.as_str(), hash.as_str()])
                    .collect();

                hash_string(&combined)
            },
            NodeType::Socket {} => {
                hash_string(&self.name.to_string())
            }
            NodeType::Fifo {} => {
                hash_string(&self.name.to_string())
            }
            NodeType::Device {} => {
                hash_string(&self.name.to_string())
            }
            NodeType::Unknown { .. } => {
                hash_string(&self.name.to_string())
            }
        };

        // Only lock the hash now as we're updating the value:
        let mut guard = self.hash.write()?;
        * guard = Some(hash.clone());
        Ok(hash)
    }
}
