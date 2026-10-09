//! Secure publication of the daemon's local discovery endpoint.
//!
//! The endpoint file (`daemon.json`) carries the bearer token that gates every
//! RPC on the localhost socket. It must therefore never be written
//! world-readable, followed through a symlink, or left partially written after
//! a crash. This module centralizes the two operations that protect it:
//!
//! - [`prepare_private_data_dir`] verifies the data directory is safe to hold a
//!   credential and makes it private to the current user.
//! - [`publish_private_endpoint`] writes the endpoint atomically with
//!   owner-only permissions and fails closed on any error.
//!
//! Neither function ever returns the token in an error, so callers may log
//! errors without leaking the credential.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// The name of the JSON file that advertises the running daemon's endpoint.
pub const ENDPOINT_FILE: &str = "daemon.json";

/// Prepare `data_dir` to hold the daemon's bearer-token endpoint.
///
/// Creates the directory when it is missing and otherwise verifies that the
/// final path component is a real directory (not a symlink) owned by the
/// current effective user, then ensures it is private to that user. Returns an
/// error rather than proceeding with an unsafe location, so the daemon must
/// fail closed instead of publishing a credential into an attacker-influenced
/// path.
pub fn prepare_private_data_dir(data_dir: &Path) -> io::Result<()> {
    // Reject a symlink (or any non-directory) at the final component before we
    // do anything that could traverse it.
    match fs::symlink_metadata(data_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "refusing to use data directory '{}': it is a symbolic link",
                        data_dir.display()
                    ),
                ));
            }
            if !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!(
                        "refusing to use data directory '{}': not a directory",
                        data_dir.display()
                    ),
                ));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(data_dir)?;
        }
        Err(e) => return Err(e),
    }

    enforce_private_directory(data_dir)
}

/// Atomically publish `{ "port": ..., "token": ... }` to
/// `<data_dir>/daemon.json`, readable and writable only by the current user.
///
/// The endpoint is written to a temporary file in the same directory, flushed
/// and synced, and then atomically renamed into place. On any failure the
/// temporary file is dropped and the caller is expected to abort startup.
pub fn publish_private_endpoint(data_dir: &Path, port: u16, token: &str) -> io::Result<()> {
    let endpoint = data_dir.join(ENDPOINT_FILE);

    // Reject a pre-existing symlink at the target so a rename can never be
    // redirected onto a victim file. `rename` replaces the symlink itself
    // rather than its target, but refusing early keeps the behavior explicit
    // and testable.
    if let Ok(meta) = fs::symlink_metadata(&endpoint) {
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to publish endpoint '{}': it is a symbolic link",
                    endpoint.display()
                ),
            ));
        }
    }

    let body = serde_json::json!({ "port": port, "token": token }).to_string();

    // Create the temporary file in the same directory so the final rename is
    // atomic and stays on the same filesystem.
    let mut tmp = tempfile::NamedTempFile::new_in(data_dir)?;

    // Enforce owner-only permissions *before* any credential bytes are written.
    enforce_private_file(tmp.path())?;

    tmp.write_all(body.as_bytes())?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;

    // Atomically replace the endpoint. `persist` uses `rename` (or an
    // equivalent atomic replace) rather than truncating in place, so a crash
    // cannot leave a partially written credential and a symlink cannot be
    // followed.
    tmp.persist(&endpoint).map_err(|e| e.error)?;

    Ok(())
}

