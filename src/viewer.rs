// SPDX-License-Identifier: AGPL-3.0-or-later

//! `view-idx` — inspect a `.idx`/`.etr`/`.json` index: summary metadata, a
//! tree view (depth-capped with `-L`, scoped to a sub-path with `-p`), and
//! the largest entries.

// Stdlib
use std::error::Error;
use std::sync::Arc;
// Clap
use clap::{Arg, ArgAction, Command};
// Paths
use std::path::PathBuf;
use ptar_lib::index::path::resolve_chain;

use ptar_lib::index::*;
use ptar_lib::index::tree::TreeNode;
use ptar_lib::index::serialize::{DataFmt, load_tree};
use ptar_lib::index::display::format_size;

fn main() -> Result<(), Box<dyn Error>> {
    let args = Command::new("Index viewer and search tool for Parallel Tar")
        .version("2.1")
        .author("Johannes Blaschke")
        .about("View and search Parallel Tar index files.")
        .arg(
            Arg::new("index_path")
                .short('f')
                .long("file")
                .help("Path of the index file")
                .required(true)
                .num_args(1)
        )
        .arg(
            Arg::new("json_fmt")
                .short('j')
                .long("json")
                .help("Input file is JSON, not msgpack-idx")
                .action(ArgAction::SetTrue)
        )
        .arg(
            Arg::new("max_depth")
                .short('L')
                .long("level")
                .value_name("DEPTH")
                .help("Maximum tree depth to print (0 = root only)")
                .value_parser(clap::value_parser!(usize))
                .required(false)
        )
        .arg(
            Arg::new("path")
                .short('p')
                .long("path")
                .value_name("PATH")
                .help("Sub-path within the index to view (matched by child name)")
                .required(false)
        )
        .get_matches();

    fn get_arg<'a, T: Clone + Send + Sync + 'static>(
            args:&'a clap::ArgMatches, name: &str
        ) -> Result<&'a T, String>{
        args.get_one::<T>(name).ok_or(format!("Failed to get: '{}'", name))
    }

    let index_path: &String = get_arg(& args, "index_path")?;
    let json_fmt:    bool   = args.get_flag("json_fmt");
    let max_depth:   usize  = args.get_one::<usize>("max_depth")
        .copied()
        .unwrap_or(usize::MAX);

    let data_fmt = if json_fmt {
        DataFmt::Json(index_path.to_string())
    } else {
        DataFmt::Idx(index_path.to_string())
    };

    println!("Loading index at: '{:?}'", data_fmt);
    let tree: Arc<TreeNode> = load_tree(data_fmt)?;

    // Resolve sub-path if given. The metadata, hash, and tree print below all
    // operate on the resolved node — the index-level summary (file/dir counts,
    // total size) reflects the subtree, not the whole index.
    let view_node: Arc<TreeNode> = match args.get_one::<String>("path") {
        None => Arc::clone(&tree),
        Some(p) => {
            let rel = PathBuf::from(p);
            let chain = resolve_chain(&tree, &rel)
                .map_err(|e| -> Box<dyn Error> { e.into() })?;
            chain.last().unwrap().clone()
        }
    };

    let meta = view_node.read_metadata().unwrap_or_default();
    let hash = view_node.read_hash().unwrap_or_default();

    println!("Done loading!");
    println!();

    // ─── Index metadata ──────────────────────────────────────────────
    println!("--- Index Metadata -------------------------------------------");
    println!("Source file: {}", index_path);
    println!("Root path:   {}", tree.path.display());
    println!("Root name:   {}", tree.name);
    if !Arc::ptr_eq(&view_node, &tree) {
        println!("Viewing:     {}", view_node.path.display());
    }
    println!("Root hash:   {}", hash);
    println!(
        "Contents:    {} files, {} directories, {} total",
        meta.files, meta.dirs, format_size(meta.size as u64)
    );
    if max_depth != usize::MAX {
        println!("Depth limit: {}", max_depth);
    }
    println!("--------------------------------------------------------------");
    println!();

    // ─── Tree ────────────────────────────────────────────────────────
    view_node.print_tree_depth("", true, max_depth);
    println!();

    // ─── Largest entries ─────────────────────────────────────────────
    println!("--- Largest Entries ------------------------------------------");
    let mut all_nodes: Vec<_> = view_node.collect_all();
    all_nodes.sort_by(
        |a, b| {
            let meta_a = a.read_metadata().unwrap_or_default();
            let meta_b = b.read_metadata().unwrap_or_default();
            meta_b.size.cmp(& meta_a.size)
    });
    for (i, node) in all_nodes.iter().take(5).enumerate() {
        let meta = node.read_metadata().unwrap_or_default();
        let hash = node.read_hash().unwrap_or_default();
        println!(
            "{}: {} is {} files + {} dirs ({}, {})",
            i, node.path.display(), meta.files, meta.dirs,
            format_size(meta.size as u64), format!("{:.16}", hash)
        );
    };
    println!("--------------------------------------------------------------");

    Ok(())
}
