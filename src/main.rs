// SPDX-License-Identifier: AGPL-3.0-or-later
// Clap
use clap::{Arg, Command};
// Stdlib
use std::error::Error;

use ptar_lib::archive::tar::{create, extract};

fn main() -> Result<(), Box<dyn Error>> {
    // By default emit warnings
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn")
    ).init();

    let args = Command::new("Parallel Tar")
        .version("2.0")
        .author("Johannes Blaschke")
        .about("Add target directory to parallel list of Tar archives.")
        .arg(
            Arg::new("target")
            .value_name("TARGET")
            .help("Target for compression/decompression")
            .required(true)
            .index(1)
        )
        .arg(
            Arg::new("from_tree")
            .short('t')
            .long("tree")
            .help("Assemble archive from tree file (don't traverse directory)")
            .required(false)
            .num_args(0),
        )
        .arg(
            Arg::new("json_fmt")
            .short('j')
            .long("json")
            .help("Input index as JSON.")
            .required(false)
            .num_args(0)
            .requires("from_tree")
        )
        .arg(
            Arg::new("create")
            .short('c')
            .long("create")
            .help("Create an archive")
            .required_unless_present("extract")
            .num_args(0)
        )
        .arg(
            Arg::new("extract")
            .short('x')
            .long("extract")
            .help("Extract a list of archives")
            .required_unless_present("create")
            .num_args(0)
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
            Arg::new("archive_name")
            .short('f')
            .long("file")
            .help("Name of the Tar archive")
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
        .arg(
            Arg::new("compress")
            .short('z')
            .long("compress")
            .help("Work with compressed tar files")
            .required(false)
            .num_args(0)
        )
        .get_matches();

    fn get_arg<'a, T: Clone + Send + Sync + 'static>(
            args:&'a clap::ArgMatches, name: &str
        ) -> Result<&'a T, String>{
        args.get_one::<T>(name).ok_or(format!("Failed to get: '{}'", name))
    }

    let target: &String       = get_arg(&args, "target")?;
    let archive_name: &String = get_arg(&args, "archive_name")?;
    let num_threads: &u32     = get_arg(&args, "num_threads")?;
    let create_mode: &bool    = get_arg(&args, "create")?;
    let extract_mode: &bool   = get_arg(&args, "extract")?;
    let follow_links: &bool   = get_arg(&args, "follow_links")?;
    let from_tree: &bool      = get_arg(& args, "from_tree")?;
    let json_fmt: &bool       = get_arg(& args, "json_fmt")?;
    let compress: &bool       = get_arg(& args, "compress")?;

    if *create_mode {
        create(
            archive_name, target, num_threads, follow_links,
            from_tree, json_fmt, compress
        )?;
    } else if *extract_mode {
        extract(archive_name, target, num_threads, compress)?;
    }

    Ok(())
}
