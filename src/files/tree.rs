use crate::index::serialize::{DataFmt, load_tree};
use crate::index::tree::NodeType;

use std::io::Error;
// Logging
use log::info;

pub fn files_from_tree(
            json_fmt: &bool, index_path: &String
        ) -> Result<Vec<String>, Error> {

    let data_fmt = if * json_fmt {
        DataFmt::Json(index_path.to_string())
    } else {
        DataFmt::Idx(index_path.to_string())
    };

    info!("Loading index at: '{:?}'", data_fmt);
    let tree = match load_tree(data_fmt) {
        Ok(t) => t,
        Err(_) => return Err(Error::new(
            std::io::ErrorKind::InvalidData, "Cannot load tree"
        ))
    };

    let mut all_nodes: Vec<_> = tree.collect_all();
    all_nodes.sort_by(
        |a, b| {
            let meta_a = a.read_metadata().unwrap_or_default();
            let meta_b = b.read_metadata().unwrap_or_default();
            meta_b.size.cmp(& meta_a.size)
    });

    let mut files: Vec<String> = Vec::new();
    for node in all_nodes.iter() {
        match &node.node_type {
            NodeType::File{size: _} => files.push(
                node.path.to_string_lossy().to_string()
            ),
            NodeType::Symlink{target: _} => files.push(
                node.path.to_string_lossy().to_string()
            ),
            // This is a bit funny: tar will package up empty folders, so also
            // add them to the list
            NodeType::Directory{children: c} => {
                if c.len() == 0 {
                    node.path.to_string_lossy().to_string();
                }
            },
            _ => ()
        };
    };

    Ok(files)
}
