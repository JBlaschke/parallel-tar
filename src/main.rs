// Multi-threading
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Sender, Receiver, channel};
use std::{thread, time::Duration};

// Tar files
use std::fs::File;
use std::io::{self, Write};
use tar::{Builder, Header};
use walkdir::WalkDir;
use std::error::Error;

// Clap
use clap::{Arg, Command};

fn create_tar_archive(
        folder_path: & str, output_tar_path: & str
    ) -> Result<(), Box<dyn Error>> {

    let output_file = File::create(output_tar_path)?;
    let mut archive = Builder::new(output_file);

    for entry in WalkDir::new(folder_path).follow_links(true) {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            // let mut header = Header::new_gnu();
            // header.set_size(entry.metadata()?.len());
            // let _ = header.set_path(path.strip_prefix(folder_path)?.to_str().unwrap());
            // archive.append(&header, File::open(&path)?)?;
            println!("Adding: {}", path.display());
            archive.append_path(path).unwrap();
        }
    }

    archive.finish()?;
    Ok(())
}


fn walk_path(folder_path: & str) -> Result<(), Box<dyn Error>> {

    for entry in WalkDir::new(folder_path).follow_links(true) {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            println!("Looking at file: {}", path.display());
        }
    }

    Ok(())
}


// fn worker_thread(
//         output_tar_path: & str,
//         rx: Arc<Mutex<Receiver<String>>>,
//         tx: Sender<String>
//     ) -> Result<(), Box<dyn Error>> {
//     // Inside this function, you can repeatedly receive work from rx, process
//     // it, and send the results to tx.
//     let mut ct: u32 = 0;
//     let output_file = File::create(output_tar_path)?;
//     let mut archive = Builder::new(output_file);
// 
//     loop {
//         // Grab lock the the guard mutex, and take data from channel
//         let data = rx.lock().unwrap();
//         let datum = data.try_recv();
//         drop(data);
// 
//         match datum {
//             Ok(input) => {
//                 // Perform work on 'input' and produce 'output'.
//                 let output: String = format!("{} Processed {}: {}", id, ct, input);
// 
//                 // Send the result back to the main thread.
//                 tx.send(output).unwrap();
//                 ct += 1;
//             }
//             Err(_) => {
//                 // No data -- don't break (some might come a bit later), instead
//                 // just wait a fraction of second and try again. This worker is
//                 // meant to run until the parent stops.
//                 // break;
//                 thread::sleep(Duration::from_millis(128));
//             }
//         }
//     }
//     Ok(())
// }


fn worker_thread(id:u32, rx: Arc<Mutex<Receiver<String>>>, tx: Sender<String>) {
    // Inside this function, you can repeatedly receive work from rx, process
    // it, and send the results to tx.
    let mut ct: u32 = 0;

    loop {
        // Grab lock the the guard mutex, and take data from channel
        let data = rx.lock().unwrap();
        let datum = data.try_recv();
        drop(data);

        match datum {
            Ok(input) => {
                // Perform work on 'input' and produce 'output'.
                let output: String = format!("{} Processed {}: {}", id, ct, input);

                // Send the result back to the main thread.
                tx.send(output).unwrap();
                ct += 1;
            }
            Err(_) => {
                // No data -- don't break (some might come a bit later), instead
                // just wait a fraction of second and try again. This worker is
                // meant to run until the parent stops.
                // break;
                thread::sleep(Duration::from_millis(128));
            }
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
            .required(false)
            .num_args(0)
        )
        .arg(
            Arg::new("extract")
            .short('x')
            .long("extract")
            .help("Extract a list of archives")
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
        .get_matches();

    let target = args.get_one::<String>("target").unwrap();
    let archive_name = args.get_one::<String>("archive_name").unwrap();
    let create = args.get_one::<bool>("create").unwrap();
    let extract = args.get_one::<bool>("extract").unwrap();

    let num_threads = 4;
    // Create channels for sending work and receiving results.
    let (tx_work, rx_work) = channel();
    let (tx_results, rx_results) = channel();
    let shared_work = Arc::new(Mutex::new(rx_work));

    // Spawn worker threads.
    for idx in 0..num_threads {
        let rx = Arc::clone(& shared_work);
        let tx = tx_results.clone();
        thread::spawn(move || {
            worker_thread(idx, rx, tx);
        });
    }

    let work_items = vec![
        "Hi",
        "Ho",
        "Let's",
        "Go!",
        "For",
        "Some",
        "More"
    ];
    // Add work to the work channel.
    for work_item in & work_items {
        tx_work.send(work_item.to_string()).unwrap();
    }

    drop(tx_work);

    // Non-blocking (but patient) data collection
    let mut ct_recv = 0;
    loop {
        if ct_recv >= work_items.len() {
            break;
        }
        match rx_results.recv_timeout(Duration::from_millis(4000)) {
            Ok(result) => {
                ct_recv +=1 ;
                println!("Received: {}", result);
            }
            Err(_) => {
                break;
            }
        }
    }

    create_tar_archive(target, archive_name);

}
