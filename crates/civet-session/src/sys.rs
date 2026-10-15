//! Tiny unsafe surface: signalling process groups.

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
