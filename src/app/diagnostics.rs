//! Opt-in, bounded event logging on the UI thread. No workers, timers, or
//! filesystem access are introduced until the caller explicitly initializes it.

use std::{
    cell::RefCell,
    fmt,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const MAX_LOG_BYTES: u64 = 1024 * 1024;
// More than the maximum possible timestamp, process ID, elapsed time and text
// of the final marker. Reserving this space keeps the marker within the cap.
const LIMIT_MARKER_RESERVE: u64 = 256;

thread_local! {
    static LOG: RefCell<Option<DiagnosticLog>> = const { RefCell::new(None) };
}

/// Enable logging for this UI thread. Existing content is retained and counts
/// toward the capacity. A failed initialization leaves any previous log intact.
pub fn init(path: &Path) -> io::Result<()> {
    let log = DiagnosticLog::open(path, MAX_LOG_BYTES)?;
    LOG.with(|slot| {
        let mut slot = slot
            .try_borrow_mut()
            .map_err(|_| io::Error::other("diagnostic log is currently in use"))?;
        *slot = Some(log);
        Ok(())
    })
}

pub fn enabled() -> bool {
    LOG.with(|slot| {
        slot.try_borrow()
            .ok()
            .and_then(|log| log.as_ref().map(DiagnosticLog::enabled))
            .unwrap_or(false)
    })
}

/// Record a discrete application event after returning from the window callback.
/// Disabled logging does not format the arguments or access the filesystem.
pub fn event(message: fmt::Arguments<'_>) {
    LOG.with(|slot| {
        // Unexpected recursive logging must not turn diagnostics into a panic.
        let Ok(mut slot) = slot.try_borrow_mut() else {
            return;
        };
        if let Some(log) = slot.as_mut()
            && let Err(error) = log.record(message)
        {
            // record disables itself before returning an error, so subsequent
            // events cannot repeatedly print an error or retry a broken file.
            let _ = writeln!(
                io::stderr().lock(),
                "color-picker: diagnostic logging stopped: {error}"
            );
        }
    });
}

struct DiagnosticLog {
    file: Option<File>,
    bytes_written: u64,
    maximum_bytes: u64,
    started: Instant,
}

impl DiagnosticLog {
    fn open(path: &Path, maximum_bytes: u64) -> io::Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ: allow tail/readers, but no second writer or
            // deletion while this logger owns its cached byte budget.
            options.share_mode(1);
        }
        let file = options.open(path)?;
        #[cfg(not(windows))]
        file.try_lock().map_err(|error| {
            io::Error::other(format!(
                "diagnostic log '{}' is already in use or could not be locked: {error}",
                path.display(),
            ))
        })?;
        let bytes_written = file.metadata()?.len();
        if maximum_bytes <= LIMIT_MARKER_RESERVE
            || bytes_written > maximum_bytes - LIMIT_MARKER_RESERVE
        {
            return Err(io::Error::other(format!(
                "diagnostic log '{}' has no usable capacity remaining ({} bytes; limit {} bytes, including final marker); choose a new log path",
                path.display(),
                bytes_written,
                maximum_bytes,
            )));
        }
        Ok(Self {
            file: Some(file),
            bytes_written,
            maximum_bytes,
            started: Instant::now(),
        })
    }

    fn enabled(&self) -> bool {
        self.file.is_some()
    }

    fn record(&mut self, message: fmt::Arguments<'_>) -> io::Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        let available = self.maximum_bytes - self.bytes_written - LIMIT_MARKER_RESERVE;
        // Format and sanitize directly into a bounded buffer. A huge message
        // or formatting width stops as soon as it cannot fit; it is never
        // expanded into an unbounded intermediate String.
        let mut record = SanitizedRecord {
            line: self.prefix(),
            maximum_bytes: available.saturating_sub(1) as usize,
        };
        let reached_limit =
            record.line.len() > record.maximum_bytes || fmt::write(&mut record, message).is_err();
        let line = if reached_limit {
            format!(
                "{}log.limit_reached maximum_bytes={}\n",
                self.prefix(),
                self.maximum_bytes,
            )
        } else {
            record.line.push('\n');
            record.line
        };

        // The file is present because the disabled case returned above. Avoid
        // unwrap so logging cleanup never creates a new panic path.
        if let Some(file) = self.file.as_mut() {
            if let Err(error) = file.write_all(line.as_bytes()) {
                self.file = None;
                return Err(error);
            }
            self.bytes_written += line.len() as u64;
        }
        if reached_limit {
            self.file = None;
        }
        Ok(())
    }

    fn prefix(&self) -> String {
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!(
            "unix_ms={unix_ms} pid={} elapsed_ms={} ",
            std::process::id(),
            self.started.elapsed().as_millis(),
        )
    }
}

struct SanitizedRecord {
    line: String,
    maximum_bytes: usize,
}

impl SanitizedRecord {
    fn push(&mut self, character: char) -> fmt::Result {
        if self.line.len() + character.len_utf8() > self.maximum_bytes {
            return Err(fmt::Error);
        }
        self.line.push(character);
        Ok(())
    }
}

