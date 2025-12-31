// Stdlib
use std::error::Error;

// Clap
use clap::{Arg, Command};

mod index;
use crate::index::directory_tree::{TreeNode, format_size};

use rayon::ThreadPoolBuilder;


fn main() -> Result<(), Box<dyn Error>> {
    let args = Command::new("Indexer for Parallel Tar")
        .version("2.0")
        .author("Johannes Blaschke")
        .about("Add target directory to parallel list of Tar archives.")
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
            Arg::new("index_nmae")
            .short('f')
            .long("file")
            .help("Name of the index file")
            .required(true)
            .num_args(1)
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
    // let index_name: &String = get_arg(&args, "index_name")?;
    let num_threads: &u32          = get_arg(& args, "num_threads")?;
    let follow_links: &bool        = get_arg(& args, "follow_links")?;
    let valid_symlinks_only: &bool = get_arg(& args, "valid_symlinks_only")?;

    // Thread pool used for parallel work
    let nproc: usize = * num_threads as usize;
    let pool = ThreadPoolBuilder::new().num_threads(nproc).build()?;

    println!("Building tree for: {} using {} threads\n", target, nproc);

    let tree = TreeNode::from_path(
        & target, * follow_links, * valid_symlinks_only
    )?;

    // Compute sizes bottom-up from leaves to root

    // let total = tree.compute_sizes();
    let total = pool.install(|| {tree.compute_sizes_parallel()});

    //tree.print_tree("", true);

    // let (files, dirs) = tree.count();
    let (files, dirs) = pool.install(|| {tree.count_parallel()});

    println!(
        "\n{} files, {} directories, {} total",
        files,
        dirs,
        format_size(total)
    );

    // Show the 5 largest nodes
    println!("\n--- Largest Entries ---");
    let mut all_nodes: Vec<_> = tree.collect_all();
    all_nodes.sort_by(|a, b| b.get_computed_size().cmp(&a.get_computed_size()));
    for node in all_nodes.iter().take(5) {
        println!(
            "{}: {}",
            node.path.display(),
            format_size(node.get_computed_size())
        );
    }
    Ok(())
}
