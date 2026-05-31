// SPDX-License-Identifier: AGPL-3.0-or-later
// Clap
use clap::{Arg, ArgGroup, Command, value_parser};
// Stdlib
use std::error::Error;
use std::path::{Path, PathBuf};
// Logging
use log::info;

use ptar_lib::archive::tar::{create, extract};

fn main() -> Result<(), Box<dyn Error>> {
    // By default emit warnings
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info")
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
            Arg::new("verify")
            .short('i')
            .long("verify")
            .help("Verify the correctness of an archive by generating an index from its contents")
            .required_unless_present_any(["create", "extract"])
            .num_args(0)
        )
        .group(
            ArgGroup::new("json_context")
                .args(["from_tree", "verify"])
                .multiple(true)     // allow both at once if that makes sense
                .required(false),   // group only enforced via `requires` below
        )
        .arg(
            Arg::new("json_fmt")
            .short('j')
            .long("json")
            .help("Input/output index as JSON.")
            .required(false)
            .num_args(0)
            .requires("json_context")   // satisfied by from_tree OR verify
        )
        .arg(
            Arg::new("create")
            .short('c')
            .long("create")
            .help("Create an archive")
            .required_unless_present_any(["verify","extract"])
            .num_args(0)
        )
        .arg(
            Arg::new("extract")
            .short('x')
            .long("extract")
            .help("Extract a list of archives")
            .required_unless_present_any(["create", "verify"])
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
        .arg(
            Arg::new("root")
            .short('r')
            .long("root")
            .help("Optional root prefix to prepend to tar-relative paths")
            .value_name("PATH")
            .value_parser(value_parser!(PathBuf))
        )
        .arg(
            Arg::new("use_md5")
            .short('m')
            .long("md5")
            .help("Use MD5 (instead of SHA256) to calculate checksums")
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
    let verify_mode: &bool    = get_arg(&args, "verify")?;
    let follow_links: &bool   = get_arg(&args, "follow_links")?;
    let from_tree: &bool      = get_arg(&args, "from_tree")?;
    let json_fmt: &bool       = get_arg(&args, "json_fmt")?;
    let compress: &bool       = get_arg(&args, "compress")?;
    let use_md5: &bool        = get_arg(&args, "use_md5")?;

    let root:    Option<&PathBuf> = args.get_one::<PathBuf>("root");

    if *create_mode {
        create(
            archive_name, target, num_threads, follow_links,
            from_tree, json_fmt, compress
        )?;
    } else if *extract_mode {
        extract(archive_name, target, num_threads, compress)?;
    } else if *verify_mode {
        use ptar_lib::archive::verify::verify;
        use ptar_lib::index::serialize::{save_tree, DataFmt};
        use ptar_lib::index::HashedNodes;            // for compute_hashes

        let root_path: Option<&Path> = root.map(|p| p.as_path());

        let tree = verify(
            target, num_threads, compress, use_md5, root_path,
        )?;

        // metadata is uncomputed; root hash should be computed once.
        tree.compute_metadata()?;
        // File hashes are pre-filled from the tar stream; this only fills in
        // the directory hashes (via the children-concat algorithm).
        match tree.compute_hashes(*use_md5)? {
            Some(h) => info!("Root hash: '{}'", h),
            None    => return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "verify produced an incomplete tree (some tar entries lacked hashes)",
            ).into()),
        }

        let fmt = if *json_fmt {
            DataFmt::Json(archive_name.to_string())
        } else {
            DataFmt::Idx(archive_name.to_string())
        };
        info!("Saving index: '{:?}'", fmt);
        save_tree(&tree, fmt)?;
        return Ok(());
    }

    Ok(())
}