impl fmt::Write for SanitizedRecord {
    fn write_str(&mut self, message: &str) -> fmt::Result {
        for character in message.chars() {
            if character == '\\'
                || character.is_control()
                || matches!(character, '\u{2028}' | '\u{2029}')
            {
                for escaped in character.escape_default() {
                    self.push(escaped)?;
                }
            } else {
                self.push(character)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            for _ in 0..100 {
                let serial = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let time = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!(
                    "color-picker-diagnostics-test-{}-{time}-{serial}",
                    std::process::id(),
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("could not create isolated test directory: {error}"),
                }
            }
            panic!("could not allocate a unique isolated test directory");
        }

        fn log_path(&self) -> PathBuf {
            self.0.join("nested").join("diagnostics.log")
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            // This exact path was created by this test with create_dir (never
            // adopted from an existing directory). No user paths are removed.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn log_appends_without_overwriting_existing_content() {
        let directory = TempDirectory::new();
        let path = directory.log_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "existing content\n").unwrap();
        let mut log = DiagnosticLog::open(&path, 1024).unwrap();
        assert_eq!(log.bytes_written, 17);
        log.record(format_args!("host.ready 中文")).unwrap();
        drop(log);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("existing content\n"));
        assert!(text.contains("unix_ms="));
        assert!(text.contains(&format!("pid={} ", std::process::id())));
        assert!(text.contains("elapsed_ms="));
        assert!(text.ends_with("host.ready 中文\n"));
    }

    #[test]
    fn capacity_includes_one_final_marker_and_stops_future_writes() {
        let directory = TempDirectory::new();
        let path = directory.log_path();
        let mut log = DiagnosticLog::open(&path, 640).unwrap();
        log.record(format_args!("host.started")).unwrap();
        log.record(format_args!("{}", "x".repeat(640))).unwrap();
        assert!(!log.enabled());
        let terminal_length = std::fs::metadata(&path).unwrap().len();
        for _ in 0..10 {
            log.record(format_args!("must not be written")).unwrap();
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), terminal_length);
        assert!(terminal_length <= 640);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("log.limit_reached").count(), 1);
        assert!(text.lines().last().unwrap().contains("log.limit_reached"));
        assert!(!text.contains("must not be written"));
    }

    #[test]
    fn existing_content_counts_toward_the_limit() {
        let directory = TempDirectory::new();
        let path = directory.log_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "x".repeat(380)).unwrap();
        let mut log = DiagnosticLog::open(&path, 640).unwrap();
        log.record(format_args!("event")).unwrap();
        assert!(!log.enabled());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(&"x".repeat(380)));
        assert!(text.contains("log.limit_reached"));
        assert!(text.len() <= 640);
    }

    #[test]
    fn enormous_format_width_stops_at_the_byte_budget() {
        let directory = TempDirectory::new();
        let path = directory.log_path();
        let mut log = DiagnosticLog::open(&path, 640).unwrap();
        log.record(format_args!("{:>width$}", "x", width = 65_000))
            .unwrap();
        assert!(!log.enabled());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("log.limit_reached"));
        assert!(text.len() <= 640);
    }

    #[test]
    fn second_writer_is_rejected_but_readers_and_later_append_work() {
        let directory = TempDirectory::new();
        let path = directory.log_path();
        let mut first = DiagnosticLog::open(&path, 1024).unwrap();
        first.record(format_args!("first.writer")).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(DiagnosticLog::open(&path, 1024).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        drop(first);
        let mut second = DiagnosticLog::open(&path, 1024).unwrap();
        second.record(format_args!("second.writer")).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(&before));
        assert!(after.ends_with("second.writer\n"));
    }

    #[test]
    fn already_full_log_is_rejected_without_modification() {
        let directory = TempDirectory::new();
        let path = directory.log_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "x".repeat(640);
        std::fs::write(&path, &original).unwrap();
        let error = DiagnosticLog::open(&path, 640).err().unwrap();
        assert!(error.to_string().contains("choose a new log path"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn embedded_newlines_and_controls_cannot_forge_log_lines() {
        let directory = TempDirectory::new();
        let path = directory.log_path();
        let mut log = DiagnosticLog::open(&path, 1024).unwrap();
        log.record(format_args!("title\r\nunix_ms=forged\t\0\u{2028}\\text"))
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("title\\r\\nunix_ms=forged\\t\\u{0}\\u{2028}\\\\text"));
        assert!(!text.contains('\0'));
        assert!(!text.contains('\r'));
    }

    #[test]
    fn write_failure_disables_the_log_without_retrying() {
        let directory = TempDirectory::new();
        let path = directory.log_path();
        let mut log = DiagnosticLog::open(&path, 1024).unwrap();
        log.file = Some(File::open(&path).unwrap());
        assert!(
            log.record(format_args!("cannot write read-only handle"))
                .is_err()
        );
        assert!(!log.enabled());
        assert!(log.record(format_args!("ignored after failure")).is_ok());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn disabled_logger_does_not_format_arguments() {
        struct MustNotFormat;
        impl fmt::Display for MustNotFormat {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("disabled logging must not format its message");
            }
        }
        LOG.with(|slot| *slot.borrow_mut() = None);
        assert!(!enabled());
        event(format_args!("{MustNotFormat}"));
    }
}
