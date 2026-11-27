// SPDX-License-Identifier: AGPL-3.0-or-later

//! Create and extract sharded tar archives with worker threads.
//!
//! [`create`] fans a work list of paths out to `n` workers, each writing its
//! own shard `<name>.<worker>.tar[.gz]` inside the destination directory.
//! [`extract`] runs the reverse in two phases: all shards are first scanned
//! for directories (created up front, with restrictive permissions deferred
//! via [`DirPlan`]), then each worker unpacks one shard's files.

use crate::files::path::{
    DirPlan, analyze_path, sanitize_rel_path, set_chmod_plan, apply_chmod_plan
};
use crate::files::tree::files_from_tree_with_retries;
use crate::archive::mutex::Pipe;
use crate::archive::error::ArchiverError;
use crate::archive::entry::{append_with_retries, EntryWriter};
use crate::archive::join_workers;
use crate::archive::fs::{find_files_with_retries, open_shard, set_mode_from_metadata};
use crate::retry::TimeoutRetries;

// Tar files
use tar::{Builder, Header, EntryType};
// Compression
use flate2::Compression;
use flate2::write::GzEncoder;
// File system
use std::fs::{self, File, read_link, create_dir_all};
// Multi-threading
use std::thread;
use std::thread::JoinHandle;
use std::sync::{Arc, Mutex};
// Logging
use log::{error, warn, info, debug};
// Working with cwd
use std::env::{current_dir, set_current_dir};
use std::path::{Path, PathBuf};
use std::io::{self, Write};
// Use HashSet to track the completed items, which makes later lookup faster
use std::collections::HashSet;

fn create_worker_thread(
            output_tar_path: &Path,
            pipe_work: &Pipe<String>,
            pipe_results: &Pipe<Result<String, ArchiverError<String>>>,
            compress: bool,
            max_retries: Option<u64>,
        ) -> Result<(), ArchiverError<String>> {
    let partial = output_tar_path.with_extension(if compress { "gz.partial" } else { "tar.partial" });
    let mut file = File::create_new(&partial)?;
    if compress {
        let mut encoder = GzEncoder::new(file, Compression::default());
        write_shard(&mut encoder, pipe_work, pipe_results, max_retries)?;
        encoder.finish()?.flush()?;
    } else {
        write_shard(&mut file, pipe_work, pipe_results, max_retries)?;
        file.flush()?;
        drop(file);
    }
    fs::rename(partial, output_tar_path)?;
    Ok(())
}

fn append_entry<W: Write>(archive: &mut Builder<W>, path: &Path) -> io::Result<()> {
    let metadata = path.symlink_metadata()?;
    if metadata.is_symlink() {
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Symlink);
        header.set_size(0);
        set_mode_from_metadata(&mut header, path, &metadata);
        archive.append_link(&mut header, path, read_link(path)?)
    } else {
        archive.append_path(path)
    }
}

fn write_shard<W: Write>(
    writer: W,
    pipe_work: &Pipe<String>,
    pipe_results: &Pipe<Result<String, ArchiverError<String>>>,
    max_retries: Option<u64>,
) -> Result<(), ArchiverError<String>> {
    let mut archive = Builder::new(EntryWriter::new(writer));
    loop {
        match pipe_work.take_try_many() {
            Ok(input) => {
                let path = Path::new(&input);
                if let Err(e) = append_with_retries(
                    &mut archive, path, max_retries,
                    |archive| append_entry(archive, path), thread::sleep,
                ) {
                    let error = io::Error::new(e.kind(), format!("archiving {path:?}: {e}"));
                    if !archive.get_ref().can_skip_entry()
                        || (max_retries != Some(0) && e.kind() == io::ErrorKind::TimedOut)
                    {
                        return Err(error.into());
                    }
                    pipe_results.send(Err(error.into()))?;
                    continue;
                }
                // Used to check work that has been done
                pipe_results.send(Ok(input))?;
            },
            Err(ArchiverError::ChannelClosed) => break,
            Err(error) => {
                // Check if work is done
                if pipe_work.get_completed()? {
                    break;
                }
                // If not => log the error and wait for the channel to be set to
                // completed
                debug!(
                    "'take_try_many' returned error: '{}'. Pipe not marked as \
                     completed => ignoring",
                     error
                )
            }
        }
    }
    archive.finish()?;
    Ok(())
}

