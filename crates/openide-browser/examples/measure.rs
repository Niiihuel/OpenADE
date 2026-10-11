//! Reproduces the measurements in `docs/browser-driver.md` against any page
//! served over HTTP. Chrome is the one named by `OPENIDE_BROWSER_EXECUTABLE`.
//!
//! ```sh
//! cargo run --profile desktop -p openide-browser --example measure -- hold <url>
//! cargo run --profile desktop -p openide-browser --example measure -- latency <url> [rounds]
//! cargo run --profile desktop -p openide-browser --example measure -- stream <url> [scale] [jpeg quality]
//! ```
//!
//! `hold` keeps one session open and prints the browser's process id, so
//! memory can be read from `/proc/<pid>/smaps_rollup`; a line on standard
//! input ends it. `latency` times the agent's operations. `stream` reports
//! frames per second and the time from a wheel event to the frame showing it.

use openide_browser::{Capture, Driver, FrameFormat, LaunchOptions, SnapshotOptions, Target};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn launch(width: u32, height: u32, scale: f64) -> Driver {
    let executable = std::env::var_os("OPENIDE_BROWSER_EXECUTABLE").expect("set OPENIDE_BROWSER_EXECUTABLE");
    Driver::launch(&LaunchOptions {
        executable: executable.into(),
        width,
        height,
        scale,
        temp_root: None,
    })
    .expect("Chrome starts")
}

/// Median and 95th percentile of the samples, in milliseconds.
fn summary(mut samples: Vec<Duration>) -> (f64, f64) {
    samples.sort();
    let at = |fraction: f64| samples[((samples.len() - 1) as f64 * fraction).round() as usize].as_secs_f64() * 1000.0;
    (at(0.5), at(0.95))
}

fn timed(rounds: usize, mut work: impl FnMut()) -> (f64, f64) {
    summary((0..rounds).map(|_| {
        let started = Instant::now();
        work();
        started.elapsed()
    }).collect())
}

