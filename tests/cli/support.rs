//! The harness every topic drives the flot binary through.

#[path = "support/flows.rs"]
mod flows;
#[path = "support/git.rs"]
mod git;

pub(crate) use flows::{ANCHORED, CANDIDATES, CONFINED, PROVEN, RETRYING, TWO_STEPS};
pub(crate) use git::{anchored_project, commit, git_in};
use std::path::Path;
use std::process::Command;

/// The flot binary under test. Cargo names it by an absolute path of the
/// host that built it; a suite built in WSL and run as a Windows program
/// cannot follow that path, and finds the binary beside its own `deps`.
pub(crate) fn flot_bin() -> std::path::PathBuf {
    let declared = std::path::PathBuf::from(env!("CARGO_BIN_EXE_flot"));
    if declared.exists() {
        return declared;
    }
    let exe = std::env::current_exe().expect("the test binary has a path");
    exe.parent()
        .and_then(std::path::Path::parent)
        .expect("test binaries live in target/<profile>/deps")
        .join(declared.file_name().expect("the binary has a name"))
}

pub(crate) struct Project {
    pub(crate) dir: tempfile::TempDir,
}

pub(crate) struct Output {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl Output {
    pub(crate) fn ok(&self) -> &Self {
        assert!(
            self.code == 0,
            "expected exit 0, got {}\nstdout:\n{}\nstderr:\n{}",
            self.code,
            self.stdout,
            self.stderr
        );
        self
    }

    /// A run of some of the flow's steps, each of which passed: not a pass.
    pub(crate) fn partial(&self) -> &Self {
        assert!(
            self.code == 2,
            "expected exit 2 for a partial run, got {}\nstdout:\n{}\nstderr:\n{}",
            self.code,
            self.stdout,
            self.stderr
        );
        self
    }

    pub(crate) fn has(&self, needle: &str) -> &Self {
        assert!(
            self.stdout.contains(needle) || self.stderr.contains(needle),
            "expected to find {needle:?}\nstdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    pub(crate) fn lacks(&self, needle: &str) -> &Self {
        assert!(
            !(self.stdout.contains(needle) || self.stderr.contains(needle)),
            "did not expect {needle:?}\nstdout:\n{}",
            self.stdout
        );
        self
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).expect("stdout is JSON")
    }
}

impl Project {
    pub(crate) fn new(flow: &str) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("flot.toml"), flow).expect("write flow");
        Project { dir }
    }

    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(crate) fn write(&self, relative: &str, contents: &str) {
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write file");
    }

    /// Where this project's signing key lives. Every invocation is pinned to
    /// it, so no test can read or create a key in the real home directory.
    pub(crate) fn key_path(&self) -> std::path::PathBuf {
        self.path().join(".test-keys").join("signing.key")
    }

    /// This project's store, opened the way its flot opens it.
    pub(crate) fn store(&self) -> flot::store::Store {
        let text = std::fs::read_to_string(self.path().join(".test-keys/host.secret"))
            .expect("flot created the host secret");
        let secret: [u8; 32] = hex::decode(text.trim()).unwrap().try_into().unwrap();
        flot::store::Store::open(self.path(), flot::store::HostSecret::from_bytes(secret)).unwrap()
    }

    pub(crate) fn flot(&self, args: &[&str]) -> Output {
        let output = Command::new(flot_bin())
            .args(args)
            .current_dir(self.path())
            .env("FLOT_SIGNING_KEY", self.key_path())
            .output()
            .expect("flot runs");
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    pub(crate) fn flot_stdin(&self, args: &[&str], stdin: &str) -> Output {
        use std::io::Write;
        let mut child = Command::new(flot_bin())
            .args(args)
            .current_dir(self.path())
            .env("FLOT_SIGNING_KEY", self.key_path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("flot runs");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let output = child.wait_with_output().expect("flot runs");
        Output {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

pub(crate) fn public_key(project: &Project) -> String {
    let out = project.flot(&["key", "init", "--json"]);
    out.ok();
    out.json()["public_key"].as_str().unwrap().to_string()
}

/// The id of a project's newest run.
pub(crate) fn run_id(project: &Project) -> String {
    project.flot(&["runs", "--json"]).json()["runs"][0]["run_id"]
        .as_str()
        .unwrap()
        .to_string()
}

pub(crate) fn read(project: &Project, relative: &str) -> String {
    std::fs::read_to_string(project.path().join(relative)).expect("file exists")
}

pub(crate) fn step_flow(body: &str) -> String {
    format!("[flow]\nname = \"t\"\n\n[[step]]\nid = \"s\"\n{body}\n")
}

pub(crate) fn receipt_file(project: &Project, step: usize) -> std::path::PathBuf {
    let run = project.flot(&["show", "--json"]).json();
    let id = run["steps"][step]["receipt_id"]
        .as_str()
        .unwrap()
        .to_string();
    project
        .path()
        .join(".flot/receipts")
        .join(format!("{}.json", id.trim_start_matches("sha256:")))
}
