//! Privilege dropping and PID file management.

use std::fs;
use std::io;
use std::path::Path;
use tracing::{info, warn};

/// Errors from privilege / PID file operations.
#[derive(Debug, thiserror::Error)]
pub enum PrivilegeError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("PID file {path} exists and process {pid} is still running")]
    PidFileStale { path: String, pid: u32 },
    #[error("privilege drop failed: {0}")]
    DropFailed(String),
}

// ---------------------------------------------------------------------------
// PID file management
// ---------------------------------------------------------------------------

/// Create a PID file at `path` containing the current process PID.
///
/// If a PID file already exists, checks if the referenced process is still
/// running. If it is, returns an error. If it is stale, removes it first.
pub fn create_pid_file(path: &str) -> Result<(), PrivilegeError> {
    let pid_path = Path::new(path);

    if pid_path.exists() {
        // Read existing PID and check if still running.
        if let Ok(content) = fs::read_to_string(pid_path) {
            if let Ok(old_pid) = content.trim().parse::<u32>() {
                if is_process_running(old_pid) {
                    return Err(PrivilegeError::PidFileStale {
                        path: path.to_string(),
                        pid: old_pid,
                    });
                }
            }
        }
        // Stale PID file, remove it.
        info!("removing stale PID file: {}", path);
        fs::remove_file(pid_path)?;
    }

    // Ensure parent directory exists.
    if let Some(parent) = pid_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    let pid = std::process::id();
    fs::write(pid_path, format!("{}\n", pid))?;
    info!("created PID file: {} (pid={})", path, pid);
    Ok(())
}

/// Remove the PID file if it exists and contains our PID.
pub fn remove_pid_file(path: &str) {
    let pid_path = Path::new(path);
    if pid_path.exists() {
        // Only remove if it's our PID.
        if let Ok(content) = fs::read_to_string(pid_path) {
            if let Ok(file_pid) = content.trim().parse::<u32>() {
                if file_pid == std::process::id() {
                    if let Err(e) = fs::remove_file(pid_path) {
                        warn!("failed to remove PID file {}: {}", path, e);
                    } else {
                        info!("removed PID file: {}", path);
                    }
                    return;
                }
            }
        }
        warn!("PID file {} does not belong to us, leaving it", path);
    }
}

/// Check whether a process is running. On Unix, we send signal 0 via nix.
fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal;
        use nix::unistd::Pid;
        // Sending `None` as signal is equivalent to kill(pid, 0) — it checks
        // whether we have permission to send a signal to the process, which
        // tells us whether the process exists.
        signal::kill(Pid::from_raw(pid as i32), None).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

// ---------------------------------------------------------------------------
// Privilege dropping
// ---------------------------------------------------------------------------

/// Drop privileges to the specified user and group.
///
/// This must be called after binding sockets but before entering the main loop.
/// If the user/group does not exist, logs a warning and continues.
#[cfg(target_os = "linux")]
pub fn drop_privileges(user: &str, group: &str) -> Result<(), PrivilegeError> {
    use nix::unistd::{setgid, setuid, Gid, Uid};

    // Look up group first (we need to do this before dropping root).
    match nix::unistd::Group::from_name(group) {
        Ok(Some(grp)) => {
            setgid(grp.gid)
                .map_err(|e| PrivilegeError::DropFailed(format!("setgid({}): {}", grp.gid, e)))?;
            info!("set gid to {} ({})", group, grp.gid);
        }
        Ok(None) => {
            warn!("group '{}' not found, skipping setgid", group);
        }
        Err(e) => {
            warn!(
                "failed to look up group '{}': {}, skipping setgid",
                group, e
            );
        }
    }

    match nix::unistd::User::from_name(user) {
        Ok(Some(usr)) => {
            setuid(usr.uid)
                .map_err(|e| PrivilegeError::DropFailed(format!("setuid({}): {}", usr.uid, e)))?;
            info!("set uid to {} ({})", user, usr.uid);
        }
        Ok(None) => {
            warn!("user '{}' not found, skipping setuid", user);
        }
        Err(e) => {
            warn!("failed to look up user '{}': {}, skipping setuid", user, e);
        }
    }

    Ok(())
}

/// On non-Linux platforms, privilege dropping is a no-op with a warning.
#[cfg(not(target_os = "linux"))]
pub fn drop_privileges(user: &str, group: &str) -> Result<(), PrivilegeError> {
    warn!(
        "privilege dropping is only supported on Linux (requested user={}, group={})",
        user, group
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_create_and_remove_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("test.pid");
        let path_str = pid_path.to_string_lossy().to_string();

        // Create
        create_pid_file(&path_str).unwrap();
        assert!(pid_path.exists());

        let content = fs::read_to_string(&pid_path).unwrap();
        let file_pid: u32 = content.trim().parse().unwrap();
        assert_eq!(file_pid, std::process::id());

        // Remove
        remove_pid_file(&path_str);
        assert!(!pid_path.exists());
    }

    #[test]
    fn test_stale_pid_file_removed() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("stale.pid");
        let path_str = pid_path.to_string_lossy().to_string();

        // Write a PID that almost certainly doesn't exist.
        fs::write(&pid_path, "999999999\n").unwrap();

        // Should succeed because the process doesn't exist.
        create_pid_file(&path_str).unwrap();
        let content = fs::read_to_string(&pid_path).unwrap();
        let file_pid: u32 = content.trim().parse().unwrap();
        assert_eq!(file_pid, std::process::id());
    }

    #[test]
    fn test_drop_privileges_nonexistent_user() {
        // Should warn but not error.
        let result = drop_privileges("nonexistent_user_xyz", "nonexistent_group_xyz");
        assert!(result.is_ok());
    }
}
