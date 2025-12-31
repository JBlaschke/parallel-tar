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

// use std::collections::VecDeque;
// use std::fs;
// use std::io;
// use std::path::{Path, PathBuf};
// 
// #[derive(Debug)]
// pub enum NodeType {
//     File { size: u64 },
//     Directory { children: Vec<TreeNode> },
//     Symlink { target: PathBuf },
// }
// 
// #[derive(Debug)]
// pub struct TreeNode {
//     pub name: String,
//     pub path: PathBuf,
//     pub node_type: NodeType,
//     pub computed_size: Option<u64>,
// }
// 
// impl TreeNode {
//     /// Recursively build a tree from the given path
//     pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
//         let path = path.as_ref();
//         let metadata = fs::symlink_metadata(path)?;
//         let name = path
//             .file_name()
//             .map(|s| s.to_string_lossy().into_owned())
//             .unwrap_or_else(|| path.to_string_lossy().into_owned());
// 
//         let node_type = if metadata.is_symlink() {
//             let target = fs::read_link(path).unwrap_or_default();
//             NodeType::Symlink { target }
//         } else if metadata.is_dir() {
//             let mut children = Vec::new();
//             for entry in fs::read_dir(path)? {
//                 let entry = entry?;
//                 match TreeNode::from_path(entry.path()) {
//                     Ok(child) => children.push(child),
//                     Err(e) => eprintln!("Warning: couldn't read {:?}: {}", entry.path(), e),
//                 }
//             }
//             children.sort_by(|a, b| a.name.cmp(&b.name));
//             NodeType::Directory { children }
//         } else {
//             NodeType::File { size: metadata.len() }
//         };
// 
//         Ok(TreeNode {
//             name,
//             path: path.to_path_buf(),
//             node_type,
//             computed_size: None,
//         })
//     }
// 
//     /// Get children if this is a directory
//     pub fn children(&self) -> &[TreeNode] {
//         match &self.node_type {
//             NodeType::Directory { children } => children,
//             _ => &[],
//         }
//     }
// 
//     /// Compute sizes bottom-up: starts at leaves (files), then propagates
//     /// the sum upward to parent directories. Returns the computed size.
//     pub fn compute_sizes(&mut self) -> u64 {
//         let size = match &mut self.node_type {
//             NodeType::File { size } => *size,
//             NodeType::Symlink { .. } => 0,
//             NodeType::Directory { children } => {
//                 // Recursively compute children first (post-order traversal)
//                 children.iter_mut().map(|child| child.compute_sizes()).sum()
//             }
//         };
//         self.computed_size = Some(size);
//         size
//     }
// 
//     /// Create a depth-first iterator
//     pub fn iter_depth_first(&self) -> DepthFirstIter<'_> {
//         DepthFirstIter { stack: vec![self] }
//     }
// 
//     /// Create a breadth-first iterator
//     pub fn iter_breadth_first(&self) -> BreadthFirstIter<'_> {
//         let mut queue = VecDeque::new();
//         queue.push_back(self);
//         BreadthFirstIter { queue }
//     }
// 
//     /// Pretty print the tree with computed sizes
//     pub fn print_tree(&self, prefix: &str, is_last: bool) {
//         let connector = if is_last { "└── " } else { "├── " };
//         let icon = match &self.node_type {
//             NodeType::File { .. } => "📄",
//             NodeType::Directory { .. } => "📁",
//             NodeType::Symlink { .. } => "🔗",
//         };
// 
//         let size_str = self
//             .computed_size
//             .map(|s| format!(" ({})", format_size(s)))
//             .unwrap_or_default();
// 
//         println!("{}{}{} {}{}", prefix, connector, icon, self.name, size_str);
// 
//         if let NodeType::Directory { children } = &self.node_type {
//             let new_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
//             for (i, child) in children.iter().enumerate() {
//                 child.print_tree(&new_prefix, i == children.len() - 1);
//             }
//         }
//     }
// 
//     /// Count total files and directories
//     pub fn count(&self) -> (usize, usize) {
//         match &self.node_type {
//             NodeType::File { .. } | NodeType::Symlink { .. } => (1, 0),
//             NodeType::Directory { children } => {
//                 let (files, dirs) = children
//                     .iter()
//                     .map(|c| c.count())
//                     .fold((0, 0), |(f1, d1), (f2, d2)| (f1 + f2, d1 + d2));
//                 (files, dirs + 1)
//             }
//         }
//     }
// }
// 
// /// Format bytes into human-readable size
// pub fn format_size(bytes: u64) -> String {
//     const KB: u64 = 1024;
//     const MB: u64 = KB * 1024;
//     const GB: u64 = MB * 1024;
// 
//     if bytes >= GB {
//         format!("{:.2} GB", bytes as f64 / GB as f64)
//     } else if bytes >= MB {
//         format!("{:.2} MB", bytes as f64 / MB as f64)
//     } else if bytes >= KB {
//         format!("{:.2} KB", bytes as f64 / KB as f64)
//     } else {
//         format!("{} B", bytes)
//     }
// }
// 
// /// Depth-first (pre-order) iterator over tree nodes
// pub struct DepthFirstIter<'a> {
//     stack: Vec<&'a TreeNode>,
// }
// 
// impl<'a> Iterator for DepthFirstIter<'a> {
//     type Item = &'a TreeNode;
// 
//     fn next(&mut self) -> Option<Self::Item> {
//         let node = self.stack.pop()?;
// 
//         for child in node.children().iter().rev() {
//             self.stack.push(child);
//         }
// 
//         Some(node)
//     }
// }
// 
// /// Breadth-first (level-order) iterator over tree nodes
// pub struct BreadthFirstIter<'a> {
//     queue: VecDeque<&'a TreeNode>,
// }
// 
// impl<'a> Iterator for BreadthFirstIter<'a> {
//     type Item = &'a TreeNode;
// 
//     fn next(&mut self) -> Option<Self::Item> {
//         let node = self.queue.pop_front()?;
// 
//         for child in node.children() {
//             self.queue.push_back(child);
//         }
// 
//         Some(node)
//     }
// }


