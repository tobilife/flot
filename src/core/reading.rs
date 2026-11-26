//! What a command line names, as both readers report it: the model
//! [`shell`](super::shell) and [`powershell`](super::powershell) share,
//! and the limits on how far either reads, which must agree because code
//! one hands over is read by the other.

/// What a command line names.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// Paths it writes, deletes, moves or changes the mode of, as written:
    /// a glob, a brace, `$PWD`, `$HOME`, `~` or a directory is the caller's
    /// to expand, `.` stands for the whole tree (`git stash`, `tar -x`), and
    /// [`BUILT`] for a part built when the line runs. A sed script or an
    /// option's value can be among them: a caller looks for the files it
    /// cares about, not for what is missing.
    pub writes: Vec<String>,
    /// `fetcher | interpreter` when it pipes downloaded code into one.
    pub remote_code: Option<String>,
    /// Code given to an interpreter on its command line or its input.
    pub code: Vec<Code>,
    /// Script files a shell or an interpreter is given to run.
    pub scripts: Vec<Script>,
    /// Commands whose code no reading of the line can see.
    pub unread: Vec<Unread>,
    /// Every word the line's commands are given, and the files redirected
    /// into them, located as `writes` are: where a caller looks for a file
    /// that must not even be read.
    pub named: Vec<String>,
}

/// Code an interpreter is given, which this reader does not read: only the
/// files it names can be looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// The interpreter, as `python3`.
    pub program: String,
    pub text: String,
    /// The directory it runs in, from where the line starts; empty for there.
    pub dir: String,
}

/// A script file a command runs, for the caller to read from the disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// Its path as written, from where the line starts.
    pub path: String,
    /// Its path as the command named it, from where it runs: `$0` in it,
    /// unless it is sourced.
    pub named: String,
    /// Whether `.` or `source` reads it into the shell running the
    /// command, where `$0` stays what it was.
    pub sourced: bool,
    /// The directory it runs in, from where the line starts; empty for there.
    pub dir: String,
    pub runner: Runner,
    /// How many times the code naming it was handed over as a string: its
    /// own text is read one deeper.
    pub depth: usize,
    /// What the line writes into it before running it, when the line
    /// spells that: a here-document or an `echo` redirected into it.
    pub text: Option<String>,
}

/// What runs a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    /// A POSIX shell, which reads it as commands.
    Shell,
    /// PowerShell.
    PowerShell,
    /// An interpreter, by its name: its text is code.
    Interpreter(String),
    /// The system, as a program: its `#!` line names what runs it, and a
    /// text file with none is run by the shell.
    Program,
}

/// A command whose code this reader cannot see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unread {
    /// The command as spelled, such as `sh $(..)`.
    pub shape: String,
    /// Why its code cannot be seen, said of the command.
    pub why: String,
}

/// What a word holds in place of what a command substituted in it, `$(..)`
/// or backquotes, prints, when this reader does not work it out: a value
/// it cannot know, as an unset variable's is. A word that is nothing else
/// names nothing; one with a part spelled around it, as `$(..)/check.sh`,
/// is that part, in any directory.
pub const BUILT: char = '\u{FFFC}';

/// What a word holds in place of what a program that makes text prints,
/// when a command substituted in it runs one: `printf`, `echo`, `base64`,
/// `sed`, an interpreter, `cat` or `curl`. That text can spell any name or
/// any code: such a word may name every guarded place, and code built from
/// it is refused.
pub const SPELLED: char = '\u{FFFB}';

/// What `$(mktemp ..)` stands for in a word: `mktemp` prints the name of a
/// file or directory it has just created, so the name is no file that was
/// there before, and the agent's `T=$(mktemp -d); ...; rm -rf $T` names
/// nothing a flow protects. The name is a fixed one outside any project.
pub const MKTEMP: &str = "/tmp/flot-mktemp.XXXXXX";

/// How deep code handed to a shell as a string is read again; past it,
/// every word is taken for a path it may write.
pub(super) const DEPTH: usize = 8;
/// How many of the commands `xargs -I` runs are read; past them, each item
/// is taken for a path it may write.
pub(super) const RUNS: usize = 64;

pub(super) fn merge(into: &mut Reading, found: Reading) {
    into.writes.extend(found.writes);
    if into.remote_code.is_none() {
        into.remote_code = found.remote_code;
    }
    into.code.extend(found.code);
    into.scripts.extend(found.scripts);
    into.unread.extend(found.unread);
    into.named.extend(found.named);
}

/// `word` as it is shown, with what a command substituted in it prints as
/// `$(..)`.
pub fn shown(word: &str) -> String {
    word.replace([BUILT, SPELLED], "$(..)")
}
