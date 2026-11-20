//! The app's own log file, `<state>/logs/civet-desktop.log`.
//!
//! An app launched from Finder has stderr on `/dev/null`, so without a file
//! nothing it logs (link stalls, failovers, reconnects) can be read
//! afterwards. The file is size-capped: when it would grow past
//! [`MAX_LOG_BYTES`] it is renamed to `civet-desktop.log.1` (replacing the
//! previous one) and started again, so the two together never take more
//! than twice that.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// File name under `<state>/logs/`.
pub const LOG_FILE: &str = "civet-desktop.log";

/// Size at which the log is rotated.
pub const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;

/// Append-only writer that rotates the file at a size cap.
#[derive(Debug)]
pub struct CappedLog {
    path: PathBuf,
    max: u64,
    file: File,
    written: u64,
}

impl CappedLog {
    /// Open (or create) `<state_dir>/logs/civet-desktop.log` for appending.
    /// The `logs` directory is created private (0700), like the daemon's.
    pub fn open(state_dir: &Path, max: u64) -> io::Result<Self> {
        let dir = civetd::paths::StatePaths::new(state_dir).logs_dir();
        civetd::paths::create_private_dir(&dir)?;
        let path = dir.join(LOG_FILE);
        let file = open_append(&path)?;
        let written = file.metadata()?.len();
        Ok(Self {
            path,
            max,
            file,
            written,
        })
    }

    /// Where the log is written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn rotate(&mut self) -> io::Result<()> {
        let mut old = self.path.clone().into_os_string();
        old.push(".1");
        std::fs::rename(&self.path, old)?;
        self.file = open_append(&self.path)?;
        self.written = 0;
        Ok(())
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl Write for CappedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written + buf.len() as u64 > self.max {
            // If rotating fails (e.g. the directory went away), keep
            // appending: an oversized log beats none.
            let _ = self.rotate();
        }
        self.file.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_rotates_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = CappedLog::open(dir.path(), 100).unwrap();
        let path = log.path().to_path_buf();
        assert_eq!(path, dir.path().join("logs").join(LOG_FILE));
        let rotated = dir.path().join("logs").join("civet-desktop.log.1");

        let line = [b'a'; 40];
        log.write_all(&line).unwrap();
        log.write_all(&line).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 80);
        assert!(!rotated.exists());

        // The third line would cross the cap: the first two move aside.
        log.write_all(&[b'b'; 40]).unwrap();
        assert_eq!(std::fs::read(&rotated).unwrap(), [b'a'; 80]);
        assert_eq!(std::fs::read(&path).unwrap(), [b'b'; 40]);

        // Reopening continues the file and keeps counting from its size.
        drop(log);
        let mut log = CappedLog::open(dir.path(), 100).unwrap();
        log.write_all(&[b'c'; 40]).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 80);
        log.write_all(&[b'd'; 40]).unwrap();
        assert_eq!(std::fs::metadata(&rotated).unwrap().len(), 80);
        assert_eq!(std::fs::read(&path).unwrap(), [b'd'; 40]);

        // A single write larger than the cap is still written whole.
        let mut log = CappedLog::open(dir.path(), 10).unwrap();
        log.write_all(&[b'e'; 40]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [b'e'; 40]);
    }
}
