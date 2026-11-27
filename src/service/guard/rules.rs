//! The rules a call is judged by: every flow that could own a file it
//! changes, each read once, and why changing or reading a path is refused.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::service::{keys, Workspace, DEFAULT_FLOW_FILE};

use super::expand::home_dir;
#[cfg(unix)]
use super::paths::files_below;
use super::paths::{
    below, folded, harness, normalize, on_disk, proof_match, resolve, spellings, strip_base,
    user_harness, FOLDS_CASE, GUARD_WALK_LIMIT,
};
use super::Guarded;

/// One flow as the guard reads it, once per call.
struct Guarding {
    workspace: Workspace,
    /// The flow file, resolved, which tells two spellings of one flow apart.
    key: PathBuf,
    /// Each proof pattern, compiled and as written, with its step.
    proofs: Vec<(glob::Pattern, String, String)>,
    /// The flow's directory, as named and as resolved.
    roots: Vec<PathBuf>,
    /// The repository the flow is in, which may hold it in a subdirectory,
    /// else the flow's own directory; as named and as resolved.
    tops: Vec<PathBuf>,
}

impl Guarding {
    /// Why `relative` (to the flow's directory) is part of this flow's
    /// evidence, if it is.
    fn evidence(&self, relative: &str) -> Option<String> {
        let name = folded(relative);
        let flow_file = self
            .workspace
            .flow_path
            .file_name()
            .map(|file| folded(&file.to_string_lossy()));
        let store = crate::store::STORE_DIR;
        let reason = if Some(&name) == flow_file.as_ref() {
            "it is the flow file, which says what proves the work".to_string()
        } else if name == store || name.starts_with(&format!("{store}/")) {
            "it is in the flot store, which holds the evidence".to_string()
        } else if let Some((_, _, step)) = self
            .proofs
            .iter()
            .find(|(pattern, _, _)| pattern.matches_with(relative, proof_match()))
        {
            format!("the proof of step `{step}` reads it")
        } else if name.rsplit('/').next() == Some(DEFAULT_FLOW_FILE) {
            "it would be another flow file in this flow's project, which can change what proves the work and which flow guards it".to_string()
        } else {
            return None;
        };
        Some(reason)
    }
}

/// What one call is judged against: every flow that could own a file it
/// changes, each read once, and what outside them decides whether proofs
/// are checked: the user's Claude Code settings and plugins, and flot.
pub(super) struct Guard {
    /// Nearest first: a file is named, and guarded, by the deepest flow
    /// that holds it.
    flows: Vec<Guarding>,
    /// Directories already looked in for a flow file.
    looked: std::collections::HashSet<PathBuf>,
    home: Vec<PathBuf>,
    /// The flot binary running the guard, resolved.
    exe: Option<PathBuf>,
    /// The signing key, as named and as resolved; none when it has no place.
    key: Vec<PathBuf>,
}

impl Guard {
    /// A guard of `workspace`'s flow, if it has one; [`Guard::consult`] adds
    /// the others.
    pub(super) fn new(workspace: &Workspace) -> Self {
        let home = home_dir();
        let mut guard = Guard {
            flows: Vec::new(),
            looked: std::collections::HashSet::new(),
            home: home.map(|home| spellings(&home)).unwrap_or_default(),
            exe: std::env::current_exe().ok().map(|exe| resolve(&exe)),
            key: keys::default_path()
                .map(|key| spellings(&key))
                .unwrap_or_default(),
        };
        guard.add(workspace);
        guard
    }

