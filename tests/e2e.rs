// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end tests exercising `parallel-tar` (create/extract), `parallel-idx`,
//! and the library `verify` entry point.
//!
//! NOTE: `create` changes the process working directory, so the binary is
//! always driven as a subprocess (via `CARGO_BIN_EXE_parallel-tar`) with an
//! explicit `current_dir`. This keeps the tests safe under plain `cargo test`
//! as well as `cargo nextest run` (which isolates each test in its own
//! process anyway).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use ptar_lib::archive::verify::verify;
use ptar_lib::index::tree::{NodeType, TreeNode};

// ─── Test scaffolding ─────────────────────────────────────────────────────

const NUM_THREADS: u32 = 3;

/// Temporary, test-unique working directory. Removed on drop (best effort).
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ptar-e2e-{}-{}", std::process::id(), name
        ));
        // A leftover from a crashed run would trip `create`'s
        // destination-not-free check => always start clean
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path { &self.path }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Build a small directory tree with regular files, nested directories, and
/// (on unix) a symlink. Returns (file count, symlink count).
fn populate_sample_data(root: &Path) -> (usize, usize) {
    let data = root.join("data");
    fs::create_dir_all(data.join("sub/deep")).unwrap();

    let mut f = File::create(data.join("a.txt")).unwrap();
    writeln!(f, "hello world").unwrap();

    let mut f = File::create(data.join("sub/b.txt")).unwrap();
    writeln!(f, "more data").unwrap();

    let mut f = File::create(data.join("sub/deep/c.bin")).unwrap();
    f.write_all(&[0u8, 1, 2, 3, 254, 255]).unwrap();

    #[cfg(unix)]
    let symlinks = {
        std::os::unix::fs::symlink("a.txt", data.join("link_to_a")).unwrap();
        1
    };
    #[cfg(not(unix))]
    let symlinks = 0;

    (3, symlinks)
}

fn bin() -> &'static str { env!("CARGO_BIN_EXE_parallel-tar") }

/// Run the `parallel-tar` binary in `cwd`, asserting on the exit status.
fn run_ptar(cwd: &Path, args: &[&str], expect_success: bool) {
    let output = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("failed to spawn parallel-tar");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.success(), expect_success,
        "parallel-tar {:?} exited with {:?}\nstderr:\n{}",
        args, output.status, stderr
    );
}

/// Recursively assert that directories `a` and `b` have identical contents
/// (entry names, file bytes, symlink targets).
fn assert_dirs_equal(a: &Path, b: &Path) {
    let list = |d: &Path| -> BTreeSet<String> {
        fs::read_dir(d).unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };
    let entries_a = list(a);
    let entries_b = list(b);
    assert_eq!(
        entries_a, entries_b,
        "directory listings differ between {:?} and {:?}", a, b
    );

    for name in entries_a {
        let pa = a.join(&name);
        let pb = b.join(&name);
        let ma = fs::symlink_metadata(&pa).unwrap();

        if ma.file_type().is_symlink() {
            assert!(
                fs::symlink_metadata(&pb).unwrap().file_type().is_symlink(),
                "{:?} is a symlink but {:?} is not", pa, pb
            );
            assert_eq!(
                fs::read_link(&pa).unwrap(), fs::read_link(&pb).unwrap(),
                "symlink targets differ for {:?}", name
            );
        } else if ma.is_dir() {
            assert_dirs_equal(&pa, &pb);
        } else {
            assert_eq!(
                fs::read(&pa).unwrap(), fs::read(&pb).unwrap(),
                "file contents differ for {:?}", name
            );
        }
    }
}

/// Count `File` and `Symlink` nodes in a verify tree.
fn count_nodes(node: &TreeNode) -> (usize, usize) {
    match &node.node_type {
        NodeType::File { .. } => (1, 0),
        NodeType::Symlink { .. } => (0, 1),
        NodeType::Directory { children } => {
            children.iter().fold((0, 0), |(f, s), c| {
                let (cf, cs) = count_nodes(c);
                (f + cf, s + cs)
            })
        }
        _ => (0, 0),
    }
}

