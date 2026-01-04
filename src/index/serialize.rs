use crate::index::tree::{TreeNode, NodeType, NodeMetadata};
use crate::index::error::IndexerError;

use std::sync::Arc;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use serde_json;
use rmp_serde;


#[derive(Debug, Serialize, Deserialize)]
pub enum SerializedNodeType {
    File { size: u64 },
    Directory { children: Vec<SerializedTreeNode> },
    Symlink { target: PathBuf },
    Unknown {}
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SerializedTreeNode {
    pub name: String,
    pub path: PathBuf,
    pub node_type: SerializedNodeType,
    pub metadata: Option<NodeMetadata>
}

trait Serializeable {
    fn to_serializable(&self) -> Result<SerializedTreeNode, IndexerError>;
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
            NodeType::Unknown {} => SerializedNodeType::Unknown {}
        };

        Ok(SerializedTreeNode {
            name: self.name.clone(),
            path: self.path.clone(),
            node_type,
            metadata: * self.metadata.read()?
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
            SerializedNodeType::Unknown {} => NodeType::Unknown {}
        };

        Arc::new(TreeNode {
            name: s.name,
            path: s.path,
            node_type,
            metadata: s.metadata.into()
        })
    }
}

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

pub fn save_tree(tree: &TreeNode, fmt: DataFmt) -> Result<(), IndexerError> {
    match fmt {
        DataFmt::Json(path) => save_tree_json(tree, & path),
        DataFmt::Idx(path)  => save_tree_rmp(tree, & path)
    }
}

pub fn load_tree(fmt: DataFmt) -> Result<Arc<TreeNode>, IndexerError> {
    match fmt {
        DataFmt::Json(path) => load_tree_json(& path),
        DataFmt::Idx(path)  => load_tree_rmp(& path)
    }
}
