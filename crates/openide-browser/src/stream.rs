//! The frame stream of the live view.
//!
//! Chrome sends a screencast frame and waits for its acknowledgement before
//! sending more. The acknowledgement is held back until the consumer has
//! taken the frame, so a view that paints slowly makes Chrome skip frames
//! instead of queueing them here: at most one frame is ever kept.

use crate::cdp::Client;
use serde_json::json;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Largest encoded frame handed to the view.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFormat {
    /// Lossy and small: what a moving page is streamed as.
    Jpeg { quality: u8 },
    /// Lossless.
    Png,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// The encoded image, exactly as Chrome produced it.
    pub data: Vec<u8>,
    /// Device pixels, read from the image header: viewport × scale.
    pub width: u32,
    pub height: u32,
    /// Chrome's capture time, seconds since the epoch. Zero when unknown.
    pub timestamp: f64,
    /// Counts the frames Chrome sent since the stream started, skipped or not.
    pub sequence: u64,
}

/// Width and height of a PNG or JPEG, from its header.
pub fn image_size(data: &[u8]) -> Option<(u32, u32)> {
    let word = |at: usize| Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?));
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((word(16)?, word(20)?));
    }
    if !data.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let short = |at: usize| Some(usize::from(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?)));
    let mut at = 2;
    loop {
        let marker = *data.get(at + 1)?;
        if *data.get(at)? != 0xff {
            return None;
        }
        // Every start-of-frame marker except the three that define tables.
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            return Some((short(at + 7)? as u32, short(at + 5)? as u32));
        }
        at += 2 + short(at + 2)?;
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Slot {
    active: bool,
    frame: Option<Frame>,
    /// Frames Chrome still waits to hear about: the one in the slot and any it replaced.
    unacknowledged: Vec<(String, i64)>,
    sequence: u64,
    replaced: u64,
    waker: Option<Waker>,
}

#[derive(Default)]
pub(crate) struct StreamSlot {
    slot: Mutex<Slot>,
    ready: Condvar,
}

impl StreamSlot {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    pub(crate) fn start(&self, waker: Option<Waker>) {
        let mut slot = self.lock();
        *slot = Slot {
            active: true,
            waker,
            ..Slot::default()
        };
    }

    /// Stops delivering. Whatever Chrome still waits on is acknowledged so a
    /// later stream does not start stalled.
    pub(crate) fn stop(&self, client: &Client) {
        let pending = {
            let mut slot = self.lock();
            slot.active = false;
            slot.frame = None;
            slot.waker = None;
            std::mem::take(&mut slot.unacknowledged)
        };
        acknowledge(client, pending);
        self.ready.notify_all();
    }

    pub(crate) fn close(&self) {
        let mut slot = self.lock();
        slot.active = false;
        slot.waker = None;
        drop(slot);
        self.ready.notify_all();
    }

    /// A frame arrived from Chrome. Runs on the reader thread and never waits.
    pub(crate) fn offer(&self, client: &Client, session: &str, data: Vec<u8>, ack: i64, timestamp: f64) {
        let size = image_size(&data).filter(|_| data.len() <= MAX_FRAME_BYTES);
        let mut slot = self.lock();
        let (true, Some((width, height))) = (slot.active, size) else {
            drop(slot);
            return acknowledge(client, vec![(session.to_owned(), ack)]);
        };
        slot.sequence += 1;
        if slot.frame.is_some() {
            // The consumer has not taken the previous one: the newer frame
            // replaces it, and both stay unacknowledged until it does.
            slot.replaced += 1;
        }
        slot.frame = Some(Frame {
            data,
            width,
            height,
            timestamp,
            sequence: slot.sequence,
        });
        slot.unacknowledged.push((session.to_owned(), ack));
        let waker = slot.waker.clone();
        drop(slot);
        self.ready.notify_all();
        if let Some(waker) = waker {
            waker();
        }
    }

    fn take(&self, client: &Client, deadline: Option<Instant>) -> Option<Frame> {
        let mut slot = self.lock();
        while slot.frame.is_none() {
            let left = deadline?.saturating_duration_since(Instant::now());
            if left.is_zero() || !slot.active {
                return None;
            }
            slot = self
                .ready
                .wait_timeout(slot, left)
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
        let frame = slot.frame.take();
        let pending = std::mem::take(&mut slot.unacknowledged);
        drop(slot);
        // Only now may Chrome produce the next frame.
        acknowledge(client, pending);
        frame
    }
}

fn acknowledge(client: &Client, pending: Vec<(String, i64)>) {
    for (session, ack) in pending {
        let _ = client.post(&session, "Page.screencastFrameAck", json!({"sessionId": ack}));
    }
}

/// The consumer's end of the stream. It may live on another thread than the
/// driver: taking a frame only touches the slot and posts acknowledgements.
#[derive(Clone)]
pub struct FrameStream {
    pub(crate) shared: Arc<crate::observer::Shared>,
    pub(crate) client: Client,
}

impl FrameStream {
    /// The newest frame, if one arrived since the last call.
    pub fn try_next(&self) -> Option<Frame> {
        self.shared.stream.take(&self.client, None)
    }

    /// Waits up to `timeout` for a frame. `None` when none came or the stream stopped.
    pub fn next(&self, timeout: Duration) -> Option<Frame> {
        self.shared.stream.take(&self.client, Some(Instant::now() + timeout))
    }

    /// Frames that arrived and were replaced before anyone took them.
    pub fn replaced(&self) -> u64 {
        self.shared.stream.lock().replaced
    }

    pub fn is_active(&self) -> bool {
        self.shared.stream.lock().active
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::client;
    use serde_json::Value;
    use std::sync::mpsc::Receiver;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut data = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data
    }

