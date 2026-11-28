//! What `flot run` shows on stderr while it runs: each step as it starts,
//! the lines it prints, and how it ended. One thread owns stderr and takes
//! the events in the order the steps report them, so lines from steps that
//! run at once never mix within a line. Nothing here reaches stdout.

mod live;
mod plain;
#[cfg(test)]
mod tests;
mod words;

use std::io::IsTerminal;
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::service::progress::{Event, Sink};

use super::args::Progress;

use live::{show_live, Live};
use plain::show_plain;

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
