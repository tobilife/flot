//! A command's output streams, each read on a thread of its own, kept up to
//! its byte budget and told line by line as it is read.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use super::{Lines, Stream};

/// A stream being read on its own thread. What has arrived so far stays
/// available even if the stream never closes.
pub(super) struct Drain {
    buffer: Arc<Mutex<Vec<u8>>>,
    done: mpsc::Receiver<()>,
    /// Set once the command's output is collected: a line read after that
    /// is told to no one.
    collected: Arc<AtomicBool>,
}

impl Drain {
    /// Everything read by `deadline`, and whether the stream had closed.
    pub(super) fn collect(self, deadline: Instant) -> (Vec<u8>, bool) {
        let complete = !matches!(
            self.done
                .recv_timeout(deadline.saturating_duration_since(Instant::now())),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        self.collected.store(true, Ordering::SeqCst);
        let data = std::mem::take(&mut *self.buffer.lock().unwrap_or_else(|e| e.into_inner()));
        (data, complete)
    }
}

/// A stream cut into lines for [`Lines`] as it is read.
struct Splitter {
    stream: Stream,
    lines: Lines,
    /// The line read so far, which has not ended yet.
    partial: Vec<u8>,
    collected: Arc<AtomicBool>,
}

impl Splitter {
    fn feed(&mut self, mut bytes: &[u8]) {
        while let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
            self.partial.extend_from_slice(&bytes[..end]);
            self.tell();
            bytes = &bytes[end + 1..];
        }
        self.partial.extend_from_slice(bytes);
    }

    /// The stream ended: a last line without a line ending is a line too.
    fn finish(&mut self) {
        if !self.partial.is_empty() {
            self.tell();
        }
    }

    fn tell(&mut self) {
        let line = std::mem::take(&mut self.partial);
        if !self.collected.load(Ordering::SeqCst) {
            (self.lines)(self.stream, line.strip_suffix(b"\r").unwrap_or(&line));
        }
    }
}

/// Read `source` on its own thread, keeping at most `cap` bytes. Reaching the
/// cap raises `overflow`, which stops the command; reading continues, and
/// discards, so the command is never blocked on a full pipe meanwhile. The
/// bytes kept are also told to `lines`, line by line, when it is given.
pub(super) fn drain<R: Read + Send + 'static>(
    source: Option<R>,
    cap: usize,
    overflow: Arc<AtomicBool>,
    lines: Option<(Stream, Lines)>,
) -> Drain {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (sender, done) = mpsc::channel();
    let collected = Arc::new(AtomicBool::new(false));
    let shared = Arc::clone(&buffer);
    let mut splitter = lines.map(|(stream, lines)| Splitter {
        stream,
        lines,
        partial: Vec::new(),
        collected: Arc::clone(&collected),
    });
    std::thread::spawn(move || {
        if let Some(mut source) = source {
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                match source.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let taken = {
                            let mut kept = shared.lock().unwrap_or_else(|e| e.into_inner());
                            let room = cap.saturating_sub(kept.len());
                            kept.extend_from_slice(&chunk[..read.min(room)]);
                            if read > room {
                                overflow.store(true, Ordering::SeqCst);
                            }
                            read.min(room)
                        };
                        if let Some(splitter) = &mut splitter {
                            splitter.feed(&chunk[..taken]);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        if let Some(splitter) = &mut splitter {
            splitter.finish();
        }
        let _ = sender.send(());
    });
    Drain {
        buffer,
        done,
        collected,
    }
}
