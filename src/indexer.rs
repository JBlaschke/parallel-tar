// Stdlib
use std::error::Error;

// Clap
use clap::{Arg, Command};

mod index;
use crate::index::directory_tree::{TreeNode, format_size, save_tree};

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
    let follow_links: &bool        = get_arg(& args, "follow_links")?;
    let valid_symlinks_only: &bool = get_arg(& args, "valid_symlinks_only")?;

    // Thread pool used for parallel work
    let nproc: usize = * num_threads as usize;
    let pool = ThreadPoolBuilder::new().num_threads(nproc).build()?;

    println!("Building tree for: '{}' using {} threads...", target, nproc);

    let tree = TreeNode::from_path(
        & target, * follow_links, * valid_symlinks_only
    )?;
    // Compute metadata bottom-up from leaves to root
    let meta = pool.install(|| {tree.compute_metadata()})?;

    // Display results
    println!(
        "Indexed: {} files, {} directories, {} total", 
        meta.files, meta.dirs, format_size(meta.size as u64)
    );

    // Show the 5 largest nodes
    println!("--- Largest Entries ---");
    let mut all_nodes: Vec<_> = tree.collect_all();
    all_nodes.sort_by(
        |a, b| {
            let meta_a = a.read_metadata().unwrap_or_default();
            let meta_b = b.read_metadata().unwrap_or_default();
            meta_b.size.cmp(& meta_a.size)
    });
    for node in all_nodes.iter().take(5) {
        let meta = node.read_metadata().unwrap_or_default();
        println!("{}", node.path.display());
        println!("├── {} files + {} dirs" , meta.files, meta.dirs);
        println!("└── {} " , format_size(meta.size as u64));
    };
    println!("-----------------------");

    println!("Saving index as: {}", index_path);
    let _ = save_tree(& tree, & index_path);

    Ok(())
}
