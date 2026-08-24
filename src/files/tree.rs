// SPDX-License-Identifier: AGPL-3.0-or-later

//! Convert a saved index back into a work list for the archiver.
//!
//! This is the bridge used by `parallel-tar -t`: instead of walking the
//! filesystem a second time, the archiver replays the paths recorded in an
//! index (typically an "empty tree" `.etr` produced by `parallel-idx -e`).

use crate::index::serialize::{DataFmt, load_tree};
use crate::index::tree::NodeType;
use crate::files::path::analyze_path;

use std::io::Error;
// Logging
use log::{info, debug};
// Paths
use std::path::PathBuf;

/// Load the index at `index_path` (JSON if `json_fmt`, MessagePack `.idx`
/// otherwise) and flatten it into the list of paths to archive, sorted
/// largest-first so big entries are dispatched to workers early.
///
/// Returns `(base_dir, paths)`. When the index was built from an absolute
/// path, `base_dir` is `Some(parent_of_root)` and every returned path is
/// relative to it — the caller is expected to `chdir` there (mirroring how
/// `tar` stores relative entries). When the index was built from a relative
/// path, `base_dir` is `None` and the paths are returned as stored.
pub fn files_from_tree(
            json_fmt: &bool, index_path: &String
        ) -> Result<(Option<PathBuf>, Vec<String>), Error> {

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
            NodeType::Directory{children: _} => files.push(
                node.path.to_string_lossy().to_string()
            ),
            _ => ()
        };
    };

    let (base, _rel) = analyze_path(&tree.path.to_string_lossy().to_string())?;

    match base {
        Some(root_dir) => {
            //This stripping will work because the list of paths are generated
            //from a tree => they are all guaranteed to have the same prefix.
            debug!("Tree has prefix: '{}'", root_dir.to_string_lossy());
            let stripped_files: Result<Vec<String>, Error> = files
                .iter()
                .map(|s| {
                    PathBuf::from(s)
                        .strip_prefix(&root_dir)
                        .map(|x| x.to_string_lossy().to_string())
                        .map_err(|_| Error::new(
                            std::io::ErrorKind::InvalidData, "Invalid Prefix"
                        ))
                })
                .collect();
            return Ok((Some(root_dir), stripped_files?))
        },
        None => {debug!("Not changing working dir");}
    };

    Ok((None, files))
}
