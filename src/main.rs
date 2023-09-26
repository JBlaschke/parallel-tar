use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Sender, Receiver, channel};
use std::{thread, time::Duration};


fn worker_thread(id:u32, rx: Arc<Mutex<Receiver<String>>>, tx: Sender<String>) {
    // Inside this function, you can repeatedly receive work from rx,
    // process it, and send the results to tx.
    let mut ct: u32 = 0;

    loop {
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

                // thread::sleep(Duration::from_millis(4000));
            }
            Err(_) => {
                break;
            }
        }
    }
}



fn main() {
    let num_threads = 2;
    // Create channels for sending work and receiving results.
    let (tx_work, rx_work) = channel();
    let (tx_results, rx_results) = channel();
    let shared_work = Arc::new(Mutex::new(rx_work));

    // Spawn worker threads.
    for idx in 0..num_threads {
        // let rx = rx_work.clone();
        let rx = Arc::clone(& shared_work);
        let tx = tx_results.clone();
        // let (_, rx, tx, _) = chd.get();  
        // let tx = chd.global_outputs.0.clone();
        thread::spawn(move || {
            worker_thread(idx, rx, tx);
        });
    }

    let work_items = vec![
        "Hi",
        "Ho",
        "Let's",
        "Go!"
    ];
    // Add work to the work channel.
    for work_item in & work_items {
        // let (tx_work, _, _ , _) = chd.get();
        tx_work.send(work_item.to_string()).unwrap();
    }

    // Collect results from the results channel.
    for _ in 0..work_items.len() {
        let result = rx_results.recv().unwrap();
        // let result = chd.global_outputs.1.recv().unwrap();
        println!("Received: {}", result);
    }

    drop(tx_work);

}