// use std::fs;
// use std::path::{Path, PathBuf};
// use std::io;
// use std::ffi::OsStr;
// 
// // 1. The Recursive Structure
// #[derive(Debug)]
// pub enum FileNode {
//     File {
//         path: PathBuf,
//         len: u64,
//     },
//     Directory {
//         path: PathBuf,
//         children: Vec<FileNode>,
//     },
// }
// 
// impl FileNode {
//     // Constructor (same as before)
//     pub fn from_path(path: &Path) -> io::Result<Self> {
//         let metadata = fs::symlink_metadata(path)?;
//         let path_buf = path.to_path_buf();
// 
//         if metadata.is_dir() {
//             let mut children = Vec::new();
//             for entry in fs::read_dir(path)? {
//                 let entry = entry?;
//                 children.push(FileNode::from_path(&entry.path())?);
//             }
//             
//             // Optional: Sort children by name for a cleaner tree
//             children.sort_by_key(|c| c.path().file_name().map(|s| s.to_os_string()));
// 
//             Ok(FileNode::Directory {
//                 path: path_buf,
//                 children,
//             })
//         } else {
//             Ok(FileNode::File {
//                 path: path_buf,
//                 len: metadata.len(),
//             })
//         }
//     }
// 
//     // 2. The Helper to get the file name safely
//     fn name(&self) -> &str {
//         self.path()
//             .file_name()
//             .and_then(OsStr::to_str)
//             .unwrap_or("<unknown>")
//     }
// 
//     pub fn path(&self) -> &Path {
//         match self {
//             FileNode::File { path, .. } => path,
//             FileNode::Directory { path, .. } => path,
//         }
//     }
// 
//     // 3. Public entry point for printing
//     pub fn print_tree(&self) {
//         // Print the root node first (no indentation)
//         println!("{}", self.name());
//         
//         // If it's a directory, kick off the recursive helper
//         if let FileNode::Directory { children, .. } = self {
//             for (i, child) in children.iter().enumerate() {
//                 // Check if this child is the last one in the list
//                 let is_last = i == children.len() - 1;
//                 child.print_recursive("", is_last);
//             }
//         }
//     }
// 
//     // 4. The Recursive Logic
//     fn print_recursive(&self, prefix: &str, is_last: bool) {
//         // Choose the connector: "└── " for the last item, "├── " for others
//         let connector = if is_last { "└── " } else { "├── " };
//         
//         // Print the current node
//         println!("{}{}{}", prefix, connector, self.name());
// 
//         // Calculate the prefix for *our* children
//         // If we are the last node, our children don't need a pipe from us.
//         // If we aren't, our children need a pipe "│   " to connect our siblings.
//         let child_prefix = format!(
//             "{}{}",
//             prefix, if is_last { "    " } else { "│   " }
//         );
// 
//         if let FileNode::Directory { children, .. } = self {
//             for (i, child) in children.iter().enumerate() {
//                 let is_last_child = i == children.len() - 1;
//                 child.print_recursive(&child_prefix, is_last_child);
//             }
//         }
//     }
// }
// 
// // fn main() -> io::Result<()> {
// //     // Use "." to scan the current directory
// //     let root = FileNode::from_path(Path::new("."))?;
// //     
// //     println!("--- Directory Tree ---");
// //     root.print_tree();
// //     
// //     Ok(())
// // }
