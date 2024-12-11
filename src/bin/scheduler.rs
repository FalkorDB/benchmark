use futures::future::join_all;
use std::ops::Add;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant};
use tracing::info;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Debug)]
struct Msg {
    start_time: Instant,
    // offset in milliseconds from start_time
    offset: u64,
}

#[tokio::main]
async fn main() {
    // Set up the tracing subscriber
    let filter = EnvFilter::from_default_env().add_directive(LevelFilter::INFO.into());
    let subscriber = fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .with_env_filter(filter);

    subscriber.init();

    // Create a channel for sending messages
    let (tx, rx) = tokio::sync::mpsc::channel::<Msg>(3);

    let handle = spawn_scheduler(50000, tx, 100000);

    let receiver: Arc<Mutex<Receiver<Msg>>> = Arc::new(Mutex::new(rx));
    let n_processors = 4;
    let processor_handles: Vec<JoinHandle<()>> = (0..n_processors)
        .map(|_| spawn_processor(&receiver))
        .collect();

    handle.await.unwrap();
    join_all(processor_handles).await;
}

fn spawn_processor(receiver: &Arc<Mutex<Receiver<Msg>>>) -> JoinHandle<()> {
    let receiver = Arc::clone(receiver);
    tokio::spawn(async move {
        let mut offset = 0;
        loop {
            let received = receiver.lock().await.recv().await;
            match received {
                None => {
                    info!("Received None, exiting, last offset was {:?}", offset);
                    return;
                }
                Some(received_msg) => {
                    offset = compute_offset_ms(&received_msg);
                    if offset > 0 {
                        // sleep offset millis
                        tokio::time::sleep(Duration::from_millis(offset as u64)).await;
                    } else {
                        // todo record metrics for late messages
                    }
                    // todo execute call
                }
            }
        }
    })
}

/// schedule at a rate of msg_per_sec messages per second to sender for number_of_messages
/// returns a handle to the spawned task
/// The actual send should be done as fast as possible,
/// but each message contain an offset from the start time which should
/// server as a deadline for the message to be processed or delay depending on the system at test speed
fn spawn_scheduler(
    msg_per_sec: u32,
    sender: Sender<Msg>,
    number_of_messages: u32,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval = 1000 / msg_per_sec as u64;
        // anchor the start time to 200 ms from now
        let start_time = Instant::now().add(Duration::from_millis(200));
        for count in 0..number_of_messages {
            let offset = count as u64 * interval;
            match sender.send(Msg { start_time, offset }).await {
                Ok(_) => {}
                Err(e) => {
                    info!("Error sending message: {}, exiting", e);
                    return;
                }
            }
        }
        info!("All messages sent");
    })
}

/// Compute the offset in milliseconds from the current time to the target time
#[inline]
fn compute_offset_ms(msg: &Msg) -> i64 {
    let current_time = Instant::now();
    let target_time = msg.start_time + Duration::from_millis(msg.offset);

    let duration = if current_time >= target_time {
        current_time - target_time
    } else {
        target_time - current_time
    };

    let offset_ms = duration.as_millis() as i64;

    if current_time >= target_time {
        -offset_ms
    } else {
        offset_ms
    }
}
