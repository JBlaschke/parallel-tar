// Tar files
use tar::{Builder, Header, EntryType, Archive};

// Clap
use clap::{Arg, Command};

// ----
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Sender, Receiver, channel};
use std::thread::JoinHandle;
use std::{thread, time::Duration};

// File system
use std::fs::{File, symlink_metadata, read_link};
use std::path::Path;
// Stdlib
use std::error::Error;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt; // Import for Unix-specific permissions
// ----

use ptar_lib::archive::fs::{find_files, set_mode_from_path_or_default};

use ptar_lib::archive::tar::{create, extract};

fn main() -> Result<(), Box<dyn Error>> {
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

    if * create_mode {
        create(archive_name, target, num_threads, follow_links)?;
    } else if * extract_mode {
        extract(archive_name, target, num_threads)?;
    }

    Ok(())
}
