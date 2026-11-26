//! Reserved words, wrappers, shells, interpreters and compressors.

/// Reserved words a command can follow: `if rm x`, `then rm x`, `! rm x`,
/// `{ rm x; }`.
pub(super) const KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "do", "while", "until", "!", "{", "coproc",
];
/// Programs that run the rest of their arguments as a command: the letters
/// and long names of their options that take a value, and how many
/// operands come before the command (`timeout 5 rm x`).
pub(super) const WRAPPERS: &[(&str, &str, &[&str], usize)] = &[
    (
        "sudo",
        "ugpChDrtTUR",
        &[
            "user",
            "group",
            "prompt",
            "close-from",
            "chdir",
            "role",
            "type",
            "command-timeout",
            "other-user",
            "chroot",
            "host",
        ],
        0,
    ),
    ("doas", "uC", &[], 0),
    ("env", "uCS", &["unset", "chdir", "split-string"], 0),
    ("command", "", &[], 0),
    ("nohup", "", &[], 0),
    ("time", "fo", &["format", "output"], 0),
    ("nice", "n", &["adjustment"], 0),
    ("exec", "a", &[], 0),
    ("timeout", "ks", &["kill-after", "signal"], 1),
    ("stdbuf", "ioe", &["input", "output", "error"], 0),
    ("ionice", "cnp", &["class", "classdata", "pid"], 0),
    ("setsid", "", &[], 0),
    ("busybox", "", &[], 0),
    ("watch", "n", &["interval"], 0),
    (
        "xargs",
        "ILnPsdEa",
        &[
            "arg-file",
            "delimiter",
            "max-args",
            "max-procs",
            "max-chars",
            "max-lines",
            "process-slot-var",
        ],
        0,
    ),
];
pub(super) const FETCHERS: &[&str] = &["curl", "wget"];
/// Shells, which run the string after `-c` or the commands on their input.
pub(super) const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "fish"];

/// An interpreter whose code this reader hands back for what it names,
/// and how its options give that code.
pub(super) struct Language {
    /// Its names; each also with a version after it, as `python3.12`.
    names: &'static [&'static str],
    /// Options whose value is code, as `-c` or `--eval`.
    pub(super) code: &'static [&'static str],
    /// Options whose value is the script, as php's `-f`.
    pub(super) script: &'static [&'static str],
    /// Other options that take a value.
    pub(super) valued: &'static [&'static str],
    /// Letters that take the rest of their word, however short, as perl's
    /// `-i.bak`.
    pub(super) attached: &'static str,
    /// Whether a letter's value may be the rest of its word, as `-cCODE`;
    /// otherwise it is the next word.
    pub(super) glued: bool,
    /// Options past which every word is the program's own, as python's `-c`
    /// and `-m`.
    pub(super) ends: &'static [&'static str],
    /// Whether its first operand may be `eval`, whose next is code, or
    /// `run`, whose next is the script.
    pub(super) subcommands: bool,
}

const LANGUAGES: &[Language] = &[
    Language {
        names: &["python", "pypy"],
        code: &["c"],
        script: &[],
        valued: &["W", "X", "check-hash-based-pycs"],
        attached: "",
        glued: true,
        ends: &["c", "m"],
        subcommands: false,
    },
    Language {
        names: &["node", "nodejs"],
        code: &["e", "p", "eval", "print"],
        script: &[],
        valued: &[
            "r",
            "require",
            "import",
            "loader",
            "experimental-loader",
            "C",
            "conditions",
            "input-type",
            "env-file",
            "title",
        ],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["deno"],
        code: &["eval"],
        script: &[],
        valued: &["c", "config", "import-map", "lock", "cert", "env-file"],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: true,
    },
    Language {
        names: &["bun"],
        code: &["e", "p", "eval", "print"],
        script: &[],
        valued: &["r", "preload", "cwd", "env-file", "config"],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: true,
    },
    Language {
        names: &["perl"],
        code: &["e", "E"],
        script: &[],
        valued: &[],
        attached: "iIMmxdDC",
        glued: true,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["ruby"],
        code: &["e"],
        script: &[],
        valued: &["r", "I", "C", "E", "F", "encoding"],
        attached: "ixWKT",
        glued: true,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["php"],
        code: &["r", "B", "R", "E"],
        script: &["f"],
        valued: &["c", "d", "t", "S", "z"],
        attached: "",
        glued: true,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["Rscript"],
        code: &["e"],
        script: &[],
        valued: &[],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["lua", "luajit"],
        code: &["e"],
        script: &[],
        valued: &["l"],
        attached: "",
        glued: true,
        ends: &[],
        subcommands: false,
    },
    Language {
        names: &["osascript"],
        code: &["e"],
        script: &[],
        valued: &["l", "s"],
        attached: "",
        glued: false,
        ends: &[],
        subcommands: false,
    },
];

/// The interpreter a program is, by its name, if this reader knows it.
pub(super) fn language(name: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|language| {
        language.names.iter().any(|known| {
            name.strip_prefix(known)
                .is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
    })
}

/// Whether a program runs the code it is given: a shell or an interpreter
/// this reader knows.
pub(super) fn runs_code(name: &str) -> bool {
    SHELLS.contains(&name) || language(name).is_some()
}

pub(super) const COMPRESSORS: &[&str] = &[
    "gzip",
    "gunzip",
    "bzip2",
    "bunzip2",
    "xz",
    "unxz",
    "lzma",
    "unlzma",
    "zstd",
    "unzstd",
    "compress",
    "uncompress",
    "lz4",
    "unlz4",
    "pigz",
    "unpigz",
    "brotli",
];
