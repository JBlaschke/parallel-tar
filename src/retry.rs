// SPDX-License-Identifier: AGPL-3.0-or-later

//! A timeout retry budget shared by the operations on one input.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::thread;
use std::time::Duration;

pub(crate) struct TimeoutRetries {
    limit: Option<u64>,
    used: u64,
    delay: Duration,
}

/// Retry underneath the decoder so it consumes each byte exactly once.
pub(crate) struct RetryReader<'a, R> {
    input: R,
    path: &'a Path,
    retries: &'a mut TimeoutRetries,
    offset: u64,
}

impl<'a> RetryReader<'a, File> {
    pub(crate) fn open(path: &'a Path, retries: &'a mut TimeoutRetries) -> io::Result<Self> {
        let input = retries.run(path, || File::open(path), thread::sleep)?;
        Ok(Self {
            input,
            path,
            retries,
            offset: 0,
        })
    }
}

impl<R: Read + Seek> Read for RetryReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut retrying = false;
        let n = self.retries.run(
            self.path,
            || {
                if retrying {
                    self.input.seek(SeekFrom::Start(self.offset))?;
                }
                retrying = true;
                self.input.read(buf)
            },
            thread::sleep,
        )?;
        self.offset += n as u64;
        Ok(n)
    }
}

impl TimeoutRetries {
    pub(crate) fn new(limit: Option<u64>) -> Self {
        Self {
            limit,
            used: 0,
            delay: Duration::from_secs(1),
        }
    }

    pub(crate) fn wait(
        &mut self,
        path: &Path,
        error: &io::Error,
        sleep: &mut impl FnMut(Duration),
    ) -> bool {
        if error.kind() != io::ErrorKind::TimedOut
            || self.limit.is_some_and(|limit| self.used >= limit)
        {
            return false;
        }
        self.used = self.used.saturating_add(1);
        log::debug!(
            "{path:?}: {error}. Retrying in {:?} (retry {})",
            self.delay,
            self.used
        );
        sleep(self.delay);
        self.delay = (self.delay * 2).min(Duration::from_secs(30));
        true
    }

    pub(crate) fn run<T>(
        &mut self,
        path: &Path,
        mut operation: impl FnMut() -> io::Result<T>,
        mut sleep: impl FnMut(Duration),
    ) -> io::Result<T> {
        loop {
            match operation() {
                Err(e) if self.wait(path, &e, &mut sleep) => continue,
                result => return result,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct IntermittentInput {
        bytes: Cursor<&'static [u8]>,
        timed_out: bool,
    }

    impl Read for IntermittentInput {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.bytes.position() == 3 && !self.timed_out {
                self.timed_out = true;
                self.bytes.set_position(6);
                return Err(io::ErrorKind::TimedOut.into());
            }
            let size = buf.len().min(3);
            self.bytes.read(&mut buf[..size])
        }
    }

    impl Seek for IntermittentInput {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.bytes.seek(to)
        }
    }

    #[test]
    fn retries_resume_at_the_last_read_and_both_passes_spend_one_budget() {
        let mut retries = TimeoutRetries::new(Some(1));
        let mut read_pass = |output: &mut Vec<u8>| {
            let input = IntermittentInput {
                bytes: Cursor::new(b"abcdef"),
                timed_out: false,
            };
            let mut reader = RetryReader {
                input,
                path: Path::new("shard.tar"),
                retries: &mut retries,
                offset: 0,
            };
            reader.read_to_end(output)
        };
        let mut first = Vec::new();
        read_pass(&mut first).unwrap();
        assert_eq!(first, b"abcdef");

        let mut second = Vec::new();
        assert_eq!(
            read_pass(&mut second).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(second, b"abc");
    }
}
