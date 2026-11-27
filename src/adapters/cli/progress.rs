//! What `flot run` shows on stderr while it runs: each step as it starts,
//! the lines it prints, and how it ended. One thread owns stderr and takes
//! the events in the order the steps report them, so lines from steps that
//! run at once never mix within a line. Nothing here reaches stdout.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::sync::mpsc::{self, RecvTimeoutError, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::core::receipt::{ClaimCounts, StepStatus};
use crate::service::progress::{Event, Sink, Source};

use super::args::Progress;
use super::terminal_safe;

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
const ERASE_ROW: &str = "\x1b[A\r\x1b[K";

enum Message {
    Event(Event),
    /// The run is over: clear the live view and stop.
    Done,
}

/// The thread that shows a run's progress. Dropping it clears the live view
/// and waits for the thread, so whatever is printed next starts where the
/// view was.
pub(super) struct View {
    sender: mpsc::Sender<Message>,
    thread: Option<JoinHandle<()>>,
}

impl View {
    /// Show progress as `asked`, or nothing (`None`). `auto` is live on a
    /// terminal that is not `dumb`, and plain elsewhere; a console that
    /// cannot act on escape sequences gets plain.
    pub(super) fn start(asked: Progress) -> Option<View> {
        let live = match asked {
            Progress::Off => return None,
            Progress::Plain => false,
            Progress::Live => true,
            Progress::Auto => {
                std::io::stderr().is_terminal()
                    && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
            }
        };
        let live = live && console::enable_escapes();
        let colour = std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty());
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let stderr = std::io::stderr();
            if live {
                show_live(&receiver, Live::new(stderr, colour));
            } else {
                show_plain(&receiver, stderr);
            }
        });
        Some(View {
            sender,
            thread: Some(thread),
        })
    }

    /// Where the run tells this view what happens.
    pub(super) fn sink(&self) -> Sink {
        let sender = self.sender.clone();
        Sink::new(move |event| {
            let _ = sender.send(Message::Event(event.clone()));
        })
    }
}

impl Drop for View {
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Done);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One line per event, without escape sequences, for logs.
fn show_plain(receiver: &mpsc::Receiver<Message>, mut out: impl Write) {
    let mut plain = Plain::default();
    for message in receiver {
        let Message::Event(event) = message else {
            return;
        };
        let line = format!("{}\n", terminal_safe(&plain.line(&event)));
        if out.write_all(line.as_bytes()).is_err() {
            return;
        }
    }
}

/// Output scrolling above a block of the steps running now, redrawn every
/// [`TICK`] and after each batch of events.
fn show_live<W: Write>(receiver: &mpsc::Receiver<Message>, mut live: Live<W>) {
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

/// What a step came to, in a word: one that is `ok` passed only once none of
/// its claims is left unverified or refuted.
fn outcome(status: StepStatus, claims: ClaimCounts) -> &'static str {
    match status {
        StepStatus::Ok if claims.refuted > 0 => "refuted",
        StepStatus::Ok if claims.unverified > 0 => "unverified",
        StepStatus::Ok => "passed",
        other => other.label(),
    }
}

/// ` attempt 2/3` for a step that may take more than one, and
/// ` candidate NAME` for one of a step's candidates.
fn which(attempt: u32, attempts: u32, candidate: Option<&str>) -> String {
    let mut text = String::new();
    if attempt > 0 && attempts > 1 {
        text.push_str(&format!(" attempt {attempt}/{attempts}"));
    }
    if let Some(name) = candidate {
        text.push_str(&format!(" candidate {name}"));
    }
    text
}

/// ` claims 2/3`: how many of its claims held, of how many, when it makes any.
fn held(claims: ClaimCounts) -> String {
    match claims.total() {
        0 => String::new(),
        total => format!(" claims {}/{total}", claims.verified),
    }
}

/// How a step ended: its outcome, time and claims, or, for one that never
/// executed, why it was not run, which says more than `cancelled`.
fn ended(
    status: StepStatus,
    attempt: u32,
    duration_ms: u64,
    claims: ClaimCounts,
    why: Option<&str>,
) -> String {
    if let Some(why) = why {
        return why.to_string();
    }
    // A step that never executed took no time worth telling.
    let took = match attempt {
        0 => String::new(),
        _ => format!(" {}", seconds(duration_ms)),
    };
    format!("{}{took}{}", outcome(status, claims), held(claims))
}

/// `52ms` under a tenth of a second, `1.2s` from there.
fn seconds(millis: u64) -> String {
    match millis {
        0..100 => format!("{millis}ms"),
        _ => format!("{}.{}s", millis / 1000, millis % 1000 / 100),
    }
}

/// `[build]`, `[build:stderr]` or `[build:proof]`.
fn tag(step: &str, source: Source) -> String {
    match source {
        Source::Stdout => format!("[{step}]"),
        Source::Stderr => format!("[{step}:stderr]"),
        Source::Proof => format!("[{step}:proof]"),
    }
}

fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// The plain view, which remembers only how many attempts each step may
/// take, for the line that tells how one ended.
#[derive(Default)]
struct Plain {
    attempts: HashMap<String, u32>,
}

impl Plain {
    fn line(&mut self, event: &Event) -> String {
        match event {
            Event::RunStarted { steps, jobs } => {
                format!("run {}, {}", plural(*steps, "step"), plural(*jobs, "job"))
            }
            Event::StepStarted {
                step,
                attempt,
                attempts,
                candidate,
            } => {
                self.attempts.insert(step.clone(), *attempts);
                format!(
                    "start {step}{}",
                    which(*attempt, *attempts, candidate.as_deref())
                )
            }
            Event::Output { step, source, line } => format!("{} {line}", tag(step, *source)),
            Event::StepCached {
                step,
                candidate,
                claims,
            } => format!(
                "cached {step} {}{}{}",
                outcome(StepStatus::Ok, *claims),
                held(*claims),
                which(0, 0, candidate.as_deref())
            ),
            Event::StepFinished {
                step,
                attempt,
                candidate,
                status,
                duration_ms,
                claims,
                why,
            } => {
                let attempts = self.attempts.get(step).copied().unwrap_or(0);
                format!(
                    "done {step} {}{}",
                    ended(*status, *attempt, *duration_ms, *claims, why.as_deref()),
                    which(*attempt, attempts, candidate.as_deref())
                )
            }
            Event::RunFinished { duration_ms } => format!("run finished {}", seconds(*duration_ms)),
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
struct Live<W> {
    out: W,
    colour: bool,
    /// The steps executing now, in the order they started.
    running: Vec<Running>,
    /// Rows of running steps on screen now, below everything printed.
    drawn: usize,
    /// What is to be written next, sent in one write.
    frame: String,
}

impl<W: Write> Live<W> {
    fn new(out: W, colour: bool) -> Self {
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

    fn event(&mut self, event: &Event, now: Instant) {
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
    fn draw(&mut self, now: Instant, columns: usize) -> std::io::Result<()> {
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
    fn clear(&mut self) -> std::io::Result<()> {
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

#[cfg(windows)]
mod console {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, SetConsoleMode,
        CONSOLE_SCREEN_BUFFER_INFO, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_ERROR_HANDLE,
    };

    /// Have the console stderr writes to act on escape sequences. False when
    /// stderr is no console, or one that cannot.
    pub(super) fn enable_escapes() -> bool {
        unsafe {
            let handle = GetStdHandle(STD_ERROR_HANDLE);
            let mut mode = 0;
            GetConsoleMode(handle, &mut mode) != 0
                && (mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
                    || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0)
        }
    }

    /// The width of the console window stderr writes to.
    pub(super) fn columns() -> Option<usize> {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        let found =
            unsafe { GetConsoleScreenBufferInfo(GetStdHandle(STD_ERROR_HANDLE), &mut info) } != 0;
        let width = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
        usize::try_from(width)
            .ok()
            .filter(|width| found && *width > 0)
    }
}

#[cfg(not(windows))]
mod console {
    /// A terminal here acts on escape sequences as it is.
    pub(super) fn enable_escapes() -> bool {
        true
    }

    /// The width of the terminal stderr writes to.
    pub(super) fn columns() -> Option<usize> {
        #[cfg(unix)]
        {
            let mut size: libc::winsize = unsafe { std::mem::zeroed() };
            let found =
                unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
            Some(usize::from(size.ws_col)).filter(|columns| found && *columns > 0)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(step: &str, attempt: u32, attempts: u32, candidate: Option<&str>) -> Event {
        Event::StepStarted {
            step: step.into(),
            attempt,
            attempts,
            candidate: candidate.map(str::to_string),
        }
    }

    fn output(step: &str, source: Source, line: &str) -> Event {
        Event::Output {
            step: step.into(),
            source,
            line: line.into(),
        }
    }

    fn finished(
        step: &str,
        attempt: u32,
        status: StepStatus,
        millis: u64,
        claims: ClaimCounts,
    ) -> Event {
        Event::StepFinished {
            step: step.into(),
            attempt,
            candidate: None,
            status,
            duration_ms: millis,
            claims,
            why: None,
        }
    }

    fn claims(verified: usize, unverified: usize, refuted: usize) -> ClaimCounts {
        ClaimCounts {
            verified,
            unverified,
            refuted,
        }
    }

    /// Two steps running at once, printing in turns: each frame takes back
    /// exactly the rows the last one drew, every line arrives whole, and the
    /// view ends with its rows taken back.
    #[test]
    fn the_live_view_takes_back_only_its_own_rows_and_keeps_lines_whole() {
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut live = Live::new(Vec::new(), false);
        live.event(&started("build", 1, 1, None), start);
        live.event(&started("test", 1, 3, None), start);
        live.draw(at(1200), 80).unwrap();
        live.event(&output("build", Source::Stdout, "compiling a"), at(1300));
        live.event(&output("test", Source::Stderr, "warning: slow"), at(1300));
        live.event(&output("build", Source::Stdout, "compiling b"), at(1400));
        live.draw(at(1500), 80).unwrap();
        live.event(&output("build", Source::Proof, "ok"), at(1600));
        live.event(
            &finished("build", 1, StepStatus::Ok, 1600, claims(2, 0, 0)),
            at(1600),
        );
        live.draw(at(2000), 80).unwrap();
        // Too narrow for its row: the row is cut, never wrapped.
        live.draw(at(2100), 12).unwrap();
        live.event(
            &finished("test", 1, StepStatus::Failed, 2200, claims(0, 1, 0)),
            at(2200),
        );
        live.clear().unwrap();

        let erase = |rows: usize| ERASE_ROW.repeat(rows);
        let expected = [
            "  build running 1.2s\n  test running 1.2s attempt 1/3\n".to_string(),
            erase(2),
            "[build] compiling a\n[test:stderr] warning: slow\n[build] compiling b\n".into(),
            "  build running 1.5s\n  test running 1.5s attempt 1/3\n".into(),
            erase(2),
            "[build:proof] ok\n\u{2713} build passed 1.6s claims 2/2\n".into(),
            "  test running 2.0s attempt 1/3\n".into(),
            erase(1),
            "  test runn\n".into(),
            erase(1),
            "\u{2717} test failed 2.2s claims 0/1 attempt 1/3\n".into(),
        ]
        .concat();
        let written = String::from_utf8(live.out).unwrap();
        assert_eq!(written, expected);
        assert_eq!(written.matches("\x1b[A").count(), 6);
        assert_eq!(written.matches("\x1b[K").count(), 6);
        // Nothing but rows going up and being cleared: no hidden cursor, no
        // other screen.
        assert_eq!(written.matches('\x1b').count(), 12);
    }

    #[test]
    fn the_live_view_paints_only_when_asked() {
        let mut live = Live::new(Vec::new(), true);
        live.event(&output("build", Source::Stderr, "careful"), Instant::now());
        live.event(
            &finished("build", 1, StepStatus::Ok, 0, claims(0, 0, 0)),
            Instant::now(),
        );
        live.clear().unwrap();
        let written = String::from_utf8(live.out).unwrap();
        assert_eq!(
            written,
            "\x1b[33m[build:stderr]\x1b[0m careful\n\x1b[32m\u{2713}\x1b[0m build passed 0ms\n"
        );
    }

    /// A step's output can carry escape sequences of its own; they are shown,
    /// not acted on.
    #[test]
    fn a_step_cannot_drive_the_terminal_through_the_view() {
        let mut live = Live::new(Vec::new(), false);
        live.event(
            &output("build", Source::Stdout, "\x1b[2Jgone\r"),
            Instant::now(),
        );
        live.clear().unwrap();
        assert_eq!(
            String::from_utf8(live.out).unwrap(),
            "[build] \\u{1b}[2Jgone\\u{d}\n"
        );
        let mut plain = Plain::default();
        let line = plain.line(&output("build", Source::Stdout, "\x1b[2Jgone"));
        assert_eq!(terminal_safe(&line), "[build] \\u{1b}[2Jgone");
    }

    #[test]
    fn the_plain_view_says_which_attempt_and_candidate_each_line_is() {
        let mut plain = Plain::default();
        let lines: Vec<String> = [
            Event::RunStarted { steps: 1, jobs: 2 },
            started("flaky", 1, 2, None),
            finished("flaky", 1, StepStatus::Failed, 120, claims(0, 1, 0)),
            started("flaky", 2, 2, None),
            finished("flaky", 2, StepStatus::Ok, 1999, claims(1, 0, 0)),
            started("pick", 1, 1, Some("fast")),
            Event::StepFinished {
                step: "pick".into(),
                attempt: 1,
                candidate: Some("fast".into()),
                status: StepStatus::Ok,
                duration_ms: 50,
                claims: claims(0, 0, 1),
                why: None,
            },
            Event::StepCached {
                step: "lint".into(),
                candidate: None,
                claims: claims(1, 0, 0),
            },
            Event::StepFinished {
                step: "ship".into(),
                attempt: 0,
                candidate: None,
                status: StepStatus::Cancelled,
                duration_ms: 0,
                claims: claims(0, 1, 0),
                why: Some("not run: dependency lint (failed) did not hold".into()),
            },
            Event::RunFinished { duration_ms: 12345 },
        ]
        .iter()
        .map(|event| plain.line(event))
        .collect();
        assert_eq!(
            lines,
            [
                "run 1 step, 2 jobs",
                "start flaky attempt 1/2",
                "done flaky failed 0.1s claims 0/1 attempt 1/2",
                "start flaky attempt 2/2",
                "done flaky passed 1.9s claims 1/1 attempt 2/2",
                "start pick candidate fast",
                "done pick refuted 50ms claims 0/1 candidate fast",
                "cached lint passed claims 1/1",
                "done ship not run: dependency lint (failed) did not hold",
                "run finished 12.3s",
            ]
        );
    }
}
