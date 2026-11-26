//! Confinement for a step that asks for it.
//!
//! A flow can say that a step gets no network and may write only beneath
//! some paths. That is enforced by the kernel, not asked of the agent: on
//! Linux, a new user and network namespace (only an unconfigured loopback
//! inside) and a Landlock ruleset that handles every kind of write, which
//! needs Landlock ABI 3 (Linux 6.2) to include truncation; on
//! macOS, a Seatbelt profile that `sandbox-exec` applies before it runs the
//! step's shell. Reads are not restricted. Where the policy cannot be
//! enforced the step is not run: a step that asked to be confined is never
//! run without it.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

/// A policy made ready in the parent. On Linux the child only has to make
/// two system calls that cannot allocate between `fork` and `exec`; on
/// macOS `sandbox-exec` applies the profile and then runs the step.
pub struct Prepared {
    #[cfg(target_os = "linux")]
    ruleset: Option<std::os::fd::OwnedFd>,
    #[cfg(target_os = "linux")]
    isolate_network: bool,
    #[cfg(target_os = "macos")]
    profile: String,
    pub mechanism: String,
}

/// What applies a Seatbelt profile to the program it runs.
#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("mechanism", &self.mechanism)
            .finish()
    }
}

/// Paths a confined step may always write besides its own: the devices a
/// shell writes to, and `extra`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn always_writable(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = ["/dev/null", "/dev/zero", "/dev/full", "/dev/tty"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    paths.extend(extra.iter().cloned());
    paths
}

/// Prepare `network` and `write` (absolute paths, each of which must exist)
/// for a step. `extra` are paths flot itself hands the step to write: its
/// own scratch directory, with its `$TMPDIR` and `$FLOT_METRICS`.
#[cfg(target_os = "linux")]
pub fn prepare(network: bool, write: Option<&[PathBuf]>, extra: &[PathBuf]) -> Result<Prepared> {
    let mut mechanism = Vec::new();
    let ruleset = match write {
        None => None,
        Some(write) => {
            let mut paths = always_writable(extra);
            paths.extend(write.iter().cloned());
            let (fd, abi) = landlock::ruleset(&paths)?;
            mechanism.push(format!("landlock abi {abi}"));
            Some(fd)
        }
    };
    if !network {
        mechanism.push("network namespace".to_string());
    }
    Ok(Prepared {
        ruleset,
        isolate_network: !network,
        mechanism: mechanism.join(", "),
    })
}

/// Prepare `network` and `write` as a Seatbelt profile: every write is
/// denied but beneath the allowed paths, as the kernel resolves them (so
/// `/tmp` is `/private/tmp`), and without network every network operation
/// is denied, Unix sockets included.
#[cfg(target_os = "macos")]
pub fn prepare(network: bool, write: Option<&[PathBuf]>, extra: &[PathBuf]) -> Result<Prepared> {
    use anyhow::Context;
    anyhow::ensure!(
        Path::new(SANDBOX_EXEC).is_file(),
        "{SANDBOX_EXEC}, which applies the profile, is missing"
    );
    let mut profile = String::from("(version 1)\n(allow default)\n");
    let mut mechanism = Vec::new();
    if let Some(write) = write {
        // A rule added later wins over an earlier one.
        profile.push_str("(deny file-write*)\n(allow file-write*\n  (subpath \"/dev/fd\")");
        for path in always_writable(extra).iter().chain(write) {
            let real = std::fs::canonicalize(path)
                .with_context(|| format!("cannot resolve {}", path.display()))?;
            let kind = if real.is_dir() { "subpath" } else { "literal" };
            profile.push_str(&format!("\n  ({kind} {})", seatbelt_string(&real)?));
        }
        profile.push_str(")\n");
        mechanism.push("seatbelt file writes");
    }
    if !network {
        profile.push_str("(deny network*)\n");
        mechanism.push("seatbelt network");
    }
    Ok(Prepared {
        profile,
        mechanism: mechanism.join(", "),
    })
}

/// `path` as a string in a Seatbelt profile.
#[cfg(target_os = "macos")]
fn seatbelt_string(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("{} is not UTF-8", path.display()))?;
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn prepare(_network: bool, _write: Option<&[PathBuf]>, _extra: &[PathBuf]) -> Result<Prepared> {
    anyhow::bail!(
        "flot confines steps on Linux and macOS only, and this is {}",
        std::env::consts::OS
    )
}

impl Prepared {
    /// A command that runs `program` confined. A step that cannot be
    /// confined fails to start.
    pub fn command(&self, program: &Path) -> Command {
        #[cfg(target_os = "macos")]
        {
            let mut command = Command::new(SANDBOX_EXEC);
            command.arg("-p").arg(&self.profile).arg(program);
            command
        }
        #[cfg(not(target_os = "macos"))]
        {
            let mut command = Command::new(program);
            self.apply(&mut command);
            command
        }
    }

