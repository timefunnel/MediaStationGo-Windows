//! Headless integration with the installed libmpv and a loopback-only CDN.
//! Set MPV_STREAM_TEST_FIXTURE to an existing seekable local video, then run
//! cargo test -p jfn-playback --test stream_failure -- --ignored --nocapture.

use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use jfn_mpv::stream_cb::{self, StreamFailure, StreamProxyMode};
use jfn_mpv::{Command, EndFileReason, Event, Handle};
use jfn_playback::{EndReason, PlaybackEventKind, PlaybackPhase, PlaybackStateMachine};

static FAILURES: Mutex<Option<mpsc::Sender<StreamFailure>>> = Mutex::new(None);

fn forward_failure(failure: StreamFailure) {
    if let Some(tx) = FAILURES.lock().unwrap().as_ref() {
        tx.send(failure).unwrap();
    }
}

struct LoopbackCdn {
    url: String,
    reject: Arc<AtomicBool>,
    rejected: Arc<AtomicUsize>,
    stall: Arc<AtomicBool>,
    stalled: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<std::io::Result<()>>>,
}

impl LoopbackCdn {
    fn spawn(bytes: Vec<u8>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let url = format!("http://{}/fixture", listener.local_addr()?);
        let reject = Arc::new(AtomicBool::new(false));
        let rejected = Arc::new(AtomicUsize::new(0));
        let stall = Arc::new(AtomicBool::new(false));
        let stalled = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (reject_worker, rejected_worker, stop_worker) =
            (reject.clone(), rejected.clone(), stop.clone());
        let (stall_worker, stalled_worker) = (stall.clone(), stalled.clone());
        let bytes = Arc::new(bytes);
        let join = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut workers = Vec::new();
            while !stop_worker.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((socket, _)) => {
                        if workers.len() >= 64 {
                            return Err(std::io::Error::other("loopback request limit exceeded"));
                        }
                        let (bytes, reject, rejected) = (
                            bytes.clone(),
                            reject_worker.clone(),
                            rejected_worker.clone(),
                        );
                        let (stall, stalled) = (stall_worker.clone(), stalled_worker.clone());
                        workers.push(thread::spawn(move || {
                            serve_range(socket, &bytes, &reject, &rejected, &stall, &stalled)
                        }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => return Err(e),
                }
            }
            for worker in workers {
                worker
                    .join()
                    .map_err(|_| std::io::Error::other("loopback worker panicked"))??;
            }
            Ok(())
        });
        Ok(Self {
            url,
            reject,
            rejected,
            stall,
            stalled,
            stop,
            join: Some(join),
        })
    }

    fn finish(&mut self) -> std::io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| std::io::Error::other("loopback server panicked"))??;
        }
        Ok(())
    }
}

impl Drop for LoopbackCdn {
    fn drop(&mut self) {
        stream_cb::set_failure_handler(None);
        *FAILURES.lock().unwrap() = None;
        if let Err(e) = self.finish() {
            eprintln!("loopback cleanup failed: {e}");
        }
    }
}