fn scan_dirs_worker_thread(
            tar_path: &str,
            destination: &str,
            pipe_results: &Pipe<String>,
            compress: &bool,
            priority: usize,
            plan: Arc<Mutex<DirPlan>>,
            retries: &mut TimeoutRetries,
        ) -> Result<(), ArchiverError<String>> {

    let destination_path = PathBuf::from(destination);
    let mut archive = open_shard(tar_path, *compress, retries)?;

    for entry_res in archive.entries()? {
        let entry = entry_res?;
        if entry.header().entry_type() != EntryType::Directory {
            continue;
        }

        let rel = match entry.path() {
            Ok(p) => sanitize_rel_path(&p),
            Err(_) => None,
        };
        let Some(rel) = rel else {
            warn!("Did not extract: '{:?}', path is unsafe", entry.path());
            // skip unsafe paths
            continue;
        };

        let full_path = destination_path.join(&rel);
        pipe_results.send(full_path.to_string_lossy().to_string())?;

        if let Ok(mode) = entry.header().mode() {
            // Match tar crate default-ish: ignore suid/sgid/sticky unless you
            // truly want them.
            let mode = mode & 0o777;
            let mut guard = plan.lock()?;
            set_chmod_plan(&mut guard, &full_path, mode, priority)?;
        }
    }

    Ok(())
}

fn extract_worker_thread(
            tar_path: &str,
            destination: &str,
            compress: &bool,
            retries: &mut TimeoutRetries,
        ) -> Result<(), ArchiverError<String>> {

    let mut archive = open_shard(tar_path, *compress, retries)?;

    for entry_res in archive.entries()? {
        let mut entry = entry_res?;

        // Directories treated seperately
        if entry.header().entry_type() == EntryType::Directory {
            continue;
        }

        let rel = match entry.path() {
            Ok(p) => sanitize_rel_path(&p),
            Err(_) => None,
        };
        let Some(_rel) = rel else {
            warn!("Did not extract: '{:?}', path is unsafe", entry.path());
            // skip unsafe paths
            continue;
        };

        // Extract safely (tar crate handles traversal checks, symlinks, etc.)
        entry.unpack_in(destination)?;
    }

    Ok(())
}

/// Create a sharded archive of `target` using `num_threads` workers.
///
/// `target` is either a directory to walk, or — when `from_tree` — an index
/// file (`.etr`/`.idx`, JSON if `json_fmt`) whose recorded paths are
/// archived instead. Shards are written to a freshly-created directory
/// named `archive_name` (it is an error if that path already exists),
/// gzip-compressed when `compress`. When the target resolves to an absolute
/// path, the process `chdir`s to its parent so tar entries are stored
/// relative to it.
///
/// Every requested path is checked off against worker acknowledgements at
/// the end; paths that vanished mid-run produce warnings rather than
/// errors.
pub fn create(
            archive_name: &String, 
            target: &String,
            num_threads: &u32, 
            follow_links: &bool,
            from_tree: &bool,
            json_fmt: &bool,
            compress: &bool
        ) -> Result<(), ArchiverError<String>> {
    create_with_retries(
        archive_name, target, num_threads, follow_links, from_tree, json_fmt, compress, Some(0),
    )
}