    fn add(&mut self, workspace: &Workspace) {
        if !workspace.flow_path.is_file() {
            return;
        }
        let key = resolve(&workspace.flow_path);
        let top = workspace
            .root
            .ancestors()
            .find(|dir| dir.join(".git").exists())
            .unwrap_or(&workspace.root);
        // A flow reached through a link too names its files from there.
        if let Some(known) = self.flows.iter_mut().find(|known| known.key == key) {
            for (spellings, dir) in [
                (&mut known.roots, workspace.root.as_path()),
                (&mut known.tops, top),
            ] {
                for spelling in self::spellings(dir) {
                    if !spellings.contains(&spelling) {
                        spellings.push(spelling);
                    }
                }
            }
            return;
        }
        // A flow file that does not parse still guards itself, the store
        // and the harness.
        let proofs = workspace
            .flow()
            .map(|flow| {
                flow.steps
                    .iter()
                    .flat_map(|step| {
                        step.verify_inputs.iter().filter_map(|pattern| {
                            let compiled = glob::Pattern::new(pattern).ok()?;
                            Some((compiled, pattern.clone(), step.id.clone()))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.flows.push(Guarding {
            workspace: workspace.clone(),
            key,
            proofs,
            roots: spellings(&workspace.root),
            tops: spellings(top),
        });
        self.flows
            .sort_by_key(|flow| std::cmp::Reverse(flow.roots[0].components().count()));
    }

    /// Add every flow file from `dir` up to the top of its repository: one
    /// that can be created below a flow does not hide the flow above it.
    pub(super) fn consult(&mut self, dir: &Path) {
        for spelling in spellings(dir) {
            for ancestor in spelling.ancestors() {
                if !self.looked.insert(ancestor.to_path_buf()) {
                    break;
                }
                let flow_path = ancestor.join(DEFAULT_FLOW_FILE);
                if flow_path.is_file() {
                    self.add(&Workspace::beside(&flow_path));
                }
                if ancestor.join(".git").exists() {
                    break;
                }
            }
        }
    }

    /// Whether any flow guards the call. Where none does, nothing is flot's
    /// to refuse.
    pub(super) fn active(&self) -> bool {
        !self.flows.is_empty()
    }

    /// The flows found, for a refusal to be noted in.
    pub(super) fn workspaces(&self) -> impl Iterator<Item = &Workspace> {
        self.flows.iter().map(|flow| &flow.workspace)
    }

    /// Why changing `path` (absolute), or anything below it, would touch a
    /// flow's proofs or decide whether they are checked, if it would. The
    /// path is judged as named, then where it leads through links, then as
    /// another name of a guarded file, then, for a directory, by what it
    /// holds.
    pub(super) fn judge(&self, path: &Path) -> Option<Guarded> {
        if self.flows.is_empty() {
            return None;
        }
        let named = normalize(path);
        if let Some((path, reason)) = self.rule(&named) {
            return Some(Guarded { path, reason });
        }
        let resolved = resolve(&named);
        let reason = (resolved != named)
            .then(|| self.rule(&resolved))
            .flatten()
            .map(|(to, reason)| format!("it leads to {to}: {reason}"))
            .or_else(|| {
                self.hard_link(&resolved)
                    .map(|(to, reason)| format!("it is a hard link to {to}: {reason}"))
            })
            .or_else(|| resolved.is_dir().then(|| self.holds(&resolved)).flatten())?;
        Some(Guarded {
            path: self.name(&named),
            reason,
        })
    }

    /// Why reading `path` (absolute) would let an agent sign runs as the
    /// user, if it would: it is the signing key, a copy flot keeps of one,
    /// the host secret beside it, or the directory that holds them, as
    /// named or where it leads through links. Only a path that goes by the
    /// name of one is followed, as through a link to the directory above
    /// the key: a link of another name to the key itself is made by a
    /// command that names the key. Unlike what [`Guard::judge`] finds, a
    /// secret is one with or without a flow.
    pub(super) fn secret(&self, path: &Path) -> Option<Guarded> {
        let named = normalize(path);
        let reason = self.secret_reason(&named).or_else(|| {
            let name = folded(&named.file_name()?.to_string_lossy());
            let known = self.key.iter().any(|key| {
                let file = key.file_name().map(|file| folded(&file.to_string_lossy()));
                let dir = key
                    .parent()
                    .and_then(Path::file_name)
                    .map(|dir| folded(&dir.to_string_lossy()));
                file.is_some_and(|file| {
                    name == file
                        || name.starts_with(&format!("{file}."))
                        || name.starts_with(&format!(".{file}."))
                }) || dir.as_deref() == Some(name.as_str())
                    || name == "host.secret"
            });
            if !known {
                return None;
            }
            self.secret_reason(&named.canonicalize().ok()?)
        })?;
        Some(Guarded {
            path: self.name(&named),
            reason: reason.to_string(),
        })
    }

    fn secret_reason(&self, path: &Path) -> Option<&'static str> {
        for key in &self.key {
            let (Some(dir), Some(file)) = (key.parent(), key.file_name()) else {
                continue;
            };
            let Some(relative) = strip_base(path, dir, FOLDS_CASE) else {
                continue;
            };
            if relative.is_empty() {
                // A directory that holds the work too, such as a project the
                // key was put in, is named for the work.
                let holds_work = self
                    .flows
                    .iter()
                    .flat_map(|flow| flow.roots.iter().chain(&flow.tops))
                    .chain(&self.home)
                    .any(|base| strip_base(base, dir, FOLDS_CASE).is_some());
                if !holds_work {
                    return Some(
                        "it holds the signing key, and an agent that reads it can sign runs as the user",
                    );
                }
                continue;
            }
            let name = folded(&relative);
            let file = folded(&file.to_string_lossy());
            if name == file
                || name.starts_with(&format!("{file}."))
                || name.starts_with(&format!(".{file}."))
            {
                return Some(
                    "it is the signing key, or a copy flot keeps of one, and an agent that reads it can sign runs as the user",
                );
            }
            if name == "host.secret" {
                return Some(
                    "it is the host secret beside the signing key, which seals this host's trust list, and an agent that reads it can make a key of its own trusted and sign runs as the user",
                );
            }
        }
        None
    }

    /// The secrets as they are on disk, for a wildcard to be matched
    /// against: the signing key, the host secret, what else in their
    /// directory is one, and the directory.
    pub(super) fn secrets(&self) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for key in &self.key {
            let Some(dir) = key.parent() else {
                continue;
            };
            found.push(key.clone());
            found.push(dir.join("host.secret"));
            found.push(dir.to_path_buf());
            if let Ok(entries) = std::fs::read_dir(dir) {
                found.extend(
                    entries
                        .flatten()
                        .map(|entry| entry.path())
                        .filter(|path| self.secret_reason(path).is_some()),
                );
            }
        }
        let mut seen = std::collections::HashSet::new();
        found.retain(|path| seen.insert(path.clone()));
        found
    }

    /// Where the guard names files from: each flow's directory and
    /// repository, and the user's home directory.
    fn bases(&self) -> Vec<&Path> {
        self.flows
            .iter()
            .flat_map(|flow| [flow.roots[0].as_path(), flow.tops[0].as_path()])
            .chain(self.home.first().map(PathBuf::as_path))
            .collect()
    }

    /// `place` and each directory it is in, up to the flow, repository or
    /// home directory that holds it: what changing would change it. A place
    /// outside them all is only itself.
    fn above(place: &Path, bases: &[&Path]) -> Vec<PathBuf> {
        let mut found = vec![place.to_path_buf()];
        for dir in place.ancestors().skip(1) {
            if !bases.iter().any(|base| dir.starts_with(base)) {
                break;
            }
            found.push(dir.to_path_buf());
            if bases.contains(&dir) {
                break;
            }
        }
        found
    }

    /// The guarded places, and the directories that hold them, that
    /// `pattern` matches: what a command that changes a path built when it
    /// runs may change. A `*` in it spans directories. A place in a
    /// directory that matches too is left to that directory's reason.
    pub(super) fn matching(&self, pattern: &str) -> Vec<PathBuf> {
        if self.flows.is_empty() {
            return Vec::new();
        }
        let Ok(pattern) = glob::Pattern::new(pattern) else {
            return Vec::new();
        };
        let options = glob::MatchOptions {
            case_sensitive: !FOLDS_CASE,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        };
        let bases = self.bases();
        let mut found = Vec::new();
        for place in self.places() {
            for path in Self::above(&place, &bases) {
                if pattern.matches_path_with(&path, options) && !found.contains(&path) {
                    found.push(path);
                }
            }
        }
        let held: Vec<PathBuf> = found
            .iter()
            .filter(|path| {
                found
                    .iter()
                    .any(|dir| dir != *path && path.starts_with(dir))
            })
            .cloned()
            .collect();
        found.retain(|path| !held.contains(path));
        found
    }

    /// What `text`, code run in `dir`, names of the guarded places, the
    /// directories that hold them and the secrets: a word in it that is one
    /// by its path from a flow, its repository, the home directory or
    /// `dir`, by its absolute path, or by a file name with a dot in it, or
    /// a path inside a guarded directory. A path is taken as written, but
    /// one that follows what the code puts in, as `$ROOT/check.sh` or
    /// `{root}/check.sh`, may start anywhere, so its tail is matched too. A
    /// word with neither a dot nor a slash counts only as a string, in
    /// quotes: unquoted, as `tests` in a comment, it is code. A guarded
    /// directory, named alone, by a path or as where a path starts, counts
    /// only in code that calls something that removes, moves or writes
    /// files: code that lists `("scripts", "tests")` or joins `root` and
    /// `"cache"` to read them cannot change what the directory holds. A
    /// guarded file or a secret counts wherever it is named. Code is not
    /// read for what it does: what it names is all that can be told, and a
    /// name it builds from parts is not seen.
    pub(super) fn named_in(&self, text: &str, dir: &Path) -> Vec<PathBuf> {
        let changes = changes_files(text);
        let mut exact: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut inside: BTreeMap<String, PathBuf> = BTreeMap::new();
        let bases = self.bases();
        let mut note = |path: &Path, place: bool| {
            let mut names: Vec<String> = vec![path.to_string_lossy().replace('\\', "/")];
            for base in bases.iter().copied().chain([dir]) {
                if let Some(relative) = strip_base(path, base, FOLDS_CASE) {
                    if !relative.is_empty() {
                        if self.home.first().map(PathBuf::as_path) == Some(base) {
                            names.push(format!("~/{relative}"));
                        }
                        names.push(relative);
                    }
                }
            }
            if let Some(file) = path.file_name().map(|file| file.to_string_lossy()) {
                if file.contains('.') {
                    names.push(file.into_owned());
                }
            }
            let dir_place = place && path.is_dir();
            for name in names {
                let name = folded(&name);
                if dir_place {
                    inside
                        .entry(name.clone())
                        .or_insert_with(|| path.to_path_buf());
                }
                exact.entry(name).or_insert_with(|| path.to_path_buf());
            }
        };
        if !self.flows.is_empty() {
            for place in self.places() {
                for (at, path) in Self::above(&place, &bases).iter().enumerate() {
                    note(path, at == 0);
                }
            }
        }
        for secret in self.secrets() {
            if self.secret_reason(&secret).is_some() && secret.symlink_metadata().is_ok() {
                note(&secret, false);
            }
        }
        let mut found: Vec<PathBuf> = Vec::new();
        let is_path = |c: char| c.is_alphanumeric() || "._-/\\~+@:".contains(c);
        let mut rest = text;
        while let Some(start) = rest.find(is_path) {
            let before = rest[..start].chars().next_back();
            let after_start = &rest[start..];
            let length = after_start
                .find(|c: char| !is_path(c))
                .unwrap_or(after_start.len());
            let word = &after_start[..length];
            let after = after_start[length..].chars().next();
            rest = &after_start[length..];
            let quoted = matches!(before, Some('\'' | '"' | '`')) && before == after;
            if !quoted && !word.contains(['.', '/', '\\']) {
                continue;
            }
            let glued = before.is_some_and(|c| !(c.is_whitespace() || "'\"`([{,;=<>".contains(c)));
            let word = folded(&word.replace('\\', "/"));
            let word = word.trim_end_matches('/');
            let parts: Vec<&str> = word
                .split('/')
                .enumerate()
                .filter(|(at, part)| *at == 0 || *part != ".")
                .map(|(_, part)| part)
                .take(32)
                .collect();
            let parts = match parts.split_first() {
                Some((&".", rest)) if !rest.is_empty() => rest.to_vec(),
                _ => parts,
            };
            let starts = if glued { parts.len() } else { 1 };
            for start in 0..starts {
                if start > 0 && parts[start].is_empty() {
                    continue;
                }
                for end in (start + 1..=parts.len()).rev() {
                    let name = parts[start..end].join("/");
                    let table = if end == parts.len() { &exact } else { &inside };
                    if let Some(path) = table.get(&name) {
                        if !changes && path.is_dir() && self.secret_reason(path).is_none() {
                            continue;
                        }
                        if !found.contains(path) {
                            found.push(path.clone());
                        }
                    }
                }
            }
        }
        found
    }

    /// The rule that guards `path` as it is named, without following links
    /// or looking below it: the name it goes by, from the flow or
    /// repository whose rule it is, and why.
    fn rule(&self, path: &Path) -> Option<(String, String)> {
        // The binary first: it is the binary wherever it is, a flot store
        // included, where a proof built on a copy of the work leaves it,
        // and by whatever name, as macOS's `/var` for `/private/var`.
        if let Some(exe) = self.exe.as_deref() {
            let same = |path: &Path| strip_base(path, exe, FOLDS_CASE).as_deref() == Some("");
            let name = |path: &Path| path.file_name().map(|name| folded(&name.to_string_lossy()));
            if name(path) == name(exe) && (same(path) || same(&resolve(path))) {
                return Some((
                    path.display().to_string(),
                    "it is the flot binary that enforces this guard".to_string(),
                ));
            }
        }
        for flow in &self.flows {
            if let Some(relative) = below(path, &flow.roots) {
                if let Some(reason) = flow.evidence(&relative) {
                    return Some((relative, reason));
                }
            }
        }
        for flow in &self.flows {
            if let Some(relative) = below(path, &flow.tops) {
                if let Some(reason) = harness(&relative) {
                    return Some((relative, reason));
                }
            }
        }
        let relative = below(path, &self.home)?;
        user_harness(&relative).map(|reason| (format!("~/{relative}"), reason))
    }

    /// `path` as the guard names it: as written, or else as it resolves,
    /// since a path can be named through a link the bases are not, as
    /// macOS's `/var` is to `/private/var`.
    fn name(&self, path: &Path) -> String {
        if let Some(named) = self.named_below(path) {
            return named;
        }
        self.named_below(&resolve(path))
            .unwrap_or_else(|| path.display().to_string())
    }

    /// `path` relative to the flow or home directory that holds it.
    fn named_below(&self, path: &Path) -> Option<String> {
        let relative = self
            .flows
            .iter()
            .find_map(|flow| below(path, &flow.roots))
            .or_else(|| self.flows.iter().find_map(|flow| below(path, &flow.tops)));
        Some(match relative {
            Some(relative) if relative.is_empty() => ".".to_string(),
            Some(relative) => relative,
            None => match below(path, &self.home)? {
                relative if relative.is_empty() => "~".to_string(),
                relative => format!("~/{relative}"),
            },
        })
    }

    /// Where guarded files are known to be, as far as they exist: each
    /// flow's file, store and proofs (a pattern by its matches), its
    /// repository's harness, the user's Claude Code settings and plugins,
    /// and flot itself.
    fn places(&self) -> Vec<PathBuf> {
        let mut places = Vec::new();
        for flow in &self.flows {
            let root = &flow.workspace.root;
            places.push(flow.workspace.flow_path.clone());
            places.push(root.join(crate::store::STORE_DIR));
            for (_, pattern, _) in &flow.proofs {
                places.extend(on_disk(root, pattern));
            }
            let top = &flow.tops[0];
            for place in [".git/config", ".git/hooks", ".github/workflows"] {
                places.push(top.join(place));
            }
            for dir in [top, root] {
                places.extend(on_disk(dir, ".claude/settings*.json"));
            }
        }
        if let Some(home) = self.home.first() {
            places.extend(on_disk(home, ".claude/settings*.json"));
            places.push(home.join(".claude/plugins"));
        }
        places.extend(self.exe.clone());
        let mut seen = std::collections::HashSet::new();
        places.retain(|place| place.symlink_metadata().is_ok() && seen.insert(place.clone()));
        places
    }

    /// What the directory `dir` (resolved) holds that is guarded, as a
    /// reason, if anything: what is known to be there, then what a bounded
    /// walk finds that no flow names in advance, such as another flow's
    /// file or Claude Code settings deeper down.
    fn holds(&self, dir: &Path) -> Option<String> {
        let mut held: Vec<(String, String)> = Vec::new();
        let mut note = |found: (String, String)| {
            if !held.iter().any(|(name, _)| *name == found.0) {
                held.push(found);
            }
        };
        for place in self.places() {
            let resolved = resolve(&place);
            if strip_base(&resolved, dir, FOLDS_CASE).is_some() {
                if let Some(found) = self.rule(&place).or_else(|| self.rule(&resolved)) {
                    note(found);
                }
            }
        }
        let mut queue = std::collections::VecDeque::from([dir.to_path_buf()]);
        let mut read = 0;
        'walk: while let Some(next) = queue.pop_front() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                read += 1;
                if read > GUARD_WALK_LIMIT {
                    break 'walk;
                }
                let path = entry.path();
                if let Some(found) = self.rule(&path) {
                    note(found);
                } else if entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && entry.file_name() != ".git"
                {
                    queue.push_back(path);
                }
            }
        }
        let ((name, reason), rest) = held.split_first()?;
        let mut said = format!("it holds {name}: {reason}");
        if !rest.is_empty() {
            let shown: Vec<&str> = rest.iter().take(6).map(|(name, _)| name.as_str()).collect();
            said.push_str(&format!("; also {}", shown.join(", ")));
            if rest.len() > shown.len() {
                said.push_str(&format!(" and {} more", rest.len() - shown.len()));
            }
        }
        Some(said)
    }

    /// The guarded file that `path` (resolved) is another name of, by
    /// device and inode.
    #[cfg(unix)]
    fn hard_link(&self, path: &Path) -> Option<(String, String)> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.nlink() < 2 {
            return None;
        }
        let mut files = Vec::new();
        for place in self.places() {
            if place.is_dir() {
                files.extend(files_below(&place, GUARD_WALK_LIMIT));
            } else {
                files.push(place);
            }
        }
        files.into_iter().find_map(|file| {
            let other = std::fs::metadata(&file).ok()?;
            if (other.dev(), other.ino()) != (meta.dev(), meta.ino()) {
                return None;
            }
            self.rule(&file).or_else(|| self.rule(&resolve(&file)))
        })
    }

    #[cfg(not(unix))]
    fn hard_link(&self, _path: &Path) -> Option<(String, String)> {
        None
    }
}

/// Parts of the names of the calls with which code removes, moves or
/// writes files, as Python's `shutil.rmtree`, Node's `fs.writeFileSync`
/// and `createWriteStream` or Ruby's `FileUtils.rm_rf`.
const CHANGES_FILES: &[&str] = &[
    "rmtree",
    "rmdir",
    "remove",
    "unlink",
    "rename",
    "replace",
    "move",
    "copy",
    "write",
    "truncate",
    "rm_",
    "cp_",
    "rimraf",
    "rmsync",
    "chmod",
    "chown",
    "delete",
    "append",
    "touch",
    "emptydir",
    "outputfile",
    "symlink",
];

/// Whether `code` calls something that removes, moves or writes files: a
/// name in it, whatever its case, that holds one of [`CHANGES_FILES`] or
/// is `rm`, `mv` or `cp`, or a string that opens a file to write, as
/// Python's `'w'` or Perl's `'>'`.
fn changes_files(code: &str) -> bool {
    let named = code
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map(str::to_lowercase)
        .any(|name| {
            matches!(name.as_str(), "rm" | "mv" | "cp")
                || CHANGES_FILES.iter().any(|call| name.contains(call))
        });
    named
        || ['\'', '"'].into_iter().any(|quote| {
            code.split(quote).skip(1).step_by(2).any(|string| {
                let mode = !string.is_empty()
                    && string.len() <= 3
                    && string.chars().all(|c| "rwabxt+".contains(c))
                    && string.contains(['w', 'a', 'x', '+']);
                mode || string.starts_with('>') || string.starts_with("+<")
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_that_changes_files_is_told_by_its_calls() {
        for code in [
            "import shutil; shutil.rmtree(d)",
            "os.remove(p)",
            "Path(p).write_text('')",
            "require('fs').rmSync(d, {recursive: true})",
            "fs.createWriteStream(p)",
            "FileUtils.rm_rf(d)",
            "subprocess.run(['rm', '-rf', d])",
            "open(p, 'w')",
            "open(p, \"ab\")",
            "open(F, '>x')",
            "fs.appendFileSync(p, s)",
        ] {
            assert!(changes_files(code), "{code}");
        }
        for code in [
            "for d in ('scripts', 'tests'): print(sorted(os.listdir(d)))",
            "cache_dir = os.path.join(root, 'cache'); load_dataset(n, cache_dir=cache_dir)",
            "print(open(p).read())",
            "open(p, 'r')",
            "require('fs').readdirSync('.github')",
            "import term",
        ] {
            assert!(!changes_files(code), "{code}");
        }
    }
}