fn serve_range(
    mut socket: TcpStream,
    bytes: &[u8],
    reject: &AtomicBool,
    rejected: &AtomicUsize,
    stall: &AtomicBool,
    stalled: &AtomicUsize,
) -> std::io::Result<()> {
    // The listener polls; each bounded response worker uses blocking I/O.
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut request = BufReader::new(socket.try_clone()?);
    let mut offset = None;
    let mut total = 0;
    loop {
        let mut line = String::new();
        let count = request.read_line(&mut line)?;
        total += count;
        if total > 8192 {
            return Err(std::io::Error::other("oversize loopback headers"));
        }
        if count == 0 || line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Range")
        {
            offset = value
                .trim()
                .strip_prefix("bytes=")
                .and_then(|s| s.strip_suffix('-'))
                .and_then(|s| s.parse::<usize>().ok());
        }
    }
    let offset = offset.ok_or_else(|| std::io::Error::other("missing Range"))?;
    if reject.load(Ordering::Acquire) {
        rejected.fetch_add(1, Ordering::AcqRel);
        let body = br#"{"message":"115 pmt 3-2","status":403}"#;
        write!(
            socket,
            "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        return socket.write_all(body);
    }
    if offset >= bytes.len() {
        return Err(std::io::Error::other("out-of-bounds fixture range"));
    }
    write!(
        socket,
        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {offset}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len() - 1,
        bytes.len(),
        bytes.len() - offset
    )?;
    if stall.load(Ordering::Acquire) {
        socket.write_all(&bytes[offset..offset + 1])?;
        socket.flush()?;
        stalled.fetch_add(1, Ordering::AcqRel);
        // Keep the response incomplete until the client cancels and closes it.
        // A timeout is a failed regression, never a successful peer close.
        let mut byte = [0_u8];
        return match socket.read(&mut byte) {
            Ok(0) => Ok(()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(())
            }
            Ok(_) => Err(std::io::Error::other(
                "unexpected bytes after stalled Range",
            )),
            Err(error) => Err(error),
        };
    }
    match socket.write_all(&bytes[offset..]) {
        Ok(()) => Ok(()),
        // Seeking deliberately abandons the previous response.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::ConnectionReset
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[test]
#[ignore = "requires installed libmpv and an existing seekable local video fixture"]
fn stalled_stream_can_stop_switch_files_and_destroy_mpv() -> Result<(), Box<dyn Error>> {
    let fixture = std::env::var("MPV_STREAM_TEST_FIXTURE")?;
    let bytes = std::fs::read(&fixture)?;
    assert!(bytes.len() < 32 * 1024 * 1024);
    for operation in ["stop", "switch", "destroy"] {
        let mut cdn = LoopbackCdn::spawn(bytes.clone())?;
        let handle = Handle::create()?;
        for (name, value) in [
            ("config", "no"),
            ("vo", "null"),
            ("ao", "null"),
            ("terminal", "no"),
            ("hwdec", "no"),
            ("audio", "no"),
            ("pause", "yes"),
            ("cache", "no"),
            ("stream-buffer-size", "4096"),
            ("demuxer-readahead-secs", "0"),
            ("demuxer-max-back-bytes", "0"),
        ] {
            handle.set_option_string(name, value)?;
        }
        handle.initialize()?;
        unsafe { stream_cb::register_protocol(handle.raw()) };
        let uri = stream_cb::build_uri(
            &cdn.url,
            "MediaStationGoWindows/test",
            None,
            StreamProxyMode::Direct,
        );
        handle.command(&Command::new(["loadfile", &uri])?)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut loaded = false;
        while Instant::now() < deadline {
            if matches!(handle.wait_event(0.01), Event::FileLoaded) {
                loaded = true;
                break;
            }
        }
        assert!(loaded, "fixture never loaded");
        let target = (handle.get_property::<f64>("duration")? * 0.8).to_string();
        cdn.stall.store(true, Ordering::Release);
        handle.command_async(0, &Command::new(["seek", &target, "absolute+exact"])?)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        while cdn.stalled.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            cdn.stalled.load(Ordering::Acquire),
            1,
            "seek never reached the stalled response"
        );
        thread::sleep(Duration::from_millis(120));
        let (tx, rx) = mpsc::channel();
        let next_file = fixture.clone();
        let worker = thread::spawn(move || -> Result<Duration, String> {
            let started = Instant::now();
            if operation != "destroy" {
                let args = if operation == "stop" {
                    vec!["stop"]
                } else {
                    vec!["loadfile", &next_file]
                };
                handle
                    .command_async(0, &Command::new(args).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                let deadline = Instant::now() + Duration::from_secs(2);
                let mut completed = false;
                while Instant::now() < deadline {
                    match handle.wait_event(0.01) {
                        Event::EndFile(_) if operation == "stop" => {
                            completed = true;
                            break;
                        }
                        Event::FileLoaded if operation == "switch" => {
                            completed = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !completed {
                    return Err(format!("{operation} did not complete"));
                }
            }
            drop(handle);
            let elapsed = started.elapsed();
            tx.send(elapsed).map_err(|e| e.to_string())?;
            Ok(elapsed)
        });
        let completed = rx.recv_timeout(Duration::from_secs(2));
        let server_result = cdn.finish();
        let worker_result = worker
            .join()
            .map_err(|_| "native cancellation worker panicked")?;
        server_result?;
        worker_result?;
        let elapsed = completed?;
        assert_eq!(
            cdn.stalled.load(Ordering::Acquire),
            1,
            "canceled stream reopened a Range"
        );
        eprintln!("native stalled response: {operation} including mpv destroy = {elapsed:?}");
    }
    Ok(())
}

#[test]
#[ignore = "requires installed libmpv and an existing seekable local video fixture"]
fn terminal_seek_failure_reaches_playback_state_before_native_eof() -> Result<(), Box<dyn Error>> {
    let fixture = std::env::var("MPV_STREAM_TEST_FIXTURE")?;
    let bytes = std::fs::read(fixture)?;
    assert!(bytes.len() < 32 * 1024 * 1024);
    let mut cdn = LoopbackCdn::spawn(bytes)?;
    let (tx, rx) = mpsc::channel();
    *FAILURES.lock().unwrap() = Some(tx);
    stream_cb::set_failure_handler(Some(forward_failure));
    let handle = Handle::create()?;
    for (name, value) in [
        ("config", "no"),
        ("vo", "null"),
        ("ao", "null"),
        ("terminal", "no"),
        ("hwdec", "no"),
        ("audio", "no"),
        ("pause", "yes"),
        ("cache", "no"),
        ("stream-buffer-size", "4096"),
        ("demuxer-readahead-secs", "0"),
        ("demuxer-max-back-bytes", "0"),
    ] {
        handle.set_option_string(name, value)?;
    }
    handle.initialize()?;
    unsafe { stream_cb::register_protocol(handle.raw()) };
    let uri = stream_cb::build_uri(
        &cdn.url,
        "MediaStationGoWindows/test",
        None,
        StreamProxyMode::Direct,
    );
    let session_id = stream_cb::session_id(&uri).ok_or("missing session ID")?;
    let mut state = PlaybackStateMachine::new();
    state.on_stream_load_starting("local-fixture".to_string(), session_id);
    handle.command(&Command::new(["loadfile", &uri])?)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut seek_started = None;
    let mut failure_seen = false;
    let mut ended = false;
    while Instant::now() < deadline {
        while let Ok(failure) = rx.try_recv() {
            assert_eq!(failure.session_id, session_id);
            assert_eq!(failure.http_status(), Some(403));
            let events = state.on_stream_failed(failure);
            assert!(events.iter().any(|e| e.kind == PlaybackEventKind::Error));
            failure_seen = true;
        }
        match handle.wait_event(0.01) {
            Event::FileLoaded if seek_started.is_none() => {
                state.on_file_loaded();
                state.on_pause_changed(false);
                let duration = handle.get_property::<f64>("duration")?;
                assert!(duration.is_finite() && duration > 0.0);
                let target = (duration * 0.8).to_string();
                cdn.reject.store(true, Ordering::Release);
                seek_started = Some(Instant::now());
                handle.command(&Command::new(["seek", &target, "absolute+exact"])?)?;
            }
            Event::PlaybackRestart => {
                state.on_playback_restart();
            }
            Event::EndFile(reason) => {
                // The callback queues its typed error before returning to mpv;
                // consume it before the native terminal event, as the coordinator does.
                while let Ok(failure) = rx.try_recv() {
                    assert_eq!(failure.http_status(), Some(403));
                    state.on_stream_failed(failure);
                    failure_seen = true;
                }
                assert!(
                    failure_seen,
                    "native file ended before the injected seek failure: {reason:?}"
                );
                let reason = match reason {
                    EndFileReason::Eof => EndReason::Eof,
                    EndFileReason::Error(_) => EndReason::Error,
                    _ => EndReason::Canceled,
                };
                assert!(state.on_end_file(reason, String::new()).is_empty());
                ended = true;
                break;
            }
            _ => {}
        }
    }
    handle.command(&Command::new(["stop"])?)?;
    drop(handle);
    cdn.finish()?;
    assert!(ended, "libmpv did not end after terminal seek failure");
    assert!(failure_seen);
    assert_eq!(state.snapshot().phase, PlaybackPhase::Stopped);
    assert_eq!(cdn.rejected.load(Ordering::Acquire), 3);
    let elapsed = seek_started.ok_or("seek was never issued")?.elapsed();
    eprintln!(
        "injected seek: typed HTTP 403, native EOF suppressed, 3 rejected Ranges, elapsed={elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(5));
    Ok(())
}
