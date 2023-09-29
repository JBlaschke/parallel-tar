// Multi-threading
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Sender, Receiver, channel, TryRecvError};
use std::thread::JoinHandle;
use std::{thread, time::Duration};

// Tar files
use std::fs::{File, symlink_metadata, read_link};
use std::os::unix::fs::PermissionsExt; // Import for Unix-specific permissions
use std::path::Path;
use tar::{Builder, Header, EntryType, Archive};
use walkdir::WalkDir;
use std::error::Error;


// Clap
use clap::{Arg, Command};


fn find_files(
        folder_path: & str, follow_links: bool
    ) -> Result<Vec<String>, Box<dyn Error>> {

    let mut files: Vec<String> = Vec::new();
    for entry in WalkDir::new(folder_path).follow_links(follow_links) {
        let entry = entry?;
        let path = entry.path();

        files.push(path.to_str().unwrap().to_string());
    }

    Ok(files)
}


fn set_mutex<T: Copy>(mutex: & Arc<Mutex<T>>, val: T) {
    let mut lock = mutex.lock().unwrap();
    * lock = val;
    drop(lock);
}


fn get_mutex<T: Copy>(mutex: & Arc<Mutex<T>>) -> T {
    let lock = mutex.lock().unwrap();
    let val = * lock;
    drop(lock);
    return val;
}


fn take_mutex_try_many<T>(
        rx: & Arc<Mutex<Receiver<T>>>,
        max_try: u32, wait: Duration,
        completed: & Arc<Mutex<bool>>
    ) -> Result<T, TryRecvError> {

    let mut ct = 0;
    loop {
        // Grab lock the the guard mutex, and take data from channel
        let data = rx.lock().unwrap();
        let datum = data.try_recv();
        drop(data);
        match datum {
            Ok(input) => {
                return Ok(input);
            }
            Err(error) => {
                if (ct > max_try) || get_mutex(& completed) {
                    return Err(error);
                }
                ct += 1;
                thread::sleep(wait);
            }
        }
    }
}


fn collect_expected<T>(ct_expect: usize, rx: Receiver<T>, wait: Duration) -> Vec<T> {
    let mut items: Vec<T> = Vec::new();
    // Non-blocking (but patient) data collection
    let mut ct_recv = 0;
    loop {
        if ct_recv >= ct_expect {
            break;
        }
        match rx.recv_timeout(wait) {
            Ok(result) => {
                items.push(result);
                ct_recv +=1 ;
            }
            Err(error) => {
                panic!(
                    "Failure {} while collecting {} out of {}",
                    error, ct_recv, ct_expect
                );
            }
        }
    }
    return items;
}


fn is_symlink(path_str: & str) -> bool {
    let path = Path::new(& path_str);
    path.symlink_metadata().map(
        |metadata| metadata.file_type().is_symlink()
    ).unwrap_or(false)
}


fn create_worker_thread(
        output_tar_path: & str,
        rx: Arc<Mutex<Receiver<String>>>,
        tx: Sender<String>,
        completed: Arc<Mutex<bool>>
    ) {

    let output_file = File::create(output_tar_path).unwrap();
    let mut archive = Builder::new(output_file);

    loop {
        match take_mutex_try_many(& rx, 100, Duration::from_millis(128), & completed) {
            Ok(input) => {
                if is_symlink(& input) {
                    let mut header = Header::new_gnu();
                    header.set_entry_type(EntryType::Symlink);
                    header.set_size(0);
                    header.set_mode(
                        symlink_metadata(& input).unwrap().permissions().mode()
                    );

                    let link_target = read_link(& input).unwrap();
                    let _ = header.set_link_name(& link_target);
                    archive.append_link(&mut header, & input, & link_target).unwrap();
                } else {
                    archive.append_path(input.clone()).unwrap();
                }
                // Used to check work that has been done
                tx.send(input).unwrap();
            }
            Err(error) => {
                // Check if work is done
                if get_mutex(& completed) {
                    return;
                }

                panic!(
                    "Failure {} on thread responsible for: {}",
                    error, output_tar_path
                );
            }
        }
    }
}


fn extract_worker_thread(tar_path: & str, destination: & str) {
    let mut ar = Archive::new(File::open(tar_path).unwrap());
    ar.unpack(destination).unwrap();
}


fn create(
        archive_name: & String, target: & String,
        num_threads: & u32, follow_links: & bool
    ) {
    // Create channels for sending work and receiving results
    let (tx_work, rx_work) = channel();
    let (tx_results, rx_results) = channel();
    let shared_work = Arc::new(Mutex::new(rx_work));
    // Used to signal threads to shut down (once work is complete)
    let work_completed = Arc::new(Mutex::new(false));

    // Spawn worker threads
    println!("Starting {} worker threads", num_threads);
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    for idx in 0..*num_threads {
        let rx = Arc::clone(& shared_work);
        let tx = tx_results.clone();
        let cmp = Arc::clone(& work_completed);
        let name = format!("{}.{}.tar", archive_name, idx);
        handles.push(
            thread::spawn(move || {
                create_worker_thread(name.as_str(), rx, tx, cmp);
            })
        );
    }

    println!("Enumerating files. Following links? {}", follow_links);
    let work_items = find_files(target, *follow_links).unwrap();
    // Add work to the work channel
    for work_item in & work_items {
        tx_work.send(work_item.to_string()).unwrap();
    }

    println!("Collecting worker status (workers are working) ...");
    let processed_items = collect_expected(
        work_items.len(), rx_results, Duration::from_millis(4000)
    );
    set_mutex(& work_completed, true);

    println!(" ... waiting for workers to finish ...");
    for h in handles {
        h.join().unwrap();
    }
    println!(" ... workers are done ...");
    drop(tx_work);

    println!("... checking worker status.");
    for i in &processed_items {
        if ! work_items.iter().any(|e| e == i ) {
            println!("Work item {} requested but not processed!", i)
        }
    }
}

fn main() {
    let args = Command::new("Parallel Tar")
        .version("1.0")
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

    let target = args.get_one::<String>("target").unwrap();
    let archive_name = args.get_one::<String>("archive_name").unwrap();
    let num_threads = args.get_one::<u32>("num_threads").unwrap();
    let create_mode = args.get_one::<bool>("create").unwrap();
    let extract_mode = args.get_one::<bool>("extract").unwrap();
    let follow_links = args.get_one::<bool>("follow_links").unwrap();

    if * create_mode {
        create(archive_name, target, num_threads, follow_links);
    } else if * extract_mode {
        
    }
}