    fn acknowledged(written: &Receiver<Value>) -> Vec<i64> {
        written
            .try_iter()
            .map(|message| {
                assert_eq!(message["method"], "Page.screencastFrameAck");
                assert_eq!(message["sessionId"], "S");
                message["params"]["sessionId"].as_i64().unwrap()
            })
            .collect()
    }

    #[test]
    fn sizes_are_read_from_png_and_jpeg_headers() {
        assert_eq!(image_size(&png(2560, 1600)), Some((2560, 1600)));
        // SOI, an APP0 segment to skip, then a baseline start of frame.
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x4a, 0x46];
        jpeg.extend_from_slice(&[0xff, 0xdb, 0x00, 0x03, 0x00]);
        jpeg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08, 0x06, 0x40, 0x0a, 0x00, 0x03]);
        assert_eq!(image_size(&jpeg), Some((2560, 1600)));
        // A progressive one, and a table segment that is not a frame header.
        let progressive = [0xff, 0xd8, 0xff, 0xc4, 0x00, 0x02, 0xff, 0xc2, 0x00, 0x11, 0x08, 0x01, 0xe0, 0x02, 0x80];
        assert_eq!(image_size(&progressive), Some((640, 480)));
        for broken in [&b""[..], b"GIF89a", &[0xff, 0xd8], &[0xff, 0xd8, 0xff, 0xc0, 0x00], &jpeg[..12], &png(1, 1)[..20]] {
            assert_eq!(image_size(broken), None);
        }
    }

    #[test]
    fn a_frame_is_acknowledged_only_when_the_consumer_takes_it() {
        let (client, written, _source) = client();
        let slot = StreamSlot::default();
        slot.start(None);
        slot.offer(&client, "S", png(640, 480), 1, 10.0);
        assert!(acknowledged(&written).is_empty(), "nothing is acknowledged while the frame waits");
        let frame = slot.take(&client, None).unwrap();
        assert_eq!((frame.width, frame.height, frame.sequence, frame.timestamp), (640, 480, 1, 10.0));
        assert_eq!(acknowledged(&written), [1]);
        assert!(slot.take(&client, None).is_none());
        assert!(acknowledged(&written).is_empty());
    }

    #[test]
    fn a_slow_consumer_gets_the_newest_frame_and_never_a_queue() {
        let (client, written, _source) = client();
        let slot = StreamSlot::default();
        slot.start(None);
        for ack in 1..=3 {
            slot.offer(&client, "S", png(100 + ack as u32, 50), ack, 0.0);
        }
        assert!(acknowledged(&written).is_empty(), "Chrome is left waiting, so it skips");
        let frame = slot.take(&client, None).unwrap();
        assert_eq!((frame.width, frame.sequence), (103, 3), "only the newest is kept");
        assert_eq!(acknowledged(&written), [1, 2, 3], "taking it releases every frame Chrome waits on");
        assert_eq!(slot.lock().replaced, 2);
        assert!(slot.lock().unacknowledged.is_empty());
    }

    #[test]
    fn frames_outside_a_stream_are_acknowledged_and_dropped() {
        let (client, written, _source) = client();
        let slot = StreamSlot::default();
        // Not started.
        slot.offer(&client, "S", png(10, 10), 1, 0.0);
        assert_eq!(acknowledged(&written), [1]);
        slot.start(None);
        // Not an image, and one above the size limit.
        slot.offer(&client, "S", b"junk".to_vec(), 2, 0.0);
        let mut huge = png(10, 10);
        huge.resize(MAX_FRAME_BYTES + 1, 0);
        slot.offer(&client, "S", huge, 3, 0.0);
        assert_eq!(acknowledged(&written), [2, 3]);
        assert!(slot.take(&client, None).is_none());
        // Stopping releases the frame that was waiting and refuses later ones.
        slot.offer(&client, "S", png(10, 10), 4, 0.0);
        slot.stop(&client);
        assert_eq!(acknowledged(&written), [4]);
        assert!(slot.take(&client, None).is_none());
        slot.offer(&client, "S", png(10, 10), 5, 0.0);
        assert_eq!(acknowledged(&written), [5]);
        // A new stream starts clean.
        slot.start(None);
        let fresh = slot.lock();
        assert_eq!((fresh.sequence, fresh.replaced, fresh.active), (0, 0, true));
    }

    #[test]
    fn a_waiting_consumer_is_woken_by_a_frame_and_by_the_stop() {
        let (client, _written, _source) = client();
        let slot = Arc::new(StreamSlot::default());
        let woken = Arc::new(Mutex::new(0));
        let counter = woken.clone();
        slot.start(Some(Arc::new(move || *counter.lock().unwrap() += 1)));
        let waiter = {
            let (slot, client) = (slot.clone(), client.clone());
            std::thread::spawn(move || slot.take(&client, Some(Instant::now() + Duration::from_secs(5))))
        };
        std::thread::sleep(Duration::from_millis(20));
        slot.offer(&client, "S", png(8, 8), 1, 0.0);
        assert_eq!(waiter.join().unwrap().unwrap().width, 8);
        assert_eq!(*woken.lock().unwrap(), 1, "the view is told a frame is ready");

        let started = Instant::now();
        assert!(slot.take(&client, Some(Instant::now() + Duration::from_millis(30))).is_none());
        assert!(started.elapsed() >= Duration::from_millis(30), "an empty stream waits out its timeout");
        let waiter = {
            let (slot, client) = (slot.clone(), client.clone());
            std::thread::spawn(move || slot.take(&client, Some(Instant::now() + Duration::from_secs(30))))
        };
        std::thread::sleep(Duration::from_millis(20));
        slot.stop(&client);
        assert!(waiter.join().unwrap().is_none(), "a stopped stream releases its consumer at once");
    }
}
