use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tokio::sync::{mpsc, Mutex};
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .init();

    // Create a channel with a buffer size of 32.
    let (tx, rx) = mpsc::channel::<i32>(2);
    let rx: Arc<Mutex<Receiver<i32>>> = Arc::new(Mutex::new(rx)); // Wrap the receiver in Arc<Mutex>

    let mut handles = vec![];
    // Spawn three worker tasks.
    for i in 0..30 {
        handles.push(spawn_worker(&rx, i));
    }

    // Simulate sending data from a stream
    for i in 0..1000 {
        tx.send(i).await.unwrap();
        info!("Sent: {}", i);
        // sleep(Duration::from_millis(500)).await; // Optional delay
    }

    // Close the sender when done sending messages
    drop(tx); // This will close the channel when all senders are dropped

    // Wait for all workers to finish processing
    for handle in handles {
        handle.await.unwrap(); // Await each worker's completion
    }
}

fn spawn_worker(
    rx: &Arc<Mutex<Receiver<i32>>>,
    worker_id: usize,
) -> tokio::task::JoinHandle<()> {
    let rx_worker = Arc::clone(&rx);
    tokio::spawn(async move {
        loop {
            let mut receiver = rx_worker.lock().await; // Lock the mutex to access the receiver
            match receiver.recv().await {
                Some(message) => {
                    info!("Worker {} received: {}", worker_id, message);
                    // Simulate some work
                    // sleep(Duration::from_secs(1)).await;
                }
                None => break,
            }
        }
    })
}
