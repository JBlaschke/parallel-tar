use crate::index::tree::{TreeNode, NodeType};
use crate::index::error::IndexerError;
// Working with references and concurrent access
use std::sync::{Arc, RwLock};
// Working with the file system
use std::fs;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
// Logging
use log::warn;

pub trait Filesystem {
    fn node_type_from_path(
        path: impl AsRef<Path>,
        follow_symlinks: bool,
        valid_symlinks_only: bool
    ) -> Result<NodeType, IndexerError>;

    fn from_path(
        path: impl AsRef<Path>,
        follow_symlinks: bool,
        valid_symlinks_only: bool
    ) -> Result<Arc<Self>, IndexerError>;
}

impl Filesystem for TreeNode {
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

        let file_type = metadata.file_type();
        let node_type = if file_type.is_symlink() {
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
                NodeType::Symlink { target: target }
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
            NodeType::Directory { children: children }
        } else if file_type.is_file() {
            NodeType::File { size: metadata.len() }
        } else {
            #[cfg(unix)]
            {
                // Special treament for Unix sockets on the File System
                use std::os::unix::fs::FileTypeExt;
                if file_type.is_socket() {
                    NodeType::Socket {}
                } else if file_type.is_fifo() {
                    NodeType::Fifo {}
                } else if file_type.is_block_device() || file_type.is_char_device() {
                    NodeType::Device {}
                } else {
                    NodeType::Unknown { error: "".to_string() }
                }
            }
            #[cfg(not(unix))]
            {
                NodeType::Unknown { error: "".to_string() }
            }
        };

        return Ok(node_type);
    }

    /// Recursively build a tree from the given path
    fn from_path(
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
            Err(IndexerError::Io(e)) => {
                warn!(
                    "'node_type_from_path({:?})' failed with '{}'",
                    path.to_string_lossy().into_owned(),
                    e.kind()
                );
                NodeType::Unknown { error: e.to_string() }
            },
            Err(e) => return Err(e)
        };

        Ok(Arc::new(TreeNode {
            name,
            path: path.to_path_buf(),
            node_type,
            metadata: RwLock::new(None),
            hash: RwLock::new(None)
        }))
    }
}
