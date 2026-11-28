//! The live view, for a terminal: what steps print scrolls by above one row
//! per step running now.

use std::io::Write;
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use crate::core::receipt::StepStatus;
use crate::service::progress::{Event, Source};

use super::super::terminal_safe;
use super::words::{ended, held, outcome, seconds, tag, which};
use super::{console, Message};

/// How often the live view redraws the running steps' times.
const TICK: Duration = Duration::from_millis(100);

/// Events taken at most between two redraws of the live view, so a chatty
/// step costs one redraw per batch of lines, and the running steps' times
/// still move.
const BATCH: usize = 256;

/// Columns assumed when the terminal does not say.
const COLUMNS: usize = 80;

/// Moves to the start of the row above and clears it: the live view's rows
/// are taken back with this alone.
pub(super) const ERASE_ROW: &str = "\x1b[A\r\x1b[K";

/// Output scrolling above a block of the steps running now, redrawn every
/// [`TICK`] and after each batch of events.
pub(super) fn show_live<W: Write>(receiver: &mpsc::Receiver<Message>, mut live: Live<W>) {
    loop {
        let mut taken = 0;
        let mut next = match receiver.recv_timeout(TICK) {
            Ok(message) => Some(message),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Message::Done),
        };
        while let Some(message) = next.take() {
            let Message::Event(event) = message else {
                let _ = live.clear();
                return;
            };
            live.event(&event, Instant::now());
            taken += 1;
            if taken < BATCH {
                next = match receiver.try_recv() {
                    Ok(message) => Some(message),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => Some(Message::Done),
                };
            }
        }
        if live
            .draw(Instant::now(), console::columns().unwrap_or(COLUMNS))
            .is_err()
        {
            return;
        }
    }
}

/// A step executing now, as the live view shows it.
struct Running {
    step: String,
    attempt: u32,
    attempts: u32,
    candidate: Option<String>,
    since: Instant,
}

/// The live view: what steps print and how they ended scrolls by, and below
/// it one row per step running now. The rows are taken back with
/// [`ERASE_ROW`] alone: the cursor is never hidden and the screen never
/// switched, so a run stopped at any point leaves the terminal as it was.
pub(super) struct Live<W> {
    pub(super) out: W,
    colour: bool,
    /// The steps executing now, in the order they started.
    running: Vec<Running>,
    /// Rows of running steps on screen now, below everything printed.
    drawn: usize,
    /// What is to be written next, sent in one write.
    frame: String,
}

impl<W: Write> Live<W> {
    pub(super) fn new(out: W, colour: bool) -> Self {
        Live {
            out,
            colour,
            running: Vec::new(),
            drawn: 0,
            frame: String::new(),
        }
    }

    /// `text` in the SGR colour `code` when colour is wanted.
    fn paint(&self, code: &str, text: &str) -> String {
        if self.colour {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub(super) fn event(&mut self, event: &Event, now: Instant) {
        match event {
            Event::RunStarted { .. } | Event::RunFinished { .. } => {}
            Event::StepStarted {
                step,
                attempt,
                attempts,
                candidate,
            } => {
                self.running.retain(|running| running.step != *step);
                self.running.push(Running {
                    step: step.clone(),
                    attempt: *attempt,
                    attempts: *attempts,
                    candidate: candidate.clone(),
                    since: now,
                });
            }
            Event::Output { step, source, line } => {
                let colour = match source {
                    Source::Stdout => "2",
                    Source::Stderr => "33",
                    Source::Proof => "36",
                };
                let tag = self.paint(colour, &terminal_safe(&tag(step, *source)));
                self.scroll(&format!("{tag} {}", terminal_safe(line)));
            }
            Event::StepCached {
                step,
                candidate,
                claims,
            } => {
                let text = format!(
                    " {step} {}{}{}",
                    outcome(StepStatus::Ok, *claims),
                    held(*claims),
                    which(0, 0, candidate.as_deref())
                );
                let mark = self.paint("36", "cached");
                self.scroll(&format!("{mark}{}", terminal_safe(&text)));
            }
            Event::StepFinished {
                step,
                attempt,
                candidate,
                status,
                duration_ms,
                claims,
                why,
            } => {
                let attempts = match self.running.iter().position(|r| r.step == *step) {
                    Some(index) => self.running.remove(index).attempts,
                    None => 0,
                };
                let text = format!(
                    " {step} {}{}",
                    ended(*status, *attempt, *duration_ms, *claims, why.as_deref()),
                    which(*attempt, attempts, candidate.as_deref())
                );
                let mark = match outcome(*status, *claims) {
                    "passed" => self.paint("32", "\u{2713}"),
                    _ => self.paint("31", "\u{2717}"),
                };
                self.scroll(&format!("{mark}{}", terminal_safe(&text)));
            }
        }
    }

    /// Print `line` above the running steps, taking their rows back first.
    fn scroll(&mut self, line: &str) {
        self.erase();
        self.frame.push_str(line);
        self.frame.push('\n');
    }

    fn erase(&mut self) {
        for _ in 0..self.drawn {
            self.frame.push_str(ERASE_ROW);
        }
        self.drawn = 0;
    }

    /// Redraw the running steps as of `now`, each cut to fit in `columns`
    /// so that it takes exactly one row, and write the frame.
    pub(super) fn draw(&mut self, now: Instant, columns: usize) -> std::io::Result<()> {
        self.erase();
        for running in &self.running {
            let elapsed = now.saturating_duration_since(running.since).as_millis();
            let elapsed = u64::try_from(elapsed).unwrap_or(u64::MAX);
            let line = format!(
                "  {} running {}{}",
                running.step,
                seconds(elapsed),
                which(
                    running.attempt,
                    running.attempts,
                    running.candidate.as_deref()
                )
            );
            let line: String = terminal_safe(&line)
                .chars()
                .take(columns.saturating_sub(1))
                .collect();
            self.frame.push_str(&line);
            self.frame.push('\n');
        }
        self.drawn = self.running.len();
        self.write()
    }

    /// Take the running steps' rows back, leaving the cursor where the view
    /// began.
    pub(super) fn clear(&mut self) -> std::io::Result<()> {
        self.erase();
        self.write()
    }

    fn write(&mut self) -> std::io::Result<()> {
        if self.frame.is_empty() {
            return Ok(());
        }
        let written = self.out.write_all(self.frame.as_bytes());
        self.frame.clear();
        written?;
        self.out.flush()
    }
}
