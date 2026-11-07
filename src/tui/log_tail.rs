// Byte-offset ring-buffer tail of a log file.
//
// Reads only newly-appended bytes on each poll, keeping a bounded VecDeque
// of the last N complete lines. Handles file truncation/rotation (restarts
// from offset 0). Handles partial lines (bytes without a trailing newline)
// by stashing them and prepending on the next poll.

use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use anyhow::{Context, Result};

pub struct LogTail {
    path: PathBuf,
    offset: u64,
    buffer: VecDeque<String>,
    partial: String,
    pub max_lines: usize,
}

impl LogTail {
    pub fn new(path: PathBuf, max_lines: usize) -> Self {
        Self {
            path,
            offset: 0,
            buffer: VecDeque::with_capacity(max_lines),
            partial: String::new(),
            max_lines,
        }
    }

    // Seed the buffer with the LAST `max_lines` lines currently on disk.
    // Sets `offset` to end-of-file so `poll` only picks up new bytes after.
    pub fn seed(&mut self) -> Result<()> {
        self.buffer.clear();
        self.partial.clear();

        if !self.path.exists() {
            self.offset = 0;
            return Ok(());
        }

        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("Failed to read {}", self.path.display()))?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start = all_lines.len().saturating_sub(self.max_lines);
        for line in &all_lines[start..] {
            self.buffer.push_back((*line).to_string());
        }
        self.offset = fs::metadata(&self.path)?.len();
        Ok(())
    }

    // Read newly-appended bytes since last poll. Returns count of NEW complete
    // lines appended to the buffer.
    pub fn poll(&mut self) -> Result<usize> {
        if !self.path.exists() {
            return Ok(0);
        }
        let len = fs::metadata(&self.path)?.len();

        // File shrank (truncation / rotation). Re-seed from scratch.
        if len < self.offset {
            self.seed()?;
            return Ok(0);
        }

        if len == self.offset {
            return Ok(0);
        }

        let mut file = fs::File::open(&self.path)
            .with_context(|| format!("Failed to open {}", self.path.display()))?;
        file.seek(SeekFrom::Start(self.offset))?;

        let mut bytes = Vec::with_capacity((len - self.offset) as usize);
        file.read_to_end(&mut bytes)?;
        self.offset = len;

        let text = String::from_utf8_lossy(&bytes);
        let mut new_count = 0;

        // Prepend any partial line from the previous poll.
        let combined = format!("{}{}", self.partial, text);
        self.partial.clear();

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

        Ok(new_count)
    }

    fn push_line(&mut self, line: String) {
        if self.buffer.len() == self.max_lines {
            self.buffer.pop_front();
        }
        self.buffer.push_back(line);
    }

    // Replace the buffer with a fixed set of lines (used by demo mode).
    pub fn inject_lines(&mut self, lines: Vec<String>) {
        self.buffer.clear();
        for line in lines {
            self.push_line(line);
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.buffer.iter().map(String::as_str)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
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
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines, vec!["line 7", "line 8", "line 9"]);
        assert_eq!(tail.offset, fs::metadata(&path).unwrap().len());
    }

    #[test]
    fn poll_appends_new_lines_only() {
        let (_dir, path) = tmp_file();
        {
            let mut f = fs::File::create(&path).unwrap();
            writeln!(f, "one").unwrap();
        }
        let mut tail = LogTail::new(path.clone(), 100);
        tail.seed().unwrap();
        assert_eq!(tail.len(), 1);

        {
            let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, "two").unwrap();
            writeln!(f, "three").unwrap();
        }
        let n = tail.poll().unwrap();
        assert_eq!(n, 2);
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines, vec!["one", "two", "three"]);
    }

    #[test]
    fn poll_handles_partial_line_across_two_polls() {
        let (_dir, path) = tmp_file();
        fs::write(&path, "").unwrap();
        let mut tail = LogTail::new(path.clone(), 100);
        tail.seed().unwrap();

        // Append a partial line (no trailing newline).
        {
            let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
            write!(f, "partial-").unwrap();
        }
        let n = tail.poll().unwrap();
        assert_eq!(n, 0);
        assert!(tail.is_empty());

        // Complete the line in a second write.
        {
            let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, "line").unwrap();
        }
        let n = tail.poll().unwrap();
        assert_eq!(n, 1);
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines, vec!["partial-line"]);
    }

    #[test]
    fn poll_handles_truncation() {
        let (_dir, path) = tmp_file();
        {
            let mut f = fs::File::create(&path).unwrap();
            for i in 0..5 {
                writeln!(f, "old {}", i).unwrap();
            }
        }
        let mut tail = LogTail::new(path.clone(), 100);
        tail.seed().unwrap();
        assert_eq!(tail.len(), 5);

        // Truncate the file (simulates rotation).
        fs::write(&path, "fresh\n").unwrap();
        let n = tail.poll().unwrap();
        assert_eq!(n, 0); // Re-seed doesn't count as newly-appended.
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines, vec!["fresh"]);
    }

    #[test]
    fn ring_buffer_evicts_oldest_when_over_capacity() {
        let (_dir, path) = tmp_file();
        {
            let mut f = fs::File::create(&path).unwrap();
            for i in 0..5 {
                writeln!(f, "line {}", i).unwrap();
            }
        }
        let mut tail = LogTail::new(path.clone(), 3);
        tail.seed().unwrap();
        assert_eq!(tail.len(), 3);
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines, vec!["line 2", "line 3", "line 4"]);

        {
            let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
            writeln!(f, "line 5").unwrap();
            writeln!(f, "line 6").unwrap();
        }
        tail.poll().unwrap();
        let lines: Vec<&str> = tail.lines().collect();
        assert_eq!(lines, vec!["line 4", "line 5", "line 6"]);
    }
}
