//! The plain view, for logs: one line per event, without escape sequences.

use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc;

use crate::core::receipt::StepStatus;
use crate::service::progress::Event;

use super::super::terminal_safe;
use super::words::{ended, held, outcome, seconds, tag, which};
use super::Message;

/// One line per event, without escape sequences, for logs.
pub(super) fn show_plain(receiver: &mpsc::Receiver<Message>, mut out: impl Write) {
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

fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// The plain view, which remembers only how many attempts each step may
/// take, for the line that tells how one ended.
#[derive(Default)]
pub(super) struct Plain {
    attempts: HashMap<String, u32>,
}

impl Plain {
    pub(super) fn line(&mut self, event: &Event) -> String {
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
