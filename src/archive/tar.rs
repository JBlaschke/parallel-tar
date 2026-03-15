// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::files::path::{
    DirPlan, analyze_path, sanitize_rel_path, set_chmod_plan, apply_chmod_plan
};
use crate::files::tree::files_from_tree;
use crate::archive::mutex::Pipe;
use crate::archive::error::ArchiverError;
use crate::archive::fs::{is_symlink, set_mode_from_path_or_default, find_files};

// Tar files
use tar::{Builder, Header, EntryType, Archive};
// Compression
use flate2::Compression;
use flate2::write::GzEncoder;
use flate2::read::GzDecoder;
// File system
use std::fs::{File, read_link, create_dir_all};
// Multi-threading
use std::thread;
use std::thread::JoinHandle;
use std::sync::{Arc, Mutex};
// Logging
use log::{error, warn, info, debug};
// Working with cwd
use std::env::{current_dir, set_current_dir};
use std::path::{Path, PathBuf};
// Working with Boxed I/O (for compile-time compression flag)
use std::io::{Write, Read};
// Use HashSet to track the completed items, which makes later lookup faster
use std::collections::HashSet;

fn create_worker_thread(
            output_tar_path: &PathBuf,
            pipe_work: &Pipe<String>,
            pipe_results: &Pipe<Result<String, ArchiverError<String>>>,
            compress: &bool
        ) -> Result<(), ArchiverError<String>> {
    let output_file = File::create(output_tar_path)?;
    let writer: Box<dyn Write> = if *compress {
        Box::new(GzEncoder::new(output_file, Compression::default()))
    } else {
        Box::new(output_file)
    };
    let mut archive = Builder::new(writer);

    loop {
        match pipe_work.take_try_many() {
            Ok(input) => {
                if is_symlink(& input) {
                    // Symlink => configure header
                    let mut header = Header::new_gnu();
                    header.set_entry_type(EntryType::Symlink);
                    header.set_size(0);
                    // If there is an issue with reading the link (e.g. the file
                    // permissions), this will default to standard metadata and
                    // proceed with those
                    set_mode_from_path_or_default(&mut header, & input);
                    let link_target = match read_link(& input) {
                        Ok(v) => v,
                        Err(e) => {
                            pipe_results.input().send(Err(e.into()))?;
                            continue;
                        }
                    };
                    let _ = header.set_link_name(& link_target);
                    // Add link to tar
                    match archive.append_link(
                        &mut header, & input, & link_target
                    ) {
                        Ok(_)  => (),
                        Err(e) => {
                            pipe_results.input().send(Err(e.into()))?;
                            continue;
                        }
                    };
                } else {
                    // File => simply append file
                    match archive.append_path(input.clone()) {
                        Ok(_)  => (),
                        Err(e) => {
                            pipe_results.input().send(Err(e.into()))?;
                            continue;
                        }
                    }
                }
                // Used to check work that has been done
                pipe_results.input().send(Ok(input))?;
            },
            Err(error) => {
                // Check if work is done
                if pipe_work.get_completed()? {
                    return Ok(());
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
}

fn scan_dirs_worker_thread(
            tar_path: &str,
            destination: &str,
            pipe_results: &Pipe<String>,
            compress: &bool,
            priority: usize,
            plan: Arc<Mutex<DirPlan>>
        ) -> Result<(), ArchiverError<String>> {

    let input_file = File::open(tar_path)?;

    let reader: Box<dyn Read> = if *compress {
        Box::new(GzDecoder::new(input_file))
    } else {
        Box::new(input_file)
    };

    let destination_path = PathBuf::from(destination);
    let mut archive = Archive::new(reader);

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
        pipe_results.input().send(full_path.to_string_lossy().to_string())?;

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
            compress: &bool
        ) -> Result<(), ArchiverError<String>> {

    let input_file = File::open(tar_path)?;

    let reader: Box<dyn Read> = if *compress {
        Box::new(GzDecoder::new(input_file))
    } else {
        Box::new(input_file)
    };

    let mut archive = Archive::new(reader);

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

pub fn create(
            archive_name: &String, 
            target: &String,
            num_threads: &u32, 
            follow_links: &bool,
            from_tree: &bool,
            json_fmt: &bool,
            compress: &bool
        ) -> Result<(), ArchiverError<String>> {
    let pipe_work    = Pipe::<String>::new();
    let pipe_results = Pipe::<Result<String, ArchiverError<String>>>::new();

    let mut tfiles: Vec<String> = Vec::new();
    let (base, rel) = if *from_tree {
        let (tbase, ifiles) = files_from_tree(json_fmt, target)?;
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

    let archive_path = Path::new(&archive_dest);
    if archive_path.exists() {
        error!("Path '{}' not free", archive_dest.to_string_lossy());
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists, "Destination path not free."
        ).into());
    } else {
        debug!(
            "Creating destination folder: {}", archive_dest.to_string_lossy()
        );
        create_dir_all(&archive_dest)?;
    }

    // This must happen BEFORE the threads are spawned, otherwise they will fail
    // while trying to receive data from empty channels.
    info!("SETUP: Enumerating files. Following links? {}", follow_links);
    let work_items = if *from_tree {
        // files_from_tree(json_fmt, target)?
        tfiles
    } else {
        find_files(&rel, *follow_links)?
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
        let loc_work    = pipe_work.clone();
        let loc_results = pipe_results.clone();
        // Initiate worker thread and "point" them to `name.<thread>.tar`
        let name = if loc_compress {
            format!("{}.{}.tar.gz", archive_name, idx)
        } else {
            format!("{}.{}.tar", archive_name, idx)
        };
        let out = archive_dest.join(name);
        info!(
            "Starting worker thread: {} and writing to '{}'",
            idx, out.to_string_lossy()
        );
        handles.push(
            thread::spawn(move || -> Result<(), ArchiverError<String>> {
                match create_worker_thread(
                            &out, &loc_work, &loc_results, &loc_compress
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

    // Add work to the work channel
    info!("Sending paths to workers. This will start the archiving files...");
    for work_item in & work_items {
        debug!("Requesting '{}' be archived", work_item);
        pipe_work.tx.send(work_item.to_string()).unwrap_or_else( |err| {
            warn!("Failed to process '{}', due to error: '{}'", work_item, err)
        });
    }

    info!("Collecting worker status (workers are working) ...");
    let processed_items = pipe_results.collect_expected(work_items.len());
    pipe_work.set_completed()?;

    info!(" ... waiting for workers to finish ...");
    for h in handles {
        h.join().unwrap_or_else( |err| {
            warn!("Failed thread join: '{:?}'", err);
            Ok(())
        })?;
    }
    info!(" ... workers are done!");
    pipe_work.close();
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

pub fn extract(
            archive_name: &String,
            target: &String,
            num_threads: &u32,
            compress: &bool
        ) -> Result<(), ArchiverError<String>> {

    let plan = Arc::new(Mutex::new(DirPlan::default()));
    let loc_compress = *compress;

    // Spawn worker threads
    info!("Starting {} worker threads", num_threads);

    let pipe_dirs = Pipe::<String>::new();
    let mut scan_handles: Vec<
            JoinHandle<Result<(), ArchiverError<String>>>
        > = Vec::with_capacity(*num_threads as usize);

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
        let plan = plan.clone();
        let prio = idx as usize; // Use loop index to assign priorities;
        let ctarget = target.clone();
        scan_handles.push(
            thread::spawn(move || {
                match scan_dirs_worker_thread(
                    name.as_str(), ctarget.as_str(), &loc_dirs,
                    &loc_compress, prio, plan
                ) {
                    Err(e) => {
                        error!("Error from spawned 'scan' thread: '{}'", e);
                        // No more work due to error => terminate pipes
                        Err(e)
                    },
                    Ok(()) => Ok(())
                }
            })
        );
    }

    info!("... scanning for directories ...");
    for h in scan_handles {
        h.join().unwrap_or_else( |err| {
            warn!("Failed thread join: '{:?}'", err);
            Ok(())
        })?;
    }
    info!(" ... workers are done scanning directories!");
    pipe_dirs.set_completed()?;

    // IMPORTANT: collecting from this pipe _after_ join ensures that the
    // completion signal is sent before blocking on `collect`
    let dirs = pipe_dirs.collect_until_finished();
    pipe_dirs.close();

    info!("Creating directories ...");
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
    info!(" ... finished creating directories!");

    let mut extract_handles: Vec<
            JoinHandle<Result<(), ArchiverError<String>>>
        > = Vec::with_capacity(*num_threads as usize);
    for idx in 0..*num_threads {
        let name = if *compress {
            format!("{}.{}.tar.gz", archive_name, idx)
        } else {
            format!("{}.{}.tar", archive_name, idx)
        };
        let ctarget = target.clone();
        extract_handles.push(
            thread::spawn(move || {
                match extract_worker_thread(
                    name.as_str(), ctarget.as_str(), &loc_compress
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
    for h in extract_handles {
        h.join().unwrap_or_else( |err| {
            warn!("Failed thread join: '{:?}'", err);
            Ok(())
        })?;
    }
    info!(" ... workers are done!");

    info!("FINALIZE: ensuring file and directory permissions match archive.");
    let plan = Arc::try_unwrap(plan).expect("plan still shared").into_inner()?;
    apply_chmod_plan(plan)?;
    info!("DONE.");

    Ok(())
}
