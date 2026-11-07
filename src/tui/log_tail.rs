// Byte-offset ring-buffer tail of a log file.
//
// Reads only newly-appended bytes on each poll, keeping a bounded VecDeque
// of the last N complete lines. Handles file truncation/rotation (restarts
// from the end of the new file). Handles partial lines (bytes without a
// trailing newline) by stashing them and prepending on the next poll.
//
// Never reads more than SEED_BYTES at once, so a large log file (they are
// not rotated) costs the same to open as a small one.

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result};

// How much of the end of the file to read when seeding, or when more than
// this much was appended since the last poll. 5000 lines of debug-level
// cloudflared output fit comfortably.
const SEED_BYTES: u64 = 2 * 1024 * 1024;

pub struct LogTail {
    // None for placeholder tails that only hold fixed lines (demo mode,
    // ephemeral and external tunnels)
    path: Option<PathBuf>,
    offset: u64,
    seeded: bool,
    buffer: VecDeque<String>,
    partial: String,
    max_lines: usize,
    // Last read error, shown in the log pane when there are no lines
    pub error: Option<String>,
}

impl LogTail {
    pub fn new(path: PathBuf, max_lines: usize) -> Self {
        Self {
            path: Some(path),
            offset: 0,
            seeded: false,
            buffer: VecDeque::with_capacity(max_lines.min(1024)),
            partial: String::new(),
            max_lines,
            error: None,
        }
    }

    // A tail with fixed content and no file behind it
    pub fn placeholder(lines: Vec<String>) -> Self {
        Self {
            path: None,
            offset: 0,
            seeded: true,
            buffer: lines.into(),
            partial: String::new(),
            max_lines: usize::MAX,
            error: None,
        }
    }

    pub fn is_file_tail_for(&self, path: &std::path::Path) -> bool {
        self.path.as_deref() == Some(path)
    }

    // Seed on first use, poll after that. Errors are kept in `self.error`
    // instead of being returned. Returns the number of new lines appended.
    pub fn refresh(&mut self) -> usize {
        let result = if self.seeded {
            self.poll()
        } else {
            self.seed().map(|_| 0)
        };
        match result {
            Ok(n) => {
                self.error = None;
                n
            }
            Err(e) => {
                self.error = Some(format!("{:#}", e));
                0
            }
        }
    }

    // Seed the buffer with the last `max_lines` lines currently on disk,
    // reading at most SEED_BYTES from the end of the file. Sets `offset` to
    // the end of what was read so `poll` only picks up new bytes after.
    pub fn seed(&mut self) -> Result<()> {
        self.buffer.clear();
        self.partial.clear();
        self.offset = 0;
        self.seeded = true;

        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }

        let mut file =
            fs::File::open(&path).with_context(|| format!("Failed to open {}", path.display()))?;
        let len = file.metadata()?.len();
        let start = len.saturating_sub(SEED_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::with_capacity((len - start) as usize);
        file.read_to_end(&mut bytes)?;
        self.offset = start + bytes.len() as u64;

        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        // Started mid-file: the first line is a fragment, drop it
        if start > 0 {
            match text.find('\n') {
                Some(i) => {
                    text.drain(..=i);
                }
                None => text.clear(),
            }
        }
        self.push_text(&text);
        Ok(())
    }

    // Read newly-appended bytes since last poll. Returns count of NEW complete
    // lines appended to the buffer.
    pub fn poll(&mut self) -> Result<usize> {
        let Some(path) = self.path.clone() else {
            return Ok(0);
        };
        if !path.exists() {
            // File removed (tunnel deleted): drop what we showed for it
            if self.offset > 0 || !self.buffer.is_empty() {
                self.seed()?;
            }
            return Ok(0);
        }
        let len = fs::metadata(&path)?.len();

        // File shrank (truncation / rotation), or so much was appended that
        // reading it all would be wasteful: re-seed from the end.
        if len < self.offset || len - self.offset > SEED_BYTES {
            let before = self.buffer.len();
            self.seed()?;
            return Ok(self.buffer.len().saturating_sub(before));
        }

        if len == self.offset {
            return Ok(0);
        }

        let mut file =
            fs::File::open(&path).with_context(|| format!("Failed to open {}", path.display()))?;
        file.seek(SeekFrom::Start(self.offset))?;

        let mut bytes = Vec::with_capacity((len - self.offset) as usize);
        file.read_to_end(&mut bytes)?;
        self.offset += bytes.len() as u64;

        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(self.push_text(&text))
    }

    // Split text into lines, completing any stashed partial line first and
    // stashing a trailing fragment. Returns the number of complete lines.
    fn push_text(&mut self, text: &str) -> usize {
        let combined = std::mem::take(&mut self.partial) + text;
        let mut new_count = 0;
        for chunk in combined.split_inclusive('\n') {
            if let Some(stripped) = chunk.strip_suffix('\n') {
                let line = stripped.strip_suffix('\r').unwrap_or(stripped);
                self.push_line(line.to_string());
                new_count += 1;
            } else {
                // Final chunk with no trailing newline — save for next poll.
                self.partial = chunk.to_string();
            }
        }
        new_count
    }