/// Shared create → extract → compare flow.
fn create_extract_roundtrip(name: &str, compress: bool) {
    let tmp = TestDir::new(name);
    populate_sample_data(tmp.path());

    let nt = NUM_THREADS.to_string();
    let mut create_args = vec!["-c", "-f", "myarch", "-n", &nt, "data"];
    let mut extract_args = vec!["-x", "-f", "myarch/myarch", "-n", &nt, "out"];
    if compress {
        create_args.push("-z");
        extract_args.push("-z");
    }

    run_ptar(tmp.path(), &create_args, true);

    // One shard per worker thread must exist
    let ext = if compress { "tar.gz" } else { "tar" };
    for idx in 0..NUM_THREADS {
        let shard = tmp.path().join(format!("myarch/myarch.{}.{}", idx, ext));
        assert!(shard.is_file(), "missing shard {:?}", shard);
    }

    fs::create_dir_all(tmp.path().join("out")).unwrap();
    run_ptar(tmp.path(), &extract_args, true);

    assert_dirs_equal(
        &tmp.path().join("data"), &tmp.path().join("out/data")
    );
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[test]
fn index_from_tree_accepts_timeout_retry_counts() {
    let tmp = TestDir::new("index-retries");
    fs::write(tmp.path().join("file"), b"abc").unwrap();
    let index = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_parallel-idx"))
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap()
    };
    assert!(index(&["-e", "-f", "tree.etr", "file"]).status.success());

    for (count, exit_code) in [("0", 0), ("3", 0), ("-1", 0), ("-2", 2), ("bad", 2)] {
        let output = index(&[
            "-f", "out.idx", "-n", "4", "--retry-on-timeout", count, "-t", "tree.etr",
        ]);
        assert_eq!(
            output.status.code(), Some(exit_code),
            "--retry-on-timeout {count}: {}", String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn e2e_create_extract_roundtrip() {
    create_extract_roundtrip("roundtrip", false);
}

#[test]
fn e2e_create_extract_roundtrip_compressed() {
    create_extract_roundtrip("roundtrip-gz", true);
}

#[test]
fn archiving_rejects_a_file_that_changes_length_after_its_header() {
    struct ResizeOnHeader {
        file: Option<File>,
        new_size: u64,
    }
    impl Write for ResizeOnHeader {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(file) = self.file.take() {
                file.set_len(self.new_size)?;
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    use std::io::{ErrorKind, Seek, SeekFrom};
    let tmp = TestDir::new("source-size");
    let path = tmp.path().join("source");
    for (actual, kind) in [
        (0, ErrorKind::UnexpectedEof),
        (1023, ErrorKind::UnexpectedEof), // A one-byte loss fits in the same tar block.
        (1025, ErrorKind::InvalidData),
    ] {
        fs::write(&path, [0x42; 1024]).unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        let mut tar = tar::Builder::new(ResizeOnHeader { file: Some(file), new_size: actual });
        let err = tar.append_path_with_name(&path, "source").unwrap_err();
        assert_eq!(err.kind(), kind);
        assert!(err.to_string().contains("1024"), "{err}");
    }

    // On filesystems with GNU sparse support, truncate inside a data extent.
    let mut file = File::create(&path).unwrap();
    let hole = 1 << 20;
    file.seek(SeekFrom::Start(hole)).unwrap();
    file.write_all(&[0x42; 1024]).unwrap();
    file.sync_all().unwrap();
    let mut tar = tar::Builder::new(ResizeOnHeader { file: Some(file), new_size: hole + 1023 });
    assert_eq!(
        tar.append_path_with_name(&path, "sparse").unwrap_err().kind(),
        ErrorKind::UnexpectedEof,
    );
}

#[test]
fn verify_reports_the_shard_offset_and_previous_entry_for_a_missing_payload() {
    let tmp = TestDir::new("bad-header");
    let mut shard = File::create(tmp.path().join("backup.0.tar")).unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(512);
    for name in ["missing.pdf", "next.pdf"] {
        header.set_path(name).unwrap();
        header.set_cksum();
        shard.write_all(header.as_bytes()).unwrap();
    }
    // The second header gets consumed as the first member's payload. The
    // reader then lands inside the second member while looking for a header.
    shard.write_all(&[0xff; 512]).unwrap();
    shard.write_all(&[0; 1024]).unwrap();
    drop(shard);

    let output = Command::new(bin())
        .current_dir(tmp.path())
        .env("RUST_LOG", "off")
        .args(["-i", "-n", "1", "-f", "verify.idx", "backup"])
        .output().unwrap();
    assert!(!output.status.success());
    let message = String::from_utf8(output.stderr).unwrap();
    for part in ["backup.0.tar", "after entry \"missing.pdf\"", "checksum field at uncompressed byte 1024"] {
        assert!(message.contains(part), "{message}");
    }
    assert!(!message.contains('\u{fffd}'), "{message}");
    assert!(!tmp.path().join("verify.idx").exists());
}

#[test]
fn archive_retry_option_works_across_create_verify_and_extract() {
    let tmp = TestDir::new("archive-retries");
    let cwd = tmp.path();
    populate_sample_data(cwd);
    let tree = Command::new(env!("CARGO_BIN_EXE_parallel-idx"))
        .args(["-e", "-f", "tree.etr", "data"])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(tree.status.success());
    for invalid in ["-2", "bad"] {
        let args = ["-ctz", "-n", "2", "--retry-on-timeout", invalid, "-f", "backup/archive", "tree.etr"];
        run_ptar(cwd, &args, false);
        assert!(!cwd.join("backup").exists());
    }
    let prefix = "backup/archive/archive";
    let create = ["-ctz", "-n", "2", "--retry-on-timeout", "3", "-f", "backup/archive", "tree.etr"];
    let verify = ["-iz", "-n", "2", "--retry-on-timeout", "-1", "-f", "verified.idx", prefix];
    let extract = ["-xz", "-n", "2", "--retry-on-timeout", "0", "-f", prefix, "out"];
    run_ptar(cwd, &create, true);
    run_ptar(cwd, &verify, true);
    fs::create_dir_all(cwd.join("out")).unwrap();
    run_ptar(cwd, &extract, true);
    assert_dirs_equal(&cwd.join("data"), &cwd.join("out/data"));
}

#[test]
fn e2e_create_refuses_existing_destination() {
    let tmp = TestDir::new("dest-not-free");
    populate_sample_data(tmp.path());
    fs::create_dir_all(tmp.path().join("myarch")).unwrap();

    let nt = NUM_THREADS.to_string();
    run_ptar(tmp.path(), &["-c", "-f", "myarch", "-n", &nt, "data"], false);
}

#[test]
fn e2e_verify_scans_all_entries() {
    let tmp = TestDir::new("verify");
    let (n_files, n_symlinks) = populate_sample_data(tmp.path());

    let nt = NUM_THREADS.to_string();
    run_ptar(tmp.path(), &["-c", "-f", "myarch", "-n", &nt, "data"], true);

    // `verify` does not change the cwd => absolute archive prefix works
    let archive = tmp.path().join("myarch/myarch")
        .to_string_lossy().into_owned();
    let tree = verify(&archive, &NUM_THREADS, &false, &false, None)
        .expect("verify failed");

    let (files, symlinks) = count_nodes(&tree);
    assert_eq!(files, n_files, "verify missed regular files");
    assert_eq!(symlinks, n_symlinks, "verify missed symlinks");

    // The scan is deterministic => a second pass yields an identical tree
    let tree2 = verify(&archive, &NUM_THREADS, &false, &false, None)
        .expect("second verify failed");
    let (files2, symlinks2) = count_nodes(&tree2);
    assert_eq!((files, symlinks), (files2, symlinks2));
}

#[test]
fn e2e_verify_missing_archive_fails_fast() {
    // Regression test for the channel-closing fix: when every worker dies
    // immediately (no shards to open), `verify` must return an error
    // promptly instead of hanging in `collect_until_finished`
    let tmp = TestDir::new("verify-missing");
    let archive = tmp.path().join("does-not-exist/nope")
        .to_string_lossy().into_owned();

    let start = Instant::now();
    let result = verify(&archive, &NUM_THREADS, &false, &false, None);
    assert!(result.is_err(), "verify of a missing archive must fail");
    assert!(
        start.elapsed().as_secs() < 30,
        "verify took {:?} to fail => channel termination is broken",
        start.elapsed()
    );
}

#[test]
fn e2e_extract_missing_archive_fails_fast() {
    // Same hang regression, via the binary's extract path
    let tmp = TestDir::new("extract-missing");
    fs::create_dir_all(tmp.path().join("out")).unwrap();

    let nt = NUM_THREADS.to_string();
    let start = Instant::now();
    run_ptar(
        tmp.path(),
        &["-x", "-f", "does-not-exist/nope", "-n", &nt, "out"],
        false
    );
    assert!(
        start.elapsed().as_secs() < 30,
        "extract took {:?} to fail => channel termination is broken",
        start.elapsed()
    );
}
