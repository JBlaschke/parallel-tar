// Stdlib
use std::error::Error;
use std::sync::Arc;

// Clap
use clap::{Arg, Command};

mod index;
use crate::index::directory_tree::{TreeNode, format_size, load_tree};

fn main() -> Result<(), Box<dyn Error>> {
    let args = Command::new("Index viewer and search tool for Parallel Tar")
        .version("2.0")
        .author("Johannes Blaschke")
        .about("Add target directory to parallel list of Tar archives.")
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
        .get_matches();

    fn get_arg<'a, T: Clone + Send + Sync + 'static>(
            args:&'a clap::ArgMatches, name: &str
        ) -> Result<&'a T, String>{
        args.get_one::<T>(name).ok_or(format!("Failed to get: '{}'", name))
    }

    let index_path: &String = get_arg(& args, "index_path")?;
    println!("Loading index at: '{}'", index_path);
    let tree: Arc<TreeNode> = load_tree(index_path)?;
    let meta = tree.read_metadata().unwrap_or_default();

    println!("Done loading!");
    tree.print_tree("", true);

    println!(
        "Loaded index containing: {} files, {} directories, {} total",
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

    Ok(())
}
