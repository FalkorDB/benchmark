use std::ops::Add;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::info;

#[derive(Debug)]
pub struct Msg<Payload: Send + Sync> {
    pub start_time: Instant,
    // offset in milliseconds from start_time
    pub offset: u64,
    pub payload: Payload,
}
impl<Payload: Send + Sync> Msg<Payload> {
    /// Compute the offset in milliseconds from the current time to the target time
    #[inline]
    pub fn compute_offset_ms(&self) -> i64 {
        let current_time = Instant::now();
        let target_time = self.start_time + Duration::from_millis(self.offset);

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

    /// The instant this message was scheduled to start executing.
    ///
    /// Latency must be measured from this anchor rather than from dequeue time,
    /// otherwise a backed-up schedule silently drops its queueing delay from the
    /// recorded latency (coordinated omission).
    #[inline]
    pub fn intended_start(&self) -> Instant {
        self.start_time + Duration::from_millis(self.offset)
    }
}
/// schedule at a rate of msg_per_sec messages per second to sender for number_of_messages
/// returns a handle to the spawned task
/// The actual send should be done as fast as possible,
/// but each message contain an offset from the start time which should
/// server as a deadline for the message to be processed or delay depending on the system at test speed
pub fn spawn_scheduler<Payload: Send + Sync + 'static>(
    msg_per_sec: usize,
    sender: Sender<Msg<Payload>>,
    requests: Vec<Payload>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let interval_in_nanos = (1_000_000_000.0 / msg_per_sec as f64) as u64;
        // anchor the start time to 200 ms from now
        let start_time = Instant::now().add(Duration::from_millis(200));
        for (count, payload) in requests.into_iter().enumerate() {
            // compute offset in millis from an interval in nonos
            let offset = (count as u64 * interval_in_nanos) / 1_000_000;
            match sender
                .send(Msg {
                    start_time,
                    offset,
                    payload,
                })
                .await
            {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intended_start_anchors_at_scheduled_time() {
        let start_time = Instant::now();
        let msg = Msg {
            start_time,
            offset: 250,
            payload: (),
        };
        // The anchor is the predetermined scheduled instant (start_time + offset ms),
        // independent of when the message is actually dequeued — this is what makes
        // latency measured from it coordinated-omission-correct.
        assert_eq!(
            msg.intended_start(),
            start_time + Duration::from_millis(250)
        );
    }

    #[test]
    fn intended_start_with_zero_offset_is_start_time() {
        let start_time = Instant::now();
        let msg = Msg {
            start_time,
            offset: 0,
            payload: (),
        };
        assert_eq!(msg.intended_start(), start_time);
    }
}
