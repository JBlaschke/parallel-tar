// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end tests exercising the `parallel-tar` binary (create/extract) and
//! the library `verify` entry point.
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
fn e2e_create_extract_roundtrip() {
    create_extract_roundtrip("roundtrip", false);
}

#[test]
fn e2e_create_extract_roundtrip_compressed() {
    create_extract_roundtrip("roundtrip-gz", true);
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
