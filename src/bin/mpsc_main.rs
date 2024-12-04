use futures::stream::{self};
use futures::stream::{FuturesUnordered, StreamExt};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tracing::info;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .pretty()
        .with_file(true)
        .with_line_number(true)
        .init();
    let (tx1, mut rx1) = mpsc::channel::<i32>(2);
    let r = Arc::new(Mutex::new(rx1));

    let stream = stream::iter(vec![17, 19]);
    // assert_eq!(vec![17, 19], stream.collect::<Vec<i32>>().await);

    // let stream = stream::iter(0..1000);
    // tokio::spawn(async move {});
    for i in 0..1000 {
        tx1.send(i).await.unwrap();
        info!("Sender 1: {}", i);
    }

    tokio::spawn(async move {});
    for i in 0..2 {
        let tx1 = tx1.clone();
        let r = r.clone();
        tokio::spawn(async move {
            info!("Receiver 1 starting");
            let mut rx = r.lock().await;
            info!("Receiver 1 have mutex");
            if let Some(rx1) = rx.recv().await {
                info!("Receiver 1: {}", rx1);
                drop(rx);
                // sleep for 1 second
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            info!("Receiver 1 done");
        });
    }
}
async fn select_ready_future<T>(futures: Vec<impl Future<Output = T>>) -> T {
    let mut futures = FuturesUnordered::from_iter(futures);
    futures
        .next()
        .await
        .expect("At least one future should complete")
}
// async fn distribute_messages<T, S>(
//     mut stream: S,
//     mut senders: Vec<mpsc::Sender<T>>,
// ) where
//     T: Clone + Send + 'static,
//     S: StreamExt<Item = T> + Unpin,
// {
//     stream
//         .for_each(|message| async {
//             let send_futures = senders
//                 .iter_mut()
//                 .map(|sender| sender.send(message.clone()));
//             let (_, index, _) = select_all(send_futures).await;
//             senders.swap_remove(index);
//         })
//         .await;
// }