/// Like [`create`], with a timeout retry limit per source entry (`None` for unlimited).
pub fn create_with_retries(
    archive_name: &String,
    target: &String,
    num_threads: &u32,
    follow_links: &bool,
    from_tree: &bool,
    json_fmt: &bool,
    compress: &bool,
    max_retries: Option<u64>,
) -> Result<(), ArchiverError<String>> {
    let mut pipe_work = Pipe::<String>::new();
    let mut pipe_results = Pipe::<Result<String, ArchiverError<String>>>::new();

    let mut tfiles: Vec<String> = Vec::new();
    let (base, rel) = if *from_tree {
        let (tbase, ifiles) = files_from_tree_with_retries(json_fmt, target, max_retries)?;
        tfiles = ifiles;
        (tbase, PathBuf::new()) // IMPORTANT: 'rel' not used if building from tree
    } else {
        analyze_path(target)?
    };
    let mut archive_dest = PathBuf::new();

    match base {
        Some(root_dir) => {
            info!(
                "Setting current working dir to: '{}'",
                root_dir.to_string_lossy()
            );
            let cwd = current_dir()?;
            let _ = set_current_dir(root_dir)?;
            archive_dest.push(cwd);
            archive_dest.push(archive_name)
        },
        None => {
            debug!("Not changing working dir");
            archive_dest.push(archive_name)
        }
    };

    info!("Saving archive to: '{}'", archive_dest.to_string_lossy());

    if let Some(parent) = archive_dest.parent() {
        create_dir_all(parent)?;
    }
    // Claim the shard directory atomically, including on concurrent create runs.
    fs::create_dir(&archive_dest)?;
    let stem = archive_dest.file_name().and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing archive name"))?;

    // This must happen BEFORE the threads are spawned, otherwise they will fail
    // while trying to receive data from empty channels.
    info!("SETUP: Enumerating files. Following links? {}", follow_links);
    let work_items = if *from_tree {
        tfiles
    } else {
        find_files_with_retries(&rel, *follow_links, max_retries)?
    };

    // Spawn worker num_threads
    let loc_compress: bool = *compress;
    info!("SETUP: Starting {} worker threads", num_threads);
    let mut handles: Vec<
            JoinHandle<Result<(), ArchiverError<String>>>
       > = Vec::with_capacity(*num_threads as usize);
    for idx in 0..*num_threads {
        // Per-thread (local) copies of the work and results pipes => avoid
        // moving their originals out of this scope by the `move` closure in
        // `thread::spawn`
        let mut loc_work = pipe_work.clone();
        loc_work.close(); // Workers only receive work.
        let loc_results = pipe_results.clone();
        // Initiate worker thread and "point" them to `name.<thread>.tar`
        let name = if loc_compress {
            format!("{stem}.{idx}.tar.gz")
        } else {
            format!("{stem}.{idx}.tar")
        };
        let out = archive_dest.join(name);
        info!(
            "Starting worker thread: {} and writing to '{}'",
            idx, out.to_string_lossy()
        );
        handles.push(
            thread::spawn(move || -> Result<(), ArchiverError<String>> {
                match create_worker_thread(
                            &out, &loc_work, &loc_results, loc_compress, max_retries
                        ) {
                    Err(e) => {
                        error!("Error from spawned 'create' thread: '{}'", e);
                        // No more work due to error => terminate pipes
                        loc_work.set_completed()?;
                        loc_results.set_completed()?;
                        Err(e)
                    },
                    Ok(()) => Ok(())
                }
            })
        );
    }

    pipe_results.close(); // Only workers send results.
    // Add work to the work channel
    info!("Sending paths to workers. This will start the archiving files...");
    for work_item in & work_items {
        debug!("Requesting '{}' be archived", work_item);
        pipe_work.send(work_item.to_string()).unwrap_or_else( |err| {
            warn!("Failed to process '{}', due to error: '{}'", work_item, err)
        });
    }
    // Workers see ChannelClosed once they have drained the submitted work.
    pipe_work.close();

    info!("Collecting worker status (workers are working) ...");
    let processed_items = pipe_results.collect_expected(work_items.len());
    pipe_work.set_completed()?;

    info!(" ... waiting for workers to finish ...");
    join_workers(handles)?;
    info!(" ... workers are done!");
    pipe_results.set_completed()?;

    info!("FINALIZE: checking worker status.");
    let mut successfully_processed: HashSet<String> = HashSet::with_capacity(
        processed_items.len()
    );
    for i in processed_items {
        match i {
            Ok(n) => {
                let _ = successfully_processed.insert(n);
            },
            Err(e) => warn!("Worker returned error: '{}'", e)
        };
    }
    for i in work_items {
        if ! successfully_processed.contains(&i) {
            warn!("Work item {} requested but not processed!", i);
        } else {
            debug!("Work item {} requested and processed", i);
        }
    }
    info!("DONE.");
    Ok(())
}

/// Extract a sharded archive into the `target` directory using
/// `num_threads` workers (one per shard `<archive_name>.<i>.tar[.gz]`).
///
/// Runs in two phases: (1) all shards are scanned and every directory is
/// created up front — restrictive directory modes are deferred in a
/// [`DirPlan`] so files can still be written; (2) each worker unpacks its
/// shard's non-directory entries. Entry paths are sanitized
/// ([`sanitize_rel_path`]) so no entry can escape `target`; the deferred
/// directory permissions are applied last, deepest-first.
pub fn extract(
            archive_name: &String,
            target: &String,
            num_threads: &u32,
            compress: &bool
        ) -> Result<(), ArchiverError<String>> {
    extract_with_retries(archive_name, target, num_threads, compress, Some(0))
}

