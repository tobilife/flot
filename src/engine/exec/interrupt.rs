//! Being asked to stop: the flag flot's signal handlers raise, and the
//! signal that withdraws one run.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Set once flot is asked to stop (SIGINT, SIGTERM, SIGHUP). Running commands
/// are stopped with their process groups, steps not yet started are cancelled,
/// and the run still records what happened.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Whether flot has been asked to stop.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// One run's way to be withdrawn by whoever started it (an MCP host
/// cancelling a tool call). A withdrawn run stops exactly as an interrupted
/// one does, recording what happened, but flot stays up and other runs are
/// untouched.
#[derive(Debug, Clone, Default)]
pub struct StopSignal(Arc<AtomicBool>);

impl StopSignal {
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether the run must stop: it was withdrawn, or flot was interrupted.
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst) || interrupted()
    }
}

/// Route SIGINT, SIGTERM and SIGHUP to the interrupt flag. Every step leads
/// its own process group, so a terminal's Ctrl-C reaches only flot; without
/// this, flot would die and leave the step running, unrecorded. A second
/// signal exits at once, for a user who does not want to wait.
pub fn install_interrupt_handlers() {
    // Windows: Ctrl-C and closing the console. Steps start in their own
    // process group, which a console Ctrl-C does not reach.
    #[cfg(windows)]
    {
        unsafe extern "system" fn on_ctrl(_: u32) -> windows_sys::core::BOOL {
            if INTERRUPTED.swap(true, Ordering::SeqCst) {
                unsafe { windows_sys::Win32::System::Threading::ExitProcess(130) };
            }
            1
        }
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_ctrl), 1);
        }
    }
    #[cfg(unix)]
    {
        extern "C" fn on_signal(_: libc::c_int) {
            if INTERRUPTED.swap(true, Ordering::SeqCst) {
                // Only async-signal-safe calls here.
                unsafe { libc::_exit(130) };
            }
        }
        let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            unsafe {
                libc::signal(signal, handler);
            }
        }
    }
}
