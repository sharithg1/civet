//! The daemon's tiny unsafe surface: flock, process liveness/signals,
//! daemonizing (fork + setsid + stdio to /dev/null + close inherited fds),
//! and the host name. Everything else is safe Rust.

#![allow(unsafe_code)]

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;

/// An exclusive `flock` on a file; released when dropped (the fd closes).
#[derive(Debug)]
pub struct LockFile {
    _file: File,
}

/// Outcome of [`try_lock`].
#[derive(Debug)]
pub enum LockAttempt {
    /// We hold the lock now.
    Acquired(LockFile),
    /// Another open file description (another daemon) holds it.
    Held,
}

/// Try to take an exclusive, non-blocking `flock` on `path` (created 0600
/// if missing).
pub fn try_lock(path: &Path) -> io::Result<LockAttempt> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    // SAFETY: `file` is an open descriptor owned by us for the duration of
    // the call; flock has no memory-safety preconditions.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(LockAttempt::Acquired(LockFile { _file: file }));
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(LockAttempt::Held)
    } else {
        Err(err)
    }
}

/// True if a lock on `path` is currently held by someone (probes by taking
/// and immediately releasing it). A missing file means "not held".
pub fn lock_is_held(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    matches!(try_lock(path), Ok(LockAttempt::Held))
}

/// True if process `pid` exists (EPERM counts as alive).
pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 only performs the permission/existence
    // check; plain integer arguments.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Send `sig` to process `pid` (> 1 only).
pub fn signal(pid: u32, sig: libc::c_int) -> io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|p| *p > 1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "refusing to signal pid"))?;
    // SAFETY: kill has no memory-safety preconditions; pid > 1 is checked so
    // we never signal init or a process group by accident.
    let rc = unsafe { libc::kill(pid, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Detach from the caller: fork (the parent exits 0 right away), start a
/// new session (no controlling terminal; SIGHUP from the SSH session's
/// teardown cannot reach us), point stdin/stdout/stderr at /dev/null and
/// close every other inherited descriptor so nothing keeps the caller's
/// pipes (e.g. a non-interactive `ssh host civetd bootstrap`) open.
///
/// Must be called while the process is still single-threaded (before the
/// tokio runtime or any other thread starts).
pub fn daemonize() -> io::Result<()> {
    // SAFETY: called from `main` before any other thread exists, so the
    // child is a faithful copy and may continue running Rust code.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid > 0 {
        // Parent: the daemon lives on in the child.
        std::process::exit(0);
    }
    // SAFETY: no preconditions; fails only if we already lead a group, which
    // a freshly forked child never does.
    if unsafe { libc::setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }
    let devnull = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    let null_fd = devnull.as_raw_fd();
    for target in 0..=2 {
        // SAFETY: both descriptors are valid; dup2 atomically replaces the
        // standard descriptors, which std does not hold owned handles to.
        if unsafe { libc::dup2(null_fd, target) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    drop(devnull);
    // Do not pin whatever directory the caller was in (callers pass an
    // absolute state dir).
    std::env::set_current_dir("/")?;
    // SAFETY: sysconf has no preconditions.
    let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    let max = if max <= 0 { 1024 } else { max.min(65_536) } as libc::c_int;
    for fd in 3..max {
        // SAFETY: we are single-threaded and hold no owned descriptors yet
        // (devnull was dropped above), so closing inherited ones cannot
        // invalidate a live Rust handle. EBADF for unused numbers is fine.
        unsafe {
            libc::close(fd);
        }
    }
    Ok(())
}

/// The host name (best effort).
pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for `buf.len()` bytes; gethostname writes
    // at most that many and we NUL-terminate defensively below.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) };
    if rc != 0 {
        return "unknown".into();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_excludes_second_holder() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.lock");
        assert!(!lock_is_held(&p));
        let first = match try_lock(&p).unwrap() {
            LockAttempt::Acquired(l) => l,
            LockAttempt::Held => panic!("fresh lock held"),
        };
        assert!(matches!(try_lock(&p).unwrap(), LockAttempt::Held));
        assert!(lock_is_held(&p));
        drop(first);
        // A child being spawned concurrently by another test can hold a
        // copy of the descriptor until its exec closes it (O_CLOEXEC).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while lock_is_held(&p) {
            assert!(std::time::Instant::now() < deadline, "lock not released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn liveness() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0));
        assert!(!hostname().is_empty());
    }
}
