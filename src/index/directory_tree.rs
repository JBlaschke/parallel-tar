use std::collections::VecDeque;
use std::fs;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use std::error::Error;
use std::fmt;

use rayon::prelude::*;

#[derive(Debug)]
pub enum IndexerError {
    Io(std::io::Error),
    InvalidPath(String),
    NotFound(String),
}

impl fmt::Display for IndexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::InvalidPath(p) => write!(f, "invalid path: {}", p),
            Self::NotFound(p) => write!(f, "node not found: {}", p),
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

#[derive(Debug)]
pub enum NodeType {
    File { size: u64 },
    Directory { children: Vec<Arc<TreeNode>> },
    Symlink { target: PathBuf },
}

#[derive(Debug)]
pub struct TreeNode {
    pub name: String,
    pub path: PathBuf,
    pub node_type: NodeType,
    pub computed_size: AtomicU64,
}

// Explicitly implement Sync since AtomicU64 is Sync
// and our other fields are Sync
unsafe impl Sync for TreeNode {}

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
                let entry = match entry {
                    Ok(v) => v,
                    Err(_) => return Err(IndexerError::InvalidPath(
                            path.to_string_lossy().into_owned()
                        ))
                };
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

        let path: &Path        = path.as_ref();
        let metadata: Metadata = fs::symlink_metadata(path)?;
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

        let node_type: NodeType = Self::node_type_from_path(
            path, follow_symlinks, valid_symlinks_only
        )?;

        Ok(Arc::new(TreeNode {
            name,
            path: path.to_path_buf(),
            node_type,
            computed_size: AtomicU64::new(0),
        }))
    }

    /// Get children if this is a directory
    pub fn children(&self) -> &[Arc<TreeNode>] {
        match &self.node_type {
            NodeType::Directory { children } => children,
            _ => &[],
        }
    }

    /// Compute sizes bottom-up sequentially
    pub fn compute_sizes(&self) -> u64 {
        let size = match &self.node_type {
            NodeType::File { size } => *size,
            NodeType::Symlink { .. } => 0,
            NodeType::Directory { children } => {
                children.iter().map(|child| child.compute_sizes()).sum()
            }
        };
        self.computed_size.store(size, Ordering::SeqCst);
        size
    }

    /// Compute sizes bottom-up in parallel using rayon
    pub fn compute_sizes_parallel(&self) -> u64 {
        let size = match &self.node_type {
            NodeType::File { size } => *size,
            NodeType::Symlink { .. } => 0,
            NodeType::Directory { children } => {
                // Process children in parallel
                children
                    .par_iter()
                    .map(|child| child.compute_sizes_parallel())
                    .sum()
            }
        };
        self.computed_size.store(size, Ordering::SeqCst);
        size
    }

    /// Get the computed size (after compute_sizes has been called)
    pub fn get_computed_size(&self) -> u64 {
        self.computed_size.load(Ordering::SeqCst)
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
        let icon = match &self.node_type {
            NodeType::File { .. } => "📄",
            NodeType::Directory { .. } => "📁",
            NodeType::Symlink { .. } => "🔗",
        };

        let size = self.computed_size.load(Ordering::SeqCst);
        let size_str = if size > 0 {
            format!(" ({})", format_size(size))
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

    /// Count total files and directories
    pub fn count(&self) -> (usize, usize) {
        match &self.node_type {
            NodeType::File { .. } | NodeType::Symlink { .. } => (1, 0),
            NodeType::Directory { children } => {
                let (files, dirs) = children
                    .iter()
                    .map(|c| c.count())
                    .fold((0, 0), |(f1, d1), (f2, d2)| (f1 + f2, d1 + d2));
                (files, dirs + 1)
            }
        }
    }

    /// Count in parallel
    pub fn count_parallel(&self) -> (usize, usize) {
        match &self.node_type {
            NodeType::File { .. } | NodeType::Symlink { .. } => (1, 0),
            NodeType::Directory { children } => {
                let (files, dirs) = children
                    .par_iter()
                    .map(|c| c.count_parallel())
                    .reduce(|| (0, 0), |(f1, d1), (f2, d2)| (f1 + f2, d1 + d2));
                (files, dirs + 1)
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
