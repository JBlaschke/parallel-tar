// SPDX-License-Identifier: AGPL-3.0-or-later
// Stdlib
use std::error::Error;

// Clap
use clap::{Arg, Command};

use ptar_lib::index::*;
use ptar_lib::index::tree::TreeNode;
use ptar_lib::index::serialize::{DataFmt, save_tree};
use ptar_lib::index::display::format_size;

use rayon::ThreadPoolBuilder;

use env_logger;

fn main() -> Result<(), Box<dyn Error>> {
    // By default emit warnings
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn")
    ).init();

    let args = Command::new("Indexer for Parallel Tar")
        .version("2.0")
        .author("Johannes Blaschke")
        .about("Create an index of files in a directory structure")
        .arg(
            Arg::new("target")
            .value_name("TARGET")
            .help("Target for indexing")
            .required(true)
            .index(1)
        )
        .arg(
            Arg::new("follow_links")
            .short('l')
            .long("follow")
            .help("Follow links while enumerating files")
            .required(false)
            .num_args(0)
        )
        .arg(
            Arg::new("valid_symlinks_only")
            .short('s')
            .long("valid")
            .help("Only include valid symlinks")
            .required(false)
            .num_args(0)
        )
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
            .help("Output index as JSON.")
            .required(false)
            .num_args(0)
        )
        .arg(
            Arg::new("use_md5")
            .short('m')
            .long("md5")
            .help("Use MD5 (instead of SHA256) to calculate checksums")
            .required(false)
            .num_args(0)
        )
        .arg(
            Arg::new("num_threads")
            .short('n')
            .help("Number of parallel threads to use")
            .required(true)
            .num_args(1)
            .value_parser(clap::value_parser!(u32))
        )
        .get_matches();

    fn get_arg<'a, T: Clone + Send + Sync + 'static>(
            args:&'a clap::ArgMatches, name: &str
        ) -> Result<&'a T, String>{
        args.get_one::<T>(name).ok_or(format!("Failed to get: '{}'", name))
    }

    let target: &String            = get_arg(& args, "target")?;
    let index_path: &String        = get_arg(& args, "index_path")?;
    let num_threads: &u32          = get_arg(& args, "num_threads")?;
    let json_fmt: &bool            = get_arg(& args, "json_fmt")?;
    let use_md5: &bool             = get_arg(& args, "use_md5")?;
    let follow_links: &bool        = get_arg(& args, "follow_links")?;
    let valid_symlinks_only: &bool = get_arg(& args, "valid_symlinks_only")?;

    // Thread pool used for parallel work
    let nproc: usize = * num_threads as usize;
    let pool = ThreadPoolBuilder::new().num_threads(nproc).build()?;

    println!("Building tree for: '{}' using {} threads...", target, nproc);

    let tree = TreeNode::from_path(
        & target, * follow_links, * valid_symlinks_only
    )?;
    println!("Computing metadata ...");
    // Compute metadata bottom-up from leaves to root
    let meta = pool.install(|| {tree.compute_metadata()})?;
    println!("Computing hashes ...");
    // Compute hashes bottom-up from leaves to root
    let hash = pool.install(|| {tree.compute_hashes(*use_md5)})?;

    // Display results
    println!(
        "Indexed: {} files, {} directories, {} total", 
        meta.files, meta.dirs, format_size(meta.size as u64)
    );

    println!("Root hash: '{}'", hash);

    // Show the 5 largest nodes
    println!("--- Largest Entries ------------------------------------------");
    let mut all_nodes: Vec<_> = tree.collect_all();
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

    let data_fmt = if * json_fmt {
        DataFmt::Json(index_path.to_string())
    } else {
        DataFmt::Idx(index_path.to_string())
    };
    println!("Saving index: '{:?}'", data_fmt);
    let _ = save_tree(& tree, data_fmt);

    Ok(())
}