    fn push_line(&mut self, line: String) {
        if self.buffer.len() >= self.max_lines {
            self.buffer.pop_front();
        }
        self.buffer.push_back(line);
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub fn line(&self, i: usize) -> &str {
        &self.buffer[i]
    }

    #[cfg(test)]
    fn lines(&self) -> Vec<&str> {
        self.buffer.iter().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        (dir, path)
    }

    fn append(path: &PathBuf, text: &str) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        write!(f, "{}", text).unwrap();
    }

    #[test]
    fn seed_reads_last_max_lines_from_file() {
        let (_dir, path) = tmp_file();
        let mut f = fs::File::create(&path).unwrap();
        for i in 0..10 {
            writeln!(f, "line {}", i).unwrap();
        }
        drop(f);

        let mut tail = LogTail::new(path.clone(), 3);
        tail.seed().unwrap();
        assert_eq!(tail.lines(), vec!["line 7", "line 8", "line 9"]);
        assert_eq!(tail.offset, fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn seed_reads_only_the_end_of_a_large_file() {
        let (_dir, path) = tmp_file();
        let mut f = fs::File::create(&path).unwrap();
        // ~3 MB, well over SEED_BYTES
        let mut i = 0;
        while f.metadata().unwrap().len() < SEED_BYTES + 1024 * 1024 {
            writeln!(f, "line {:08} {}", i, "x".repeat(100)).unwrap();
            i += 1;
        }
        drop(f);
        let last = format!("line {:08} {}", i - 1, "x".repeat(100));

        let mut tail = LogTail::new(path.clone(), 100_000);
        tail.seed().unwrap();
        // Every line is complete (the cut-off first fragment was dropped)
        assert!(tail.lines().iter().all(|l| l.starts_with("line ")));
        assert_eq!(*tail.lines().last().unwrap(), last.as_str());
        assert!(tail.len() < i);
        assert_eq!(tail.offset, fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn seed_keeps_trailing_fragment_as_partial() {
        let (_dir, path) = tmp_file();
        fs::write(&path, "one\ntw").unwrap();
        let mut tail = LogTail::new(path.clone(), 100);
        tail.seed().unwrap();
        assert_eq!(tail.lines(), vec!["one"]);
        append(&path, "o\n");
        assert_eq!(tail.poll().unwrap(), 1);
        assert_eq!(tail.lines(), vec!["one", "two"]);
    }

    #[test]
    fn poll_appends_new_lines_only() {
        let (_dir, path) = tmp_file();
        fs::write(&path, "one\n").unwrap();
        let mut tail = LogTail::new(path.clone(), 100);
        tail.seed().unwrap();
        assert_eq!(tail.len(), 1);

        append(&path, "two\nthree\n");
        assert_eq!(tail.poll().unwrap(), 2);
        assert_eq!(tail.lines(), vec!["one", "two", "three"]);
    }

    #[test]
    fn poll_handles_partial_line_across_two_polls() {
        let (_dir, path) = tmp_file();
        fs::write(&path, "").unwrap();
        let mut tail = LogTail::new(path.clone(), 100);
        tail.seed().unwrap();

        append(&path, "partial-");
        assert_eq!(tail.poll().unwrap(), 0);
        assert!(tail.is_empty());

        append(&path, "line\n");
        assert_eq!(tail.poll().unwrap(), 1);
        assert_eq!(tail.lines(), vec!["partial-line"]);
    }

    #[test]
    fn poll_handles_truncation() {
        let (_dir, path) = tmp_file();
        fs::write(&path, "old 0\nold 1\nold 2\nold 3\nold 4\n").unwrap();
        let mut tail = LogTail::new(path.clone(), 100);
        tail.seed().unwrap();
        assert_eq!(tail.len(), 5);

        // Truncate the file (simulates rotation).
        fs::write(&path, "fresh\n").unwrap();
        tail.poll().unwrap();
        assert_eq!(tail.lines(), vec!["fresh"]);
    }

    #[test]
    fn poll_clears_when_file_is_removed() {
        let (_dir, path) = tmp_file();
        fs::write(&path, "one\n").unwrap();
        let mut tail = LogTail::new(path.clone(), 100);
        tail.seed().unwrap();
        fs::remove_file(&path).unwrap();
        tail.poll().unwrap();
        assert!(tail.is_empty());

        // Recreated later (tunnel added again with the same name)
        fs::write(&path, "new\n").unwrap();
        tail.poll().unwrap();
        assert_eq!(tail.lines(), vec!["new"]);
    }

    #[test]
    fn ring_buffer_evicts_oldest_when_over_capacity() {
        let (_dir, path) = tmp_file();
        fs::write(&path, "line 0\nline 1\nline 2\nline 3\nline 4\n").unwrap();
        let mut tail = LogTail::new(path.clone(), 3);
        tail.seed().unwrap();
        assert_eq!(tail.lines(), vec!["line 2", "line 3", "line 4"]);

        append(&path, "line 5\nline 6\n");
        tail.poll().unwrap();
        assert_eq!(tail.lines(), vec!["line 4", "line 5", "line 6"]);
    }

    #[test]
    fn refresh_records_errors_and_placeholders_never_read() {
        let mut p = LogTail::placeholder(vec!["info".into()]);
        assert_eq!(p.refresh(), 0);
        assert_eq!(p.lines(), vec!["info"]);
        assert!(p.error.is_none());

        // A directory can't be read as a file
        let dir = tempfile::tempdir().unwrap();
        let mut t = LogTail::new(dir.path().to_path_buf(), 100);
        t.refresh();
        assert!(t.error.is_some());
    }
}
