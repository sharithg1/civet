//! Tiny unsafe surface: signalling process groups, dup/poll of the PTY fd,
//! the path of an open file.

use civet_proto::KillSignal;

/// Map a protocol signal to the libc signal number.
pub(crate) fn signal_number(signal: KillSignal) -> libc::c_int {
    match signal {
        KillSignal::Hangup => libc::SIGHUP,
        KillSignal::Terminate => libc::SIGTERM,
        KillSignal::Kill => libc::SIGKILL,
    }
}

/// Send `sig` to process group `pgid`. Errors (e.g. the group is already
/// gone) are returned as `io::Error` for logging.
#[allow(unsafe_code)]
pub(crate) fn killpg(pgid: i32, sig: libc::c_int) -> std::io::Result<()> {
    if pgid <= 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to signal process group <= 1",
        ));
    }
    // SAFETY: killpg has no memory-safety preconditions; it only takes plain
    // integers. pgid > 1 is checked above so we never signal init or our own
    // group by accident (0 would mean "our process group").
    let rc = unsafe { libc::killpg(pgid, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Duplicate `fd` (close-on-exec) into an owned descriptor.
#[allow(unsafe_code)]
pub(crate) fn dup_fd(fd: std::os::fd::RawFd) -> std::io::Result<std::os::fd::OwnedFd> {
    // SAFETY: `fd` is the PTY master's descriptor, owned by the `MasterPty`
    // that the caller holds for the duration of this call, so it is open.
    // The borrow does not outlive this expression; `try_clone_to_owned`
    // dups it (F_DUPFD_CLOEXEC) into a new descriptor we own.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
    borrowed.try_clone_to_owned()
}

/// True if `fd` has data to read right now (`poll` with a zero timeout;
/// never blocks). Errors count as "not readable".
#[allow(unsafe_code)]
pub(crate) fn poll_readable(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    use std::os::fd::AsRawFd;
    let mut pfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is a valid, initialized pollfd that lives for the call;
    // nfds = 1 matches it. `fd` is borrowed, so it stays open meanwhile.
    let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
    rc > 0 && pfd.revents & libc::POLLIN != 0
}

/// Where an open file actually is (`/proc/self/fd/N` on Linux, `F_GETPATH`
/// on macOS): its canonical path, whatever path was used to open it.
#[cfg(target_os = "linux")]
pub(crate) fn fd_path(file: &std::fs::File) -> std::io::Result<std::path::PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

/// Where an open file actually is (`/proc/self/fd/N` on Linux, `F_GETPATH`
/// on macOS): its canonical path, whatever path was used to open it.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(crate) fn fd_path(file: &std::fs::File) -> std::io::Result<std::path::PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes (NUL included) into
    // `buf`, which is that long and lives for the call; the fd is open
    // because `file` is borrowed.
    let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if rc == -1 {
        return Err(std::io::Error::last_os_error());
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(len);
    Ok(std::ffi::OsString::from_vec(buf).into())
}

/// Where an open file actually is: not supported on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn fd_path(_file: &std::fs::File) -> std::io::Result<std::path::PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "the path of an open file is not available on this platform",
    ))
}
