//! Cmdlets, their parameters, and how their arguments bind.

/// What a cmdlet does to the paths it is given.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// Writes, clears or removes its path.
    Changes,
    /// Creates its path, or `-Name` under it.
    Creates,
    /// Removes its path and writes its destination.
    Moves,
    /// Writes its destination.
    Copies,
    /// Removes its path and writes `-NewName` beside it.
    Renames,
    /// Writes its `-OutFile`.
    Downloads,
}

/// Cmdlets, under their names and their built-in aliases, lowercase.
pub(super) const CMDLETS: &[(&[&str], Kind)] = &[
    (
        &[
            "set-content",
            "sc",
            "add-content",
            "ac",
            "clear-content",
            "clc",
            "clear-item",
            "cli",
            "set-item",
            "si",
            "out-file",
            "remove-item",
            "rm",
            "ri",
            "del",
            "erase",
            "rd",
            "rmdir",
            "tee-object",
            "tee",
            "export-csv",
            "epcsv",
            "export-clixml",
            "set-itemproperty",
            "sp",
            "clear-itemproperty",
            "clp",
            "remove-itemproperty",
            "rp",
            "new-itemproperty",
            "set-acl",
        ],
        Kind::Changes,
    ),
    (&["new-item", "ni", "mkdir", "md"], Kind::Creates),
    (&["move-item", "mv", "mi", "move"], Kind::Moves),
    (&["copy-item", "cp", "cpi", "copy"], Kind::Copies),
    (&["rename-item", "ren", "rni"], Kind::Renames),
    (
        &["invoke-webrequest", "iwr", "invoke-restmethod", "irm"],
        Kind::Downloads,
    ),
];

/// Parameters whose value is the path a cmdlet acts on.
pub(super) const PATH_PARAMETERS: &[&str] = &["path", "literalpath", "filepath", "pspath", "lp"];
/// Other parameters that take a value, the common parameters among them.
const VALUE_PARAMETERS: &[&str] = &[
    "value",
    "encoding",
    "itemtype",
    "type",
    "name",
    "newname",
    "destination",
    "filter",
    "include",
    "exclude",
    "width",
    "delimiter",
    "inputobject",
    "credential",
    "stream",
    "target",
    "variable",
    "outfile",
    "uri",
    "method",
    "headers",
    "body",
    "erroraction",
    "ea",
    "warningaction",
    "wa",
    "informationaction",
    "infa",
    "progressaction",
    "proga",
    "errorvariable",
    "ev",
    "warningvariable",
    "wv",
    "informationvariable",
    "iv",
    "outvariable",
    "ov",
    "outbuffer",
    "ob",
    "pipelinevariable",
    "pv",
];
/// Parameters that take no value.
const SWITCHES: &[&str] = &[
    "force",
    "recurse",
    "nonewline",
    "append",
    "noclobber",
    "passthru",
    "confirm",
    "cf",
    "whatif",
    "wi",
    "verbose",
    "vb",
    "debug",
    "db",
    "usetransaction",
    "usetx",
    "asbytestream",
    "container",
    "notypeinformation",
];

/// A parameter as written, `-Dest` or `-Path:x`, by its full name: the
/// name or alias written in full, or else the one name it is a prefix of.
fn parameter(written: &str) -> Option<&'static str> {
    let written = written.to_ascii_lowercase();
    let names = || {
        PATH_PARAMETERS
            .iter()
            .chain(VALUE_PARAMETERS)
            .chain(SWITCHES)
    };
    if let Some(exact) = names().find(|name| **name == written) {
        return Some(exact);
    }
    let mut prefixed = names().filter(|name| name.starts_with(&written));
    match (prefixed.next(), prefixed.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Code that fetches from the network, and what runs a string as code.
pub(super) const FETCHERS: &[&str] = &[
    "invoke-webrequest",
    "iwr",
    "invoke-restmethod",
    "irm",
    "curl",
    "wget",
];
pub(super) const EVALUATORS: &[&str] = &["invoke-expression", "iex"];

/// What a statement pipes into the next that may name paths.
#[derive(Default)]
pub(super) struct Piped {
    pub(super) paths: Vec<String>,
    /// Items from `Get-Item` or `Get-ChildItem`, which a cmdlet binds to
    /// its path by property name; a string binds only where the path is
    /// taken by value.
    pub(super) items: bool,
}

/// One argument: a word, or words joined by commas into an array.
pub(super) type Argument = Vec<(String, bool)>;

/// The command a statement runs, lowercase: past the call operator's
/// `.`, a module qualifier and `.exe`.
pub(super) fn command_name(word: &str) -> String {
    let name = word
        .rsplit('\\')
        .next()
        .unwrap_or(word)
        .to_ascii_lowercase();
    name.strip_suffix(".exe")
        .map(str::to_string)
        .unwrap_or(name)
}

/// A cmdlet's arguments, bound to the parameters this reader knows.
pub(super) struct Bound {
    pub(super) named: Vec<(&'static str, Vec<String>)>,
    pub(super) positional: Vec<Vec<String>>,
    /// A parameter this reader does not know was given: any positional
    /// argument may be its value or the path.
    pub(super) unknown: bool,
}

impl Bound {
    pub(super) fn new(arguments: &[Argument]) -> Self {
        let mut bound = Bound {
            named: Vec::new(),
            positional: Vec::new(),
            unknown: false,
        };
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            let words: Vec<String> = argument.iter().map(|(word, _)| word.clone()).collect();
            let flag = match argument.as_slice() {
                [(word, false)] if word.len() > 1 && word.starts_with('-') => Some(&word[1..]),
                _ => None,
            };
            let Some(flag) = flag else {
                bound.positional.push(words);
                continue;
            };
            let (written, inline) = match flag.split_once(':') {
                Some((written, value)) => (written, Some(value.to_string())),
                None => (flag, None),
            };
            match parameter(written) {
                Some(name) if SWITCHES.contains(&name) => {}
                Some(name) => {
                    let values = match inline {
                        Some(value) => vec![value],
                        None => rest
                            .next()
                            .map(|value| value.iter().map(|(word, _)| word.clone()).collect())
                            .unwrap_or_default(),
                    };
                    bound.named.push((name, values));
                }
                None => bound.unknown = true,
            }
        }
        bound
    }

    /// The values given to `wanted` by name, else the positional argument
    /// at `position`.
    pub(super) fn values(&self, wanted: &[&str], position: usize) -> Vec<String> {
        let found: Vec<String> = self
            .named
            .iter()
            .filter(|(name, _)| wanted.contains(name))
            .flat_map(|(_, values)| values.clone())
            .collect();
        if found.is_empty() {
            self.positional.get(position).cloned().unwrap_or_default()
        } else {
            found
        }
    }
}
