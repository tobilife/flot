//! The Landlock system calls: a ruleset that allows writes only beneath
//! given paths, for a step to restrict itself with.

use std::ffi::CString;
use std::ops::BitOr;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};

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
const FILE_RIGHTS: [u64; 2] = [WRITE_FILE, TRUNCATE];
/// Every write right there is, through ABI 3.
const WRITES: [u64; 12] = [
    WRITE_FILE,
    REMOVE_DIR,
    REMOVE_FILE,
    MAKE_CHAR,
    MAKE_DIR,
    MAKE_REG,
    MAKE_SOCK,
    MAKE_FIFO,
    MAKE_BLOCK,
    MAKE_SYM,
    REFER,
    TRUNCATE,
];

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
    Ok(all(&WRITES))
}

/// The flags or rights in `bits`, together.
fn all<T: Copy + Default + BitOr<Output = T>>(bits: &[T]) -> T {
    bits.iter().copied().fold(T::default(), BitOr::bitor)
}

/// What a system call returned, or the error it set by returning less
/// than zero.
fn checked(returned: libc::c_long) -> std::io::Result<libc::c_long> {
    if returned < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(returned)
    }
}

/// A ruleset that handles every write the kernel's Landlock ABI knows,
/// allowing them beneath `paths`, and that ABI.
pub fn ruleset(paths: &[PathBuf]) -> Result<(OwnedFd, i64)> {
    // SAFETY: a version query takes no attribute.
    let abi = checked(unsafe {
        libc::syscall(
            SYS_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            CREATE_RULESET_VERSION,
        )
    })
    .map_err(|error| anyhow!("this kernel does not enforce Landlock ({error})"))?;
    let handled = handled_for(abi)?;
    let attr = RulesetAttr {
        handled_access_fs: handled,
    };
    // SAFETY: `attr` is a valid ruleset attribute of the size passed.
    let fd = checked(unsafe {
        libc::syscall(
            SYS_CREATE_RULESET,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0u32,
        )
    })
    .map_err(|error| anyhow!("cannot create a Landlock ruleset: {error}"))?;
    // SAFETY: the kernel returned a new file descriptor we now own.
    let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    for path in paths {
        let name = CString::new(path.as_os_str().as_bytes())
            .with_context(|| format!("{} cannot be named to the kernel", path.display()))?;
        // SAFETY: `name` is a valid C string.
        let raw = checked(
            unsafe { libc::open(name.as_ptr(), all(&[libc::O_PATH, libc::O_CLOEXEC])) }.into(),
        )
        .map_err(|error| anyhow!("cannot open {}: {error}", path.display()))?;
        // SAFETY: `open` returned a new file descriptor we now own.
        let opened = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let rights = if path.is_dir() {
            handled
        } else {
            handled & all(&FILE_RIGHTS)
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
                format!(
                    "cannot deny truncation on Landlock ABI {abi}; ABI 3 (Linux 6.2) is required"
                )
            );
        }
        for abi in [3, 4, 7] {
            let handled = handled_for(abi).unwrap();
            assert_eq!(handled & TRUNCATE, TRUNCATE, "ABI {abi}");
            assert_eq!(handled & REFER, REFER, "ABI {abi}");
            assert_eq!(handled & WRITE_FILE, WRITE_FILE, "ABI {abi}");
        }
    }

    /// The kernel's own numbers: LANDLOCK_ACCESS_FS_WRITE_FILE is bit 1,
    /// REMOVE_DIR through TRUNCATE bits 4 to 14.
    #[test]
    fn the_rights_are_the_kernels_bits() {
        assert_eq!(handled_for(3).unwrap(), 0x7ff2);
        assert_eq!(all(&FILE_RIGHTS), 0x4002);
        assert_eq!(all::<u64>(&[]), 0);
    }

    #[test]
    fn a_call_returning_less_than_zero_failed() {
        assert!(checked(-1).is_err());
        assert_eq!(checked(0).unwrap(), 0);
        assert_eq!(checked(3).unwrap(), 3);
    }

    #[test]
    fn a_write_beneath_a_missing_path_cannot_be_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("gone");
        let refused = ruleset(std::slice::from_ref(&missing))
            .unwrap_err()
            .to_string();
        // A host without Landlock ABI 3 refuses before any path.
        if ruleset(&[dir.path().to_path_buf()]).is_ok() {
            assert_eq!(
                refused,
                format!(
                    "cannot open {}: No such file or directory (os error 2)",
                    missing.display()
                )
            );
        }
    }
}
