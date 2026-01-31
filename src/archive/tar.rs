use crate::files::path::analyze_path;
use crate::archive::mutex::Pipe;
use crate::archive::error::ArchiverError;
use crate::archive::fs::{is_symlink, set_mode_from_path_or_default, find_files};

// Tar files
use tar::{Builder, Header, EntryType, Archive};
// File system
use std::fs::{File, read_link};
// Multi-threading
use std::thread;
use std::thread::JoinHandle;
// Logging
use log::{error, warn, info, debug};
// Working with cwd
use std::env::{current_dir, set_current_dir};
use std::path::PathBuf;

fn create_worker_thread(
            output_tar_path: &PathBuf,
            pipe_work: &Pipe<String>,
            pipe_results: &Pipe<Result<String, ArchiverError<String>>>,
        ) -> Result<(), ArchiverError<String>> {
    let output_file = File::create(output_tar_path)?;
    let mut archive = Builder::new(output_file);

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
            }
            Err(error) => {
                // Check if work is done
                if pipe_work.get_completed()? {
                    return Ok(());
                }
                // If not => return an error
                return Err(error.into());
            }
        }
    }
}

fn extract_worker_thread(tar_path: &str, destination: &str) {
    let mut ar = Archive::new(File::open(tar_path).unwrap());
    ar.unpack(destination).unwrap();
}

pub fn create(
            archive_name: &String, 
            target: &String,
            num_threads: &u32, 
            follow_links: &bool
        ) -> Result<(), ArchiverError<String>> {
    let pipe_work    = Pipe::<String>::new();
    let pipe_results = Pipe::<Result<String, ArchiverError<String>>>::new();

    let (base, rel) = analyze_path(target)?;
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

    // Spawn worker threads
    info!("Starting {} worker threads", num_threads);
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
        let out = archive_dest.join(format!("{}.{}.tar", archive_name, idx));
        info!(
            "Starting worker thread: {} and writing to '{}'",
            idx, out.to_string_lossy()
        );
        handles.push(
            thread::spawn(move || -> Result<(), ArchiverError<String>> {
                match create_worker_thread(&out, &loc_work, &loc_results) {
                    Err(e) => {
                        error!("Err {}", e);
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

    info!("Enumerating files. Following links? {}", follow_links);
    let work_items = find_files(&rel, *follow_links)?;
    // Add work to the work channel
    for work_item in & work_items {
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
    info!(" ... workers are done ...");
    pipe_work.close();
    pipe_results.set_completed()?;

    info!("... checking worker status.");
    let mut successfully_processed: Vec<String> = vec![];
    for i in processed_items {
        match i {
            Ok(n) => successfully_processed.push(n),
            Err(e) => warn!("Worker returned error: '{}'", e)
        };
    }
    for i in work_items {
        if ! successfully_processed.iter().any(|e| *e == i) {
            warn!("Work item {} requested but not processed!", i);
        } else {
            debug!("Work item {} requested and processed", i);
        }
    }

    Ok(())
}

pub fn extract(
            archive_name: &String, target: &String, num_threads: &u32
        ) -> Result<(), ArchiverError<String>> {

    // Spawn worker threads
    info!("Starting {} worker threads", num_threads);
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    for idx in 0..*num_threads {
        let name = format!("{}.{}.tar", archive_name, idx);
        let ctarget = target.clone();
        handles.push(
            thread::spawn(move || {
                extract_worker_thread(name.as_str(), ctarget.as_str());
            })
        );
    }

    info!(" ... waiting for workers to finish ...");
    for h in handles {
        h.join().unwrap_or_else( |err| {
            warn!("Failed thread join: '{:?}'", err)
        });
    }
    info!(" ... workers are done.");

    Ok(())
}
