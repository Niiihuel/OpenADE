use super::*;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::ReadBuf;

fn probe() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("inspection_probe.rs");
    let binary = directory.path().join(if cfg!(windows) {
        "inspection_probe.exe"
    } else {
        "inspection_probe"
    });
    std::fs::write(
        &source,
        r#"
use std::{io::{Read, Write}, thread, time::Duration};
fn main() {
    match std::env::args().nth(1).as_deref().unwrap() {
        "binary" => {
            std::io::stdout().write_all(&[255, 0, 27, 65]).unwrap();
            std::io::stderr().write_all("ERR:ñ\n".as_bytes()).unwrap();
        }
        "flood" => std::io::stdout().write_all(&vec![b'x'; 32 * 1024]).unwrap(),
        "stream" => for index in 0..40u8 {
            std::io::stdout().write_all(&vec![index; 64]).unwrap();
            std::io::stdout().flush().unwrap();
            thread::sleep(Duration::from_millis(5));
        },
        "wait" => {
            print!("WAIT\n");
            std::io::stdout().flush().unwrap();
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
        }
        _ => panic!("unknown mode"),
    }
}
"#,
    )
    .unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let compiled = std::process::Command::new(compiler)
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    (directory, binary)
}

async fn terminal_copy(
    manager: &PersistentCommandManager,
    id: &str,
) -> PersistentCommandInspection {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let inspection = manager.inspect_pending_output(id).unwrap();
        if inspection.exit.is_some() {
            return inspection;
        }
        assert!(Instant::now() < deadline, "probe did not exit");
        sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn pending_output_inspection_preserves_binary_streams_terminal_delivery_and_capacity() {
    let (_directory, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = PersistentCommandManager::new(
        workspace.path().to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits {
            max_sessions: 1,
            ..PersistentCommandLimits::default()
        },
    )
    .unwrap();
    let started = manager
        .start(
            binary.to_str().unwrap(),
            &["binary".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let copy = terminal_copy(&manager, &started.id).await;
    assert_eq!(copy.retained_bytes, 4 + "ERR:ñ\n".len());
    assert_eq!(manager.inspect_pending_output(&started.id).unwrap(), copy);
    assert_eq!(manager.list_sessions().unwrap().len(), 1);
    assert!(
        manager
            .start(
                binary.to_str().unwrap(),
                &["binary".into()],
                &CancellationToken::new()
            )
            .is_err(),
        "inspection freed a session slot"
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for event in &copy.events {
        match event {
            PersistentCommandEvent::Output {
                stream: PersistentCommandStream::Stdout,
                bytes,
            } => stdout.extend_from_slice(bytes),
            PersistentCommandEvent::Output {
                stream: PersistentCommandStream::Stderr,
                bytes,
            } => stderr.extend_from_slice(bytes),
            event => panic!("unexpected event {event:?}"),
        }
    }
    assert_eq!(stdout, [255, 0, 27, 65]);
    assert_eq!(stderr, "ERR:ñ\n".as_bytes());
    let polled = manager
        .poll(&started.id, Duration::ZERO, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(polled.events, copy.events);
    assert_eq!(polled.exit, copy.exit);
    assert_eq!(polled.reader_error, copy.reader_error);
    assert!(polled.finished);
    assert!(manager.inspect_pending_output(&started.id).is_err());
    assert!(manager.list_sessions().unwrap().is_empty());
    let fresh = manager
        .start(
            binary.to_str().unwrap(),
            &["binary".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    terminal_copy(&manager, &fresh.id).await;
    manager
        .poll(&fresh.id, Duration::ZERO, &CancellationToken::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn pending_output_inspection_preserves_omissions_and_partial_poll_tail() {
    let (_directory, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = PersistentCommandManager::new(
        workspace.path().to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits {
            max_buffered_output_bytes: 4096,
            max_poll_output_bytes: 1024,
            ..PersistentCommandLimits::default()
        },
    )
    .unwrap();
    let started = manager
        .start(
            binary.to_str().unwrap(),
            &["flood".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let original = terminal_copy(&manager, &started.id).await;
    assert_eq!(original.retained_bytes, 4096);
    assert_eq!(original.max_buffered_output_bytes, 4096);
    assert!(original
        .events
        .contains(&PersistentCommandEvent::Omitted { bytes: 28 * 1024 }));
    assert_eq!(
        manager.inspect_pending_output(&started.id).unwrap(),
        original
    );
    let first = manager
        .poll(&started.id, Duration::ZERO, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!first.finished);
    let tail = manager.inspect_pending_output(&started.id).unwrap();
    assert_eq!(tail.retained_bytes, 3072);
    assert!(tail
        .events
        .contains(&PersistentCommandEvent::Omitted { bytes: 28 * 1024 }));
    assert_eq!(manager.inspect_pending_output(&started.id).unwrap(), tail);
    let mut retained = first
        .events
        .iter()
        .filter_map(|event| match event {
            PersistentCommandEvent::Output { bytes, .. } => Some(bytes.len()),
            _ => None,
        })
        .sum::<usize>();
    let mut omitted = 0;
    loop {
        let result = manager
            .poll(&started.id, Duration::ZERO, &CancellationToken::new())
            .await
            .unwrap();
        for event in &result.events {
            match event {
                PersistentCommandEvent::Output { bytes, .. } => retained += bytes.len(),
                PersistentCommandEvent::Omitted { bytes } => omitted += bytes,
            }
        }
        if result.finished {
            break;
        }
        let _ = manager.inspect_pending_output(&started.id).unwrap();
    }
    assert_eq!(retained, 4096);
    assert_eq!(omitted, 28 * 1024);
}

#[tokio::test]
async fn pending_output_inspection_can_race_agent_polls_without_stealing_stream_bytes() {
    let (_directory, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = Arc::new(
        PersistentCommandManager::new(
            workspace.path().to_path_buf(),
            CommandIsolation::HostUnrestricted,
            PersistentCommandLimits::default(),
        )
        .unwrap(),
    );
    let started = manager
        .start(
            binary.to_str().unwrap(),
            &["stream".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let inspector = manager.clone();
    let id = started.id.clone();
    let stop = CancellationToken::new();
    let observer_stop = stop.clone();
    let observer = std::thread::spawn(move || {
        let mut copies = 0;
        loop {
            if observer_stop.is_cancelled() {
                return copies;
            }
            match inspector.inspect_pending_output(&id) {
                Ok(copy) => {
                    assert!(copy.retained_bytes <= copy.max_buffered_output_bytes);
                    copies += 1;
                }
                Err(_) => return copies,
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = manager
            .poll(
                &started.id,
                Duration::from_millis(100),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        for event in &result.events {
            match event {
                PersistentCommandEvent::Output {
                    stream: PersistentCommandStream::Stdout,
                    bytes,
                } => output.extend_from_slice(bytes),
                event => panic!("unexpected {event:?}"),
            }
        }
        if result.finished {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    stop.cancel();
    assert!(observer.join().unwrap() > 0);
    assert_eq!(
        output,
        (0..40u8)
            .flat_map(|value| vec![value; 64])
            .collect::<Vec<_>>()
    );
    assert!(manager.inspect_pending_output(&started.id).is_err());
}

struct FailedReader;
impl AsyncRead for FailedReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("injected pipe read failure")))
    }
}

#[tokio::test]
async fn pending_output_inspection_exposes_reader_failure_without_resolving_it() {
    let (_directory, binary) = probe();
    let workspace = tempfile::tempdir().unwrap();
    let manager = PersistentCommandManager::new(
        workspace.path().to_path_buf(),
        CommandIsolation::HostUnrestricted,
        PersistentCommandLimits::default(),
    )
    .unwrap();
    let started = manager
        .start(
            binary.to_str().unwrap(),
            &["binary".into()],
            &CancellationToken::new(),
        )
        .unwrap();
    let exited = terminal_copy(&manager, &started.id).await;
    let session = manager.session(&started.id).unwrap();
    // A fault-injected reader exercises the same read_pipe diagnostic contract;
    // portable tests cannot deterministically force an OS pipe read failure.
    let error = read_pipe(
        FailedReader,
        PersistentCommandStream::Stdout,
        session.output.clone(),
    )
    .await
    .unwrap_err();
    session
        .output
        .finish(exited.exit.clone().unwrap(), Some(error.to_string()));
    let inspected = manager.inspect_pending_output(&started.id).unwrap();
    assert_eq!(inspected.events, exited.events);
    assert_eq!(
        inspected.reader_error.as_deref(),
        Some("injected pipe read failure")
    );
    assert_eq!(
        manager.inspect_pending_output(&started.id).unwrap(),
        inspected
    );
    let polled = manager
        .poll(&started.id, Duration::ZERO, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(polled.reader_error, inspected.reader_error);
    assert_eq!(polled.events, inspected.events);
    assert!(polled.finished);
}