/// Like [`extract`], with one timeout retry limit per shard across both passes.
pub fn extract_with_retries(
    archive_name: &String,
    target: &String,
    num_threads: &u32,
    compress: &bool,
    max_retries: Option<u64>,
) -> Result<(), ArchiverError<String>> {
    let plan = Arc::new(Mutex::new(DirPlan::default()));
    let loc_compress = *compress;

    // Spawn worker threads
    info!("SETUP for PHASE 1: Starting {} worker threads", num_threads);

    let mut pipe_dirs = Pipe::<String>::new();
    let mut scan_handles = Vec::with_capacity(*num_threads as usize);

    for idx in 0..*num_threads {
        // Per-thread (local) copies of the work and results pipes => avoid
        // moving their originals out of this scope by the `move` closure in
        // `thread::spawn`
        let loc_dirs = pipe_dirs.clone();
        let name = if *compress {
            format!("{}.{}.tar.gz", archive_name, idx)
        } else {
            format!("{}.{}.tar", archive_name, idx)
        };
        info!(
            "Starting worker thread: {} and scanning all folders in: '{}'",
            idx, name
        );
        let plan = plan.clone();
        let prio = idx as usize; // Use loop index to assign priorities;
        let ctarget = target.clone();
        scan_handles.push(
            thread::spawn(move || {
                let mut retries = TimeoutRetries::new(max_retries);
                match scan_dirs_worker_thread(
                    name.as_str(), ctarget.as_str(), &loc_dirs,
                    &loc_compress, prio, plan, &mut retries
                ) {
                    Err(e) => {
                        error!("Error from spawned 'scan' thread: '{}'", e);
                        // No more work due to error => terminate pipes
                        Err(e)
                    },
                    Ok(()) => Ok(retries)
                }
            })
        );
    }

    info!("... waiting for directories to be scanned ...");
    let shard_retries = join_workers(scan_handles)?;
    info!(" ... workers are done scanning directories!");
    pipe_dirs.set_completed()?;
    // The workers (the producers) are joined => their pipe clones are gone.
    // Closing this last handle before collecting fully disconnects the
    // channel, so `collect` stops as soon as the buffered data is drained.
    pipe_dirs.close();

    // IMPORTANT: collecting from this pipe _after_ join ensures that the
    // completion signal is sent before blocking on `collect`
    let dirs = pipe_dirs.collect_until_finished();

    info!("PHASE 1: Creating directories ...");
    let mut created_dirs: HashSet<String> = HashSet::with_capacity(
        dirs.len()
    );
    for i in dirs {
        if created_dirs.contains(&i) {
            continue;
        }

        let _ = create_dir_all(&i);
        let _ = created_dirs.insert(i);
    }
    info!(" ... finished creating directories! (PHASE 1)");

    info!("SETUP for PHASE 2: Starting {} worker threads", num_threads);
    let mut extract_handles: Vec<
            JoinHandle<Result<(), ArchiverError<String>>>
        > = Vec::with_capacity(*num_threads as usize);
    for (idx, mut retries) in shard_retries.into_iter().enumerate() {
        let name = if *compress {
            format!("{}.{}.tar.gz", archive_name, idx)
        } else {
            format!("{}.{}.tar", archive_name, idx)
        };
        info!(
            "Starting worker thread: {} and extracting all files in: '{}'",
            idx, name
        );
        let ctarget = target.clone();
        extract_handles.push(
            thread::spawn(move || {
                match extract_worker_thread(
                    name.as_str(), ctarget.as_str(), &loc_compress, &mut retries
                ) {
                    Err(e) => {
                        error!("Error from spawned 'extract' thread: '{}'", e);
                        // No more work due to error => terminate pipes
                        Err(e)
                    },
                    Ok(()) => Ok(())
                }
            })
        );
    }

    info!(" ... waiting for workers to finish ...");
    join_workers(extract_handles)?;
    info!(" ... workers are done! (PHASE 2)");

    info!("FINALIZE: ensuring file and directory permissions match archive.");
    let plan = Arc::try_unwrap(plan).expect("plan still shared").into_inner()?;
    apply_chmod_plan(plan)?;
    info!("DONE.");

    Ok(())
}