fn hold(url: &str) {
    let mut driver = launch(900, 700, 1.0);
    driver.navigate(url).expect("the page loads");
    driver.snapshot(&SnapshotOptions::default()).expect("a snapshot");
    println!("driver {} browser {}", std::process::id(), driver.process_id().unwrap_or(0));
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn latency(url: &str, rounds: usize) {
    let started = Instant::now();
    let mut driver = launch(900, 700, 1.0);
    println!("launch {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
    driver.navigate(url).expect("the page loads");
    // The form fixture, or any page with one such field and one such button.
    let field = Target::Selector("input[name=name]");
    let button = Target::Selector("button[aria-pressed]");
    let report = |name: &str, (median, high): (f64, f64)| println!("{name:<34} median {median:7.2} ms   p95 {high:7.2} ms");
    let whole = SnapshotOptions::default();
    let size = driver.snapshot(&whole).expect("a snapshot").text.len();
    println!("snapshot of {url}: {size} bytes");
    report("snapshot", timed(rounds, || { driver.snapshot(&whole).unwrap(); }));
    report("snapshot, interactive only", timed(rounds, || {
        driver.snapshot(&SnapshotOptions { interactive_only: true, ..SnapshotOptions::default() }).unwrap();
    }));
    report("info", timed(rounds, || { driver.info().unwrap(); }));
    report("screenshot (viewport PNG)", timed(rounds, || { driver.screenshot(Capture::Viewport).unwrap(); }));
    report("evaluate", timed(rounds, || { driver.evaluate("document.title", None).unwrap(); }));
    report("click", timed(rounds, || { driver.click(&button).unwrap(); }));
    report("fill", timed(rounds, || { driver.fill(&field, "ada@example.test").unwrap(); }));
    // What the Node bridge returns for each agent action: page state, viewport PNG and tree.
    let state = |driver: &mut Driver, tree: bool| {
        driver.info().unwrap();
        driver.screenshot(Capture::Viewport).unwrap();
        if tree {
            driver.snapshot(&whole).unwrap();
        }
    };
    report("snapshot + info + PNG", timed(rounds, || state(&mut driver, true)));
    report("click + info + PNG + snapshot", timed(rounds, || { driver.click(&button).unwrap(); state(&mut driver, true); }));
    report("fill + info + PNG + snapshot", timed(rounds, || { driver.fill(&field, "ada@example.test").unwrap(); state(&mut driver, true); }));
    report("evaluate + info + PNG", timed(rounds, || { driver.evaluate("document.title", None).unwrap(); state(&mut driver, false); }));
    report("screenshot + info", timed(rounds, || { driver.screenshot(Capture::Viewport).unwrap(); driver.info().unwrap(); }));
}

fn stream(url: &str, scale: f64, quality: u8) {
    let (width, height) = (1280, 800);
    let mut driver = launch(width, height, scale);
    driver.navigate(url).expect("the page loads");
    // Make any page tall and animated enough to measure.
    driver.evaluate("() => { document.body.style.minHeight = '90000px'; const box = document.createElement('div'); box.style.cssText = 'position:fixed;top:10px;left:10px;width:120px;height:120px;background:#c30'; document.body.append(box); let t = 0; (function step() { box.style.left = (10 + (t += 7) % 900) + 'px'; requestAnimationFrame(step); })(); window.stopBox = () => { box.remove(); }; }", None).unwrap();
    let frames = driver.start_stream(FrameFormat::Jpeg { quality }, None).expect("the stream starts");
    let first = frames.next(Duration::from_secs(5)).expect("a first frame");
    println!("frames are {}x{} JPEG quality {quality} for a {width}x{height} viewport at scale {scale}", first.width, first.height);

    // The view: another thread that takes every frame the moment it is ready.
    let arrivals: Arc<Mutex<Vec<(Instant, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    let (seen, view) = (arrivals.clone(), frames.clone());
    std::thread::spawn(move || {
        while view.is_active() {
            if let Some(frame) = view.next(Duration::from_millis(200)) {
                seen.lock().unwrap().push((Instant::now(), frame.data.len()));
            }
        }
    });
    let window = Duration::from_secs(5);
    let taken = |since: Instant| -> Vec<(Instant, usize)> {
        arrivals.lock().unwrap().iter().copied().filter(|(arrived, _)| *arrived >= since).collect()
    };
    // The frame that follows each moment in `sent`, if one came.
    let following = |sent: &[Instant], frames: &[(Instant, usize)]| -> Vec<Duration> {
        sent.iter()
            .filter_map(|sent| frames.iter().find(|(arrived, _)| arrived >= sent).map(|(arrived, _)| *arrived - *sent))
            .collect()
    };

    let started = Instant::now();
    std::thread::sleep(window);
    let animated = taken(started);
    let bytes: usize = animated.iter().map(|(_, bytes)| bytes).sum();
    println!(
        "animated page: {:.1} frames/s, {} KiB per frame",
        animated.len() as f64 / window.as_secs_f64(),
        bytes / animated.len().max(1) / 1024
    );

    // Continuous scrolling of a page that is otherwise still: a wheel event every 16 ms.
    driver.evaluate("stopBox()", None).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let started = Instant::now();
    let mut sent = Vec::new();
    while started.elapsed() < window {
        let tick = Instant::now();
        sent.push(tick);
        driver.wheel(640.0, 400.0, 0.0, 40.0).unwrap();
        std::thread::sleep(Duration::from_millis(16).saturating_sub(tick.elapsed()));
    }
    std::thread::sleep(Duration::from_millis(300));
    let scrolled = taken(started);
    let during: Vec<_> = scrolled.iter().filter(|(arrived, _)| *arrived <= started + window).collect();
    let (median, high) = summary(following(&sent, &scrolled));
    println!(
        "scrolling: {:.1} frames/s; next frame after a wheel event: median {median:.1} ms, p95 {high:.1} ms",
        during.len() as f64 / window.as_secs_f64()
    );

    // One wheel event at a time on a page at rest: how long until the first
    // frame it causes arrives.
    let mut delays = Vec::new();
    let mut accepted = Vec::new();
    for _ in 0..120 {
        // Let the scroll of the event before settle.
        loop {
            let quiet = Instant::now();
            std::thread::sleep(Duration::from_millis(150));
            if taken(quiet).is_empty() {
                break;
            }
        }
        let tick = Instant::now();
        driver.wheel(640.0, 400.0, 0.0, 120.0).unwrap();
        accepted.push(tick.elapsed());
        let deadline = tick + Duration::from_secs(2);
        while taken(tick).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_micros(500));
        }
        delays.extend(following(&[tick], &taken(tick)));
    }
    let answered = delays.len();
    let (median, high) = summary(delays);
    println!("page at rest, wheel to first frame: median {median:.1} ms, p95 {high:.1} ms over {answered} of 120 events");
    let (median, high) = summary(accepted);
    println!("  of which until Chrome accepted the event: median {median:.1} ms, p95 {high:.1} ms");
    driver.stop_stream().unwrap();
    let started = Instant::now();
    let still = driver.capture_frame().expect("a lossless frame");
    println!("lossless frame {}x{}: {} KiB in {:.1} ms", still.width, still.height, still.data.len() / 1024, started.elapsed().as_secs_f64() * 1000.0);
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let number = |at: usize, default: f64| arguments.get(at).and_then(|text| text.parse().ok()).unwrap_or(default);
    match (arguments.first().map(String::as_str), arguments.get(1)) {
        (Some("hold"), Some(url)) => hold(url),
        (Some("latency"), Some(url)) => latency(url, number(2, 50.0) as usize),
        (Some("stream"), Some(url)) => stream(url, number(2, 2.0), number(3, 80.0) as u8),
        _ => eprintln!("usage: measure hold <url> | latency <url> [rounds] | stream <url> [scale] [jpeg quality]"),
    }
}
