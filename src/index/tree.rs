use crate::index::error::IndexerError;

// Serde serialization (for NodeMetadata)
use serde::{Deserialize, Serialize};

use std::collections::VecDeque;
use std::fs;
use std::fs::{File, Metadata};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use std::io::{Read, BufReader, BufWriter};

use rayon::prelude::*;

use log::warn;

use sha2::{Sha256, Digest};

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
    pub metadata: RwLock<Option<NodeMetadata>>
}


impl TreeNode {
    fn node_type_from_path(
                path: impl AsRef<Path>,
                follow_symlinks: bool,
                mut valid_symlinks_only: bool
            ) -> Result<NodeType, IndexerError> {

        let path: &Path        = path.as_ref();
        let metadata: Metadata = fs::symlink_metadata(path)?;
        // Can't follow invalid symlinks
        if follow_symlinks {
            valid_symlinks_only = true;
        }

        let node_type = if metadata.is_symlink() {
            let target: PathBuf = match fs::read_link(path) {
                Ok(v) => v,
                Err(_) => {
                    if valid_symlinks_only {
                        return Err(IndexerError::NotFound(
                            path.to_string_lossy().into_owned()
                        ));
                    }
                    path.to_path_buf()
                }
            };
            if follow_symlinks {
                return Self::node_type_from_path(
                    path, follow_symlinks, valid_symlinks_only
                );
            } else { 
                NodeType::Symlink { target }
            }
        } else if metadata.is_dir() {
            let mut children = Vec::new();
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                match TreeNode::from_path(
                        entry.path(), follow_symlinks, valid_symlinks_only
                    ) {
                    Ok(child) => children.push(child),
                    Err(e) => return Err(e.into())
                }
            }
            children.sort_by(|a, b| a.name.cmp(&b.name));
            NodeType::Directory { children }
        } else {
            NodeType::File { size: metadata.len() }
        };

        return Ok(node_type);
    }

    /// Recursively build a tree from the given path
    pub fn from_path(
                path: impl AsRef<Path>,
                follow_symlinks: bool,
                mut valid_symlinks_only: bool
            ) -> Result<Arc<Self>, IndexerError> {

        let path: &Path = path.as_ref();
        // Can't follow invalid symlinks
        if follow_symlinks {
            valid_symlinks_only = true;
        }

         // This says: "get the file name, or if there isn't one (e.g., root
         // path /), use the full path as the name." The closure avoids
         // computing the fallback string unless actually needed.
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());

        let node_type: NodeType = match Self::node_type_from_path(
                path, follow_symlinks, valid_symlinks_only
            ) {
            Ok(v) => v,
            Err(IndexerError::Io(e))
                if e.kind() == std::io::ErrorKind::PermissionDenied => {
                warn!(
                    "'node_type_from_path({:?})' failed with 'Permission denied'",
                    path.to_string_lossy().into_owned()
                );
                NodeType::Unknown {}
            },
            Err(e) => return Err(e)
        };

        Ok(Arc::new(TreeNode {
            name,
            path: path.to_path_buf(),
            node_type,
            metadata: RwLock::new(None)
        }))
    }

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

        *guard = Some(meta);
        return Ok(meta);
    }

    pub fn read_metadata(&self) -> Option<NodeMetadata> {
        self.metadata
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

    /// Pretty print the tree with computed sizes
    pub fn print_tree(&self, prefix: &str, is_last: bool) {
        let connector = if is_last { "└── " } else { "├── " };
        let icon = match & self.node_type {
            NodeType::File { .. } => "📄",
            NodeType::Directory { .. } => "📁",
            NodeType::Symlink { .. } => "🔗",
            NodeType::Unknown { .. } => "❓",
        };

        let size = match self.read_metadata() {
            Some(v) => v.size,
            None => 0
        };
        let size_str = if size > 0 {
            format!(" ({})", format_size(size as u64))
        } else {
            String::new()
        };

        println!("{}{}{} {}{}", prefix, connector, icon, self.name, size_str);

        if let NodeType::Directory { children } = &self.node_type {
            let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
            for (i, child) in children.iter().enumerate() {
                child.print_tree(&new_prefix, i == children.len() - 1);
            }
        }
    }
}

pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
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