    /// Have `command` confine itself between `fork` and `exec`.
    #[cfg(target_os = "linux")]
    fn apply(&self, command: &mut Command) {
        use std::os::fd::AsRawFd;
        use std::os::unix::process::CommandExt;
        let ruleset = self.ruleset.as_ref().map(AsRawFd::as_raw_fd);
        let isolate_network = self.isolate_network;
        // SAFETY: the closure runs in the forked child and makes only
        // async-signal-safe system calls; it allocates nothing.
        unsafe {
            command.pre_exec(move || {
                if isolate_network && libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNET) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(fd) = ruleset {
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::syscall(landlock::SYS_RESTRICT_SELF, fd, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn apply(&self, _command: &mut Command) {}
}

/// Whether a step without network, and when `write` is set confined to
/// write only beneath `root`, can start here: the policy is prepared and a
/// trivial child is confined with it. Returns the mechanism that would
/// enforce it.
pub fn probe(root: &Path, write: bool) -> Result<String> {
    let root = [root.to_path_buf()];
    let prepared = prepare(false, write.then_some(&root[..]), &[])?;
    let status = prepared
        .command(Path::new("sh"))
        .args(["-c", "exit 0"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "a confined child exited with {status}");
    Ok(prepared.mechanism)
}

/// `write` resolved against `root`; each path must exist.
pub fn resolve(root: &Path, write: &[String]) -> Result<Vec<PathBuf>> {
    write
        .iter()
        .map(|relative| {
            let path = root.join(relative);
            if path.exists() {
                Ok(path)
            } else {
                anyhow::bail!("sandbox write path `{relative}` does not exist")
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod landlock {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    use anyhow::{bail, Context, Result};

    // The same numbers on every architecture Linux added them to at once.
    const SYS_CREATE_RULESET: libc::c_long = 444;
    const SYS_ADD_RULE: libc::c_long = 445;
    pub const SYS_RESTRICT_SELF: libc::c_long = 446;
    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: libc::c_int = 1;

    const WRITE_FILE: u64 = 1 << 1;
    const REMOVE_DIR: u64 = 1 << 4;
    const REMOVE_FILE: u64 = 1 << 5;
    const MAKE_CHAR: u64 = 1 << 6;
    const MAKE_DIR: u64 = 1 << 7;
    const MAKE_REG: u64 = 1 << 8;
    const MAKE_SOCK: u64 = 1 << 9;
    const MAKE_FIFO: u64 = 1 << 10;
    const MAKE_BLOCK: u64 = 1 << 11;
    const MAKE_SYM: u64 = 1 << 12;
    /// ABI 2: linking or renaming a file into another directory.
    const REFER: u64 = 1 << 13;
    /// ABI 3: truncating a file.
    const TRUNCATE: u64 = 1 << 14;
    /// The rights that apply to a file rather than a directory.
    const FILE_RIGHTS: u64 = WRITE_FILE | TRUNCATE;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    /// The writes a ruleset handles on Landlock ABI `abi`: every one there
    /// is. Below ABI 3 the kernel cannot deny truncating a file, so a step
    /// confined there could still empty any file it can open; it is not run.
    pub fn handled_for(abi: i64) -> Result<u64> {
        if abi < 3 {
            bail!("cannot deny truncation on Landlock ABI {abi}; ABI 3 (Linux 6.2) is required");
        }
        Ok(WRITE_FILE
            | REMOVE_DIR
            | REMOVE_FILE
            | MAKE_CHAR
            | MAKE_DIR
            | MAKE_REG
            | MAKE_SOCK
            | MAKE_FIFO
            | MAKE_BLOCK
            | MAKE_SYM
            | REFER
            | TRUNCATE)
    }

    /// A ruleset that handles every write the kernel's Landlock ABI knows,
    /// allowing them beneath `paths`, and that ABI.
    pub fn ruleset(paths: &[PathBuf]) -> Result<(OwnedFd, i64)> {
        // SAFETY: a version query takes no attribute.
        let abi = unsafe {
            libc::syscall(
                SYS_CREATE_RULESET,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        if abi < 1 {
            bail!(
                "this kernel does not enforce Landlock ({})",
                std::io::Error::last_os_error()
            );
        }
        let handled = handled_for(abi)?;
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        // SAFETY: `attr` is a valid ruleset attribute of the size passed.
        let fd = unsafe {
            libc::syscall(
                SYS_CREATE_RULESET,
                &attr as *const RulesetAttr,
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        };
        if fd < 0 {
            bail!(
                "cannot create a Landlock ruleset: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: the kernel returned a new file descriptor we now own.
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        for path in paths {
            let name = CString::new(path.as_os_str().as_bytes())
                .with_context(|| format!("{} cannot be named to the kernel", path.display()))?;
            // SAFETY: `name` is a valid C string.
            let raw = unsafe { libc::open(name.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if raw < 0 {
                bail!(
                    "cannot open {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                );
            }
            // SAFETY: `open` returned a new file descriptor we now own.
            let opened = unsafe { OwnedFd::from_raw_fd(raw) };
            let rights = if path.is_dir() {
                handled
            } else {
                handled & FILE_RIGHTS
            };
            let rule = PathBeneathAttr {
                allowed_access: rights,
                parent_fd: std::os::fd::AsRawFd::as_raw_fd(&opened),
            };
            // SAFETY: `rule` is a valid path-beneath rule for this ruleset.
            let added = unsafe {
                libc::syscall(
                    SYS_ADD_RULE,
                    std::os::fd::AsRawFd::as_raw_fd(&ruleset),
                    RULE_PATH_BENEATH,
                    &rule as *const PathBeneathAttr,
                    0u32,
                )
            };
            if added != 0 {
                bail!(
                    "cannot allow writes beneath {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                );
            }
        }
        Ok((ruleset, abi))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_landlock_abi_that_cannot_deny_truncation_confines_nothing() {
            for abi in [1, 2] {
                let refused = handled_for(abi).unwrap_err().to_string();
                assert_eq!(
                    refused,
                    format!("cannot deny truncation on Landlock ABI {abi}; ABI 3 (Linux 6.2) is required")
                );
            }
            for abi in [3, 4, 7] {
                let handled = handled_for(abi).unwrap();
                assert_eq!(handled & TRUNCATE, TRUNCATE, "ABI {abi}");
                assert_eq!(handled & REFER, REFER, "ABI {abi}");
                assert_eq!(handled & WRITE_FILE, WRITE_FILE, "ABI {abi}");
            }
        }
    }
}