/// Remove a published endpoint, tolerating its absence. Does not follow
/// symlinks: `remove_file` deletes the link itself, never its target.
pub fn remove_endpoint(data_dir: &Path) -> io::Result<()> {
    let endpoint = data_dir.join(ENDPOINT_FILE);
    match fs::remove_file(&endpoint) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The path of the endpoint file inside `data_dir`.
pub fn endpoint_path(data_dir: &Path) -> PathBuf {
    data_dir.join(ENDPOINT_FILE)
}

#[cfg(unix)]
fn enforce_private_directory(data_dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let meta = fs::metadata(data_dir)?;

    // The directory must be owned by the current effective user; otherwise we
    // would be publishing a credential into a directory someone else controls.
    // `geteuid` is the only place this module touches the OS identity.
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing to use data directory '{}': owned by uid {}, expected {}",
                data_dir.display(),
                meta.uid(),
                euid
            ),
        ));
    }

    // Restrict an existing directory only after the ownership check above has
    // passed, so we never chmod a directory we do not own.
    let mode = meta.mode() & 0o7777;
    if mode & 0o077 != 0 {
        fs::set_permissions(data_dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(windows)]
fn enforce_private_directory(data_dir: &Path) -> io::Result<()> {
    apply_private_acl(data_dir)
}

#[cfg(unix)]
fn enforce_private_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn enforce_private_file(path: &Path) -> io::Result<()> {
    // The parent directory's protected DACL is inherited by newly created
    // files; apply it explicitly too for defense in depth.
    apply_private_acl(path)
}

/// Apply a current-user-private ACL to `path` using an SDDL security descriptor.
///
/// The descriptor grants full control to the owner, SYSTEM, and
/// Administrators, and denies everyone else. The `P` flag marks the DACL as
/// protected so it does not inherit a more permissive ACL from an ancestor.
#[cfg(windows)]
fn apply_private_acl(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::SetFileSecurityW;
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

    const SDDL: &str = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
    let sddl_wide: Vec<u16> = SDDL.encode_utf16().chain(std::iter::once(0)).collect();
    let path_wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut descriptor_size: u32 = 0;
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            &mut descriptor_size,
        )
    };
    if ok == 0 || descriptor.is_null() {
        return Err(io::Error::last_os_error());
    }

    let ok = unsafe { SetFileSecurityW(path_wide.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
    unsafe { LocalFree(descriptor) };

    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_data_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "am-daemon-endpoint-{}-{:?}-{tag}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn prepare_creates_private_directory() {
        let dir = temp_data_dir("create");
        prepare_private_data_dir(&dir).unwrap();
        assert!(dir.is_dir());

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mode = fs::metadata(&dir).unwrap().mode() & 0o7777;
            assert_eq!(mode, 0o700, "data dir must be 0700");
        }
    }

    #[cfg(unix)]
    #[test]
    fn prepare_rejects_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let dir = temp_data_dir("symlink-parent");
        let victim = temp_data_dir("symlink-victim");
        fs::create_dir_all(&victim).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let link = dir.join("link");
        symlink(&victim, &link).unwrap();

        let err = prepare_private_data_dir(&link).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        // The victim directory must be untouched.
        assert!(!link.join(ENDPOINT_FILE).exists());
    }

    #[test]
    fn publish_writes_private_atomic_endpoint() {
        let dir = temp_data_dir("publish");
        prepare_private_data_dir(&dir).unwrap();

        publish_private_endpoint(&dir, 1234, "secret-token").unwrap();

        let endpoint = endpoint_path(&dir);
        let raw = fs::read_to_string(&endpoint).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["port"], 1234);
        assert_eq!(parsed["token"], "secret-token");

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let mode = fs::metadata(&endpoint).unwrap().mode() & 0o7777;
            assert_eq!(mode, 0o600, "endpoint file must be 0600");
        }
    }

    #[cfg(unix)]
    #[test]
    fn publish_refuses_symlinked_endpoint() {
        use std::os::unix::fs::symlink;

        let dir = temp_data_dir("publish-symlink");
        prepare_private_data_dir(&dir).unwrap();
        let victim = temp_data_dir("publish-victim");
        fs::create_dir_all(&victim).unwrap();
        let victim_file = victim.join("victim.txt");
        fs::write(&victim_file, "do not touch").unwrap();

        let endpoint = endpoint_path(&dir);
        symlink(&victim_file, &endpoint).unwrap();

        let err = publish_private_endpoint(&dir, 1234, "secret-token").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&victim_file).unwrap(), "do not touch");
    }

    #[test]
    fn remove_endpoint_tolerates_missing() {
        let dir = temp_data_dir("remove-missing");
        prepare_private_data_dir(&dir).unwrap();
        remove_endpoint(&dir).unwrap();
    }
}
