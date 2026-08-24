// SPDX-License-Identifier: AGPL-3.0-or-later

//! Save and load index trees as `.idx` (MessagePack) or `.json` files.
//!
//! [`TreeNode`] itself is not directly serializable (its derived fields sit
//! behind `RwLock`s), so trees are converted to/from the plain
//! [`SerializedTreeNode`] mirror via the [`Serializeable`] trait. The
//! on-disk format is chosen by [`DataFmt`]; the "empty tree" `.etr` files
//! produced by `parallel-idx -e` are ordinary `.idx` files whose metadata
//! and hash fields are all `None`.

use crate::index::tree::{TreeNode, NodeType, NodeMetadata};
use crate::index::error::IndexerError;

use std::sync::Arc;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use serde_json;
use rmp_serde;


/// Serializable mirror of [`NodeType`]; must be kept structurally in sync
/// with it.
#[derive(Debug, Serialize, Deserialize)]
pub enum SerializedNodeType {
    File { size: u64 },
    Directory { children: Vec<SerializedTreeNode> },
    Symlink { target: PathBuf },
    Socket {},
    Fifo {},
    Device {},
    Unknown { error: String }
}

/// Serializable mirror of [`TreeNode`], with the `RwLock`s unwrapped to
/// plain `Option`s. This is the exact structure written to `.idx`/`.json`
/// files; must be kept structurally in sync with [`TreeNode`].
#[derive(Debug, Serialize, Deserialize)]
pub struct SerializedTreeNode {
    pub name: String,
    pub path: PathBuf,
    pub node_type: SerializedNodeType,
    pub metadata: Option<NodeMetadata>,
    pub hash: Option<String>
}

/// Conversion between a live tree and its on-disk mirror.
pub trait Serializeable {
    /// Deep-copy this node (and its subtree) into the serializable mirror,
    /// snapshotting the current metadata and hash values.
    fn to_serializable(&self) -> Result<SerializedTreeNode, IndexerError>;
    /// Rebuild a live tree from its serializable mirror.
    fn from_serializable(s: SerializedTreeNode) -> Arc<Self>;
}

impl Serializeable for TreeNode {
    fn to_serializable(&self) -> Result<SerializedTreeNode, IndexerError> {
        let node_type = match & self.node_type {
            NodeType::File { size } => SerializedNodeType::File {
                size: *size
            },
            NodeType::Directory { children } => {
                let children: Result<Vec<_>, IndexerError> = children
                    .iter()
                    .map(|c| c.to_serializable())
                    .collect();
                let children = children?;
                SerializedNodeType::Directory { children }
            },
            NodeType::Symlink { target } => SerializedNodeType::Symlink {
                target: target.clone(),
            },
            NodeType::Socket {} => SerializedNodeType::Socket {},
            NodeType::Fifo {} => SerializedNodeType::Fifo {},
            NodeType::Device {} => SerializedNodeType::Device {},
            NodeType::Unknown { error } => SerializedNodeType::Unknown {
                error: error.to_string()
            }
        };

        Ok(SerializedTreeNode {
            name: self.name.clone(),
            path: self.path.clone(),
            node_type,
            metadata: * self.metadata.read()?,
            hash: self.hash.read()?.clone()
        })
    }

    fn from_serializable(s: SerializedTreeNode) -> Arc<Self> {
        let node_type = match s.node_type {
            SerializedNodeType::File { size } => NodeType::File {
                size: size
            },
            SerializedNodeType::Directory { children } => NodeType::Directory {
                children: children.into_iter().map(
                    TreeNode::from_serializable
                ).collect(),
            },
            SerializedNodeType::Symlink { target } => NodeType::Symlink {
                target: target
            },
            SerializedNodeType::Socket {} => NodeType::Socket {},
            SerializedNodeType::Fifo {} => NodeType::Fifo {},
            SerializedNodeType::Device {} => NodeType::Device {},
            SerializedNodeType::Unknown { error } => NodeType::Unknown { error }
        };

        Arc::new(TreeNode {
            name: s.name,
            path: s.path,
            node_type,
            metadata: s.metadata.into(),
            hash: s.hash.into()
        })
    }
}

/// An index file path tagged with its on-disk format: human-readable JSON,
/// or the compact MessagePack `.idx` format (also used for `.etr` files).
#[derive(Debug)]
pub enum DataFmt {
    Json(String),
    Idx(String)
}

// Serialize to JSON
fn save_tree_json(tree: &TreeNode, path: &str) -> Result<(), IndexerError> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let serializable = tree.to_serializable()?;
    serde_json::to_writer_pretty(writer, &serializable)?;
    Ok(())
}

// Deserialize from JSON
fn load_tree_json(path: &str) -> Result<Arc<TreeNode>, IndexerError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let serializable: SerializedTreeNode = serde_json::from_reader(reader)?;
    Ok(TreeNode::from_serializable(serializable))
}

// Serialize to Message Pack
fn save_tree_rmp(tree: &TreeNode, path: &str) -> Result<(), IndexerError> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    let serializable = tree.to_serializable()?;
    rmp_serde::encode::write(&mut writer, &serializable)?;
    Ok(())
}

// Deserialize from Message Pack
fn load_tree_rmp(path: &str) -> Result<Arc<TreeNode>, IndexerError> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let serializable: SerializedTreeNode = rmp_serde::decode::from_read(reader)?;
    Ok(TreeNode::from_serializable(serializable))
}

/// Write `tree` to the path and format named by `fmt`, overwriting any
/// existing file.
pub fn save_tree(tree: &TreeNode, fmt: DataFmt) -> Result<(), IndexerError> {
    match fmt {
        DataFmt::Json(path) => save_tree_json(tree, & path),
        DataFmt::Idx(path)  => save_tree_rmp(tree, & path)
    }
}

/// Read an index tree from the path and format named by `fmt`.
pub fn load_tree(fmt: DataFmt) -> Result<Arc<TreeNode>, IndexerError> {
    match fmt {
        DataFmt::Json(path) => load_tree_json(& path),
        DataFmt::Idx(path)  => load_tree_rmp(& path)
    }
}
