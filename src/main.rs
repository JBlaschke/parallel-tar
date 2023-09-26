use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Sender, Receiver, channel};
use std::{thread, time::Duration};


fn worker_thread(id:u32, rx: Arc<Mutex<Receiver<String>>>, tx: Sender<String>) {
    // Inside this function, you can repeatedly receive work from rx,
    // process it, and send the results to tx.
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

}
