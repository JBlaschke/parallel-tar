use crate::index::error::IndexerError;

// Serde serialization (for NodeMetadata)
use serde::{Deserialize, Serialize};
// Used for iterationg over tree and updating tree's internal metadata
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
// Used for defining tree nodes
use std::path::PathBuf;
// Used for parallel processing
use rayon::prelude::*;
// Used for logging
use log::warn;

#[derive(Debug)]
pub enum NodeType {
    File { size: u64 },
    Directory { children: Vec<Arc<TreeNode>> },
    Symlink { target: PathBuf },
    Unknown {}
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default)]
pub struct NodeMetadata {
    pub size:  usize,
    pub files: usize,
    pub dirs:  usize
}

#[derive(Debug)]
pub struct TreeNode {
    pub name: String,
    pub path: PathBuf,
    pub node_type: NodeType,
    pub metadata: RwLock<Option<NodeMetadata>>,
    pub hash: RwLock<Option<String>>
}

impl TreeNode {
    /// Get children if this is a directory
    pub fn children(&self) -> &[Arc<TreeNode>] {
        match &self.node_type {
            NodeType::Directory { children } => children,
            _ => &[],
        }
    }

    fn reduce_metadata(
                md1: Result<NodeMetadata, IndexerError>,
                md2: Result<NodeMetadata, IndexerError>,
            ) -> Result<NodeMetadata, IndexerError> {
        let md1 = md1?;
        let md2 = md2?;

        return Ok(NodeMetadata {
            size:  md1.size  + md2.size,
            files: md1.files + md2.files,
            dirs:  md1.dirs  + md2.dirs
        });
    }

    /// Compute metadata bottom-up in parallel using rayon
    pub fn compute_metadata(&self) -> Result<NodeMetadata, IndexerError> {
        // Lock the metadata field for writing. Lock all metadata at once, to
        // avoid corruption due to different metadata fields being potentially
        // updated by different update passes.
        let mut guard = self.metadata.write()?;

        let meta = match & self.node_type {
            NodeType::File { size } => NodeMetadata {
                size: * size as usize,
                files:  1,
                dirs:   0
            },
            NodeType::Symlink { .. } => NodeMetadata {
                size:  0,
                files: 1,
                dirs:  0
            },
            NodeType::Directory { children } => {
                // Process children in parallel. Note: this is Rayon's reduce operation:
                // rayon/iter/trait.ParallelIterator.html#method.reduce
                let c_meta = children
                    .par_iter()
                    .map(|child| child.compute_metadata())
                    .reduce(
                        || Ok(NodeMetadata {
                            size:  0,
                            files: 0,
                            dirs:  0
                        }),
                        |md1, md2| Self::reduce_metadata(md1, md2),
                    )?;
                NodeMetadata {
                    size:  c_meta.size,
                    files: c_meta.files,
                    // remember to also count _this_ directory
                    dirs:  c_meta.dirs + 1
                }
            },
            NodeType::Unknown {} => NodeMetadata::default()
        };

        * guard = Some(meta);
        return Ok(meta);
    }

    /// Access the metadata field (behind the RwLock) for reading -- and copy
    /// the result. Note that errors (e.g. poisoned locks) are emitted as
    /// warnings, whereby the result would be the default `NodeMetadata`
    pub fn read_metadata(&self) -> Option<NodeMetadata> {
        self.metadata
            .read()
            .map_err(|e| warn!("Failed to get READ lock: '{}'", e))
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Access the hash field (behind the RwLock) for reading -- and copy the
    /// result. Note that errors (e.g. poisoned locks) are emitted as warnings,
    /// whereby the result would be the default `String`
    pub fn read_hash(&self) -> Option<String> {
        self.hash
            .read()
            .map_err(|e| warn!("Failed to get READ lock: '{}'", e))
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// Create a depth-first iterator
    pub fn iter_depth_first(self: &Arc<Self>) -> DepthFirstIter {
        DepthFirstIter {
            stack: vec![Arc::clone(self)],
        }
    }

    /// Create a breadth-first iterator
    pub fn iter_breadth_first(self: &Arc<Self>) -> BreadthFirstIter {
        let mut queue = VecDeque::new();
        queue.push_back(Arc::clone(self));
        BreadthFirstIter { queue }
    }

    /// Collect all nodes into a Vec for parallel processing
    pub fn collect_all(self: &Arc<Self>) -> Vec<Arc<TreeNode>> {
        self.iter_depth_first().collect()
    }
}

/// Depth-first (pre-order) iterator over tree nodes
pub struct DepthFirstIter {
    stack: Vec<Arc<TreeNode>>,
}

impl Iterator for DepthFirstIter {
    type Item = Arc<TreeNode>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;

        for child in node.children().iter().rev() {
            self.stack.push(Arc::clone(child));
        }

        Some(node)
    }
}

/// Breadth-first (level-order) iterator over tree nodes
pub struct BreadthFirstIter {
    queue: VecDeque<Arc<TreeNode>>,
}

impl Iterator for BreadthFirstIter {
    type Item = Arc<TreeNode>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.queue.pop_front()?;

        for child in node.children() {
            self.queue.push_back(Arc::clone(child));
        }

        Some(node)
    }
}

