// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resume a tar member after a source timeout, including on a gzip stream
//! that cannot be rewound. On replay, discard the already-written prefix
//! and verify it before allowing any new bytes into the shard.

use crate::retry::TimeoutRetries;
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;
use tar::Builder;

pub(super) struct EntryWriter<W> {
    inner: W,
    written: u64,
    digest: Option<Sha256>,
    // Bytes still to skip and the digest of the prefix seen on this replay.
    replay: Option<(u64, Sha256)>,
    output_failed: bool,
}

impl<W: Write> EntryWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            written: 0,
            digest: None,
            replay: None,
            output_failed: false,
        }
    }

    pub(super) fn can_skip_entry(&self) -> bool {
        self.written == 0 && !self.output_failed
    }
}

impl<W: Write> Write for EntryWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some((left, replay_digest)) = &mut self.replay {
            let n = (*left).min(buf.len() as u64) as usize;
            replay_digest.update(&buf[..n]);
            *left -= n as u64;
            if *left == 0 {
                let original = self.digest.as_ref().expect("retries enabled");
                if replay_digest.clone().finalize() != original.clone().finalize() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "tar entry changed during retry",
                    ));
                }
                self.replay = None;
            }
            return Ok(n);
        }

        let n = self.inner.write(buf).inspect_err(|e| {
            self.output_failed = e.kind() != io::ErrorKind::Interrupted;
        })?;
        if n == 0 && !buf.is_empty() {
            self.output_failed = true;
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.written += n as u64;
        if let Some(digest) = &mut self.digest {
            digest.update(&buf[..n]);
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner
            .flush()
            .inspect_err(|_| self.output_failed = true)
    }
}

pub(super) fn append_with_retries<W: Write>(
    archive: &mut Builder<EntryWriter<W>>,
    path: &Path,
    limit: Option<u64>,
    mut append: impl FnMut(&mut Builder<EntryWriter<W>>) -> io::Result<()>,
    mut sleep: impl FnMut(Duration),
) -> io::Result<()> {
    let writer = archive.get_mut();
    writer.written = 0;
    writer.digest = (limit != Some(0)).then(Sha256::new);
    writer.replay = None;
    let mut retries = TimeoutRetries::new(limit);
    loop {
        match append(archive) {
            Err(e) if !archive.get_ref().output_failed && retries.wait(path, &e, &mut sleep) => {
                let writer = archive.get_mut();
                writer.replay = (writer.written > 0).then(|| (writer.written, Sha256::new()));
            }
            Ok(()) if archive.get_ref().replay.is_some() => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "tar entry shortened during retry",
                ));
            }
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, read::GzDecoder, write::GzEncoder};
    use std::io::Read;
    use tar::{Archive, Header};

    struct Timeout;
    impl Read for Timeout {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::TimedOut.into())
        }
    }

    fn member<W: Write>(ar: &mut Builder<W>, path: &Path, data: impl Read) -> io::Result<()> {
        let mut header = Header::new_gnu();
        header.set_size(6);
        ar.append_data(&mut header, path, data)
    }

    fn write_members(out: impl Write) {
        let mut ar = Builder::new(EntryWriter::new(out));
        // Include the GNU long-name header in the prefix that must be replayed.
        let name = "x".repeat(150);
        let path = Path::new(&name);
        let mut attempts = 0;
        append_with_retries(
            &mut ar,
            path,
            Some(4),
            |ar| {
                attempts += 1;
                match attempts {
                    1 => Err(io::ErrorKind::TimedOut.into()),
                    2 => member(ar, path, b"abc".as_slice().chain(Timeout)),
                    3 => member(ar, path, b"a".as_slice().chain(Timeout)),
                    4 => member(ar, path, b"abcde".as_slice().chain(Timeout)),
                    5 => member(ar, path, b"abcdef".as_slice()),
                    _ => panic!("unexpected retry"),
                }
            },
            |_| {},
        )
        .unwrap();
        let next = Path::new("next");
        append_with_retries(
            &mut ar,
            next,
            None,
            |ar| member(ar, next, b"second".as_slice()),
            |_| {},
        )
        .unwrap();
        ar.finish().unwrap();
    }

    #[test]
    fn retries_produce_one_intact_member_in_plain_and_gzip_shards() {
        let _ = env_logger::builder().is_test(true).try_init();
        for compressed in [false, true] {
            let mut bytes = Vec::new();
            let reader: Box<dyn Read> = if compressed {
                let mut gz = GzEncoder::new(&mut bytes, Compression::fast());
                write_members(&mut gz);
                gz.finish().unwrap();
                Box::new(GzDecoder::new(bytes.as_slice()))
            } else {
                write_members(&mut bytes);
                Box::new(bytes.as_slice())
            };
            let mut ar = Archive::new(reader);
            let mut entries = ar.entries().unwrap();
            for (name, expected) in [("x".repeat(150), "abcdef"), ("next".into(), "second")] {
                let mut entry = entries.next().unwrap().unwrap();
                assert_eq!(entry.path().unwrap(), Path::new(&name));
                let mut contents = String::new();
                entry.read_to_string(&mut contents).unwrap();
                assert_eq!(contents, expected);
            }
            assert!(entries.next().is_none());
        }
    }

    #[test]
    fn exhausted_retries_leave_one_partial_entry() {
        let path = Path::new("file");
        for limit in [0, 2] {
            let mut ar = Builder::new(EntryWriter::new(Vec::new()));
            let mut attempts = 0;
            let err = append_with_retries(
                &mut ar,
                path,
                Some(limit),
                |ar| {
                    attempts += 1;
                    assert!(attempts <= limit + 1);
                    member(ar, path, b"abc".as_slice().chain(Timeout))
                },
                |_| {},
            )
            .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::TimedOut);
            assert_eq!(attempts, limit + 1);
            assert_eq!(ar.get_ref().inner.len(), 512 + 3);
            assert!(!ar.get_ref().can_skip_entry());
        }
    }

    #[test]
    fn rejects_a_changed_prefix() {
        let mut ar = Builder::new(EntryWriter::new(Vec::new()));
        let path = Path::new("file");
        let mut attempts = 0;
        let err = append_with_retries(
            &mut ar,
            path,
            Some(1),
            |ar| {
                attempts += 1;
                if attempts == 1 {
                    member(ar, path, b"abc".as_slice().chain(Timeout))
                } else {
                    member(ar, path, b"ABCdef".as_slice())
                }
            },
            |_| {},
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(!ar.get_ref().can_skip_entry());
    }

    #[test]
    fn an_output_timeout_is_terminal() {
        struct FailedOutput;
        impl Write for FailedOutput {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::TimedOut.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut ar = Builder::new(EntryWriter::new(FailedOutput));
        let path = Path::new("file");
        let err = append_with_retries(
            &mut ar,
            path,
            None,
            |ar| member(ar, path, b"abcdef".as_slice()),
            |_| panic!("retried output failure"),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(!ar.get_ref().can_skip_entry());
    }
}
