// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::archive::error::{ArchiverError, Relabel};
use crate::archive::mutex::Pipe;
use crate::files::path::sanitize_rel_path;
use crate::index::tree::{TreeNode, NodeType};
use crate::index::crypto::{hash_reader_md5, hash_reader_sha256};

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

use flate2::read::GzDecoder;
use tar::{Archive, EntryType};

use log::{debug, error, info, warn};

// ─── Public types ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ScannedEntry {
    pub rel_path: PathBuf,
    pub kind:     ScannedKind,
    pub hash:     Option<String>,
    pub size:     u64,
}

#[derive(Debug, Clone)]
pub enum ScannedKind {
    File,
    Directory,
    Symlink { target: PathBuf },
    Socket,
    Fifo,
    Device,
    Unknown { error: String },
}

/// Alias to keep the `Vec<...>` line readable.
type VerifyHandle = JoinHandle<Result<(), ArchiverError<ScannedEntry>>>;

// ─── Worker: scan one shard ──────────────────────────────────────────────

fn scan_worker_thread(
    tar_path: &str,
    compress: bool,
    use_md5:  bool,
    pipe:     &Pipe<ScannedEntry>,
) -> Result<(), ArchiverError<ScannedEntry>> {
    let input = File::open(tar_path)?;
    let buffered = BufReader::with_capacity(1 << 20, input);
    let reader: Box<dyn Read> = if compress {
        Box::new(GzDecoder::new(buffered))
    } else {
        Box::new(buffered)
    };

    let mut archive = Archive::new(reader);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);

    for entry_res in archive.entries()? {
        let mut entry = entry_res?;
        let header   = entry.header().clone();
        let ent_type = header.entry_type();

        let rel = match entry.path() {
            Ok(p) => sanitize_rel_path(&p),
            Err(_) => None,
        };
        let Some(rel) = rel else {
            warn!("Skipping unsafe path in '{}': {:?}", tar_path, entry.path());
            continue;
        };

        let mut size = header.size().unwrap_or(0);

        let (kind, hash) = match ent_type {
            EntryType::Directory => (ScannedKind::Directory, None),
            EntryType::Regular | EntryType::Continuous | EntryType::GNUSparse => {

                if ent_type == EntryType::GNUSparse {
                    // Old-GNU sparse member ('S'). Two things differ from a
                    // Regular entry:
                    //  1. header.size() is the *archived* (packed) byte
                    //     count; the file's logical size is in the GNU
                    //     header's realsize field. parallel-idx records
                    //     the logical size (st_size), so use realsize.
                    //  2. The data stream is packed segments — but tar-rs
                    //     expands the holes as zeros through Entry's Read
                    //     impl, so hashing the stream below yields the
                    //     logical content, identical to hashing the file
                    //     on disk.
                    if let Some(g) = header.as_gnu() {
                        match g.real_size() {
                            Ok(rs) => size = rs,
                            Err(e) => warn!(
                                "sparse entry {:?}: could not parse realsize \
                                 ({}); falling back to archived size",
                                rel, e
                            ),
                        }
                    } else {
                        warn!(
                            "sparse entry {:?}: no GNU header view; \
                             falling back to archived size", rel
                        );
                    }
                } else {
                    // PAX-format sparse members (GNU tar --format=posix
                    // --sparse) arrive as Regular entries whose data stream
                    // is a sparse map followed by packed data — tar-rs does
                    // NOT reconstruct these, so hashing the stream would
                    // produce a confidently wrong hash. Verify means
                    // verified: hard-error instead.
                    if let Ok(Some(exts)) = entry.pax_extensions() {
                        let pax_sparse = exts
                            .filter_map(|e| e.ok())
                            .any(|e| e.key()
                                .map(|k| k.starts_with("GNU.sparse."))
                                .unwrap_or(false));
                        if pax_sparse {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!(
                                    "entry {:?} in '{}' is a PAX-format \
                                     sparse member, which cannot be hashed \
                                     correctly from the stream; re-create \
                                     the archive with --format=gnu, or \
                                     extract and index from disk",
                                    rel, tar_path
                                ),
                            ).into());
                        }
                    }
                }

                let h = if use_md5 {
                    hash_reader_md5(&mut entry)?
                } else {
                    hash_reader_sha256(&mut entry)?
                };
                (ScannedKind::File, Some(h))
            }
            EntryType::Symlink | EntryType::Link => {
                let target = entry
                    .link_name()
                    .ok()
                    .flatten()
                    .map(|p| p.into_owned())
                    .unwrap_or_default();
                (ScannedKind::Symlink { target }, None)
            }
            EntryType::Fifo => (ScannedKind::Fifo, None),
            EntryType::Char | EntryType::Block => (ScannedKind::Device, None),
            other => {
                let msg = format!("unhandled tar entry type: {:?}", other);
                (ScannedKind::Unknown { error: msg }, None)
            }
        };

        // Drain any unread bytes so the tar reader advances to the next header.
        std::io::copy(&mut entry, &mut std::io::sink())?;

        let scanned = ScannedEntry { rel_path: rel, kind, hash, size };
        pipe.input().send(scanned)?;
    }

    Ok(())
}

// ─── Public entry point ──────────────────────────────────────────────────

pub fn verify(
    archive_name: &String,
    num_threads:  &u32,
    compress:     &bool,
    use_md5:      &bool,
    root_prefix:  Option<&Path>,
) -> Result<Arc<TreeNode>, ArchiverError<String>> {
    let pipe = Pipe::<ScannedEntry>::new();
    let loc_compress = *compress;
    let loc_use_md5  = *use_md5;

    info!(
        "SETUP: Starting {} worker threads to scan '{}'",
        num_threads, archive_name
    );
    let mut handles: Vec<VerifyHandle> = Vec::with_capacity(*num_threads as usize);

    for idx in 0..*num_threads {
        let loc_pipe = pipe.clone();
        let name = if loc_compress {
            format!("{}.{}.tar.gz", archive_name, idx)
        } else {
            format!("{}.{}.tar", archive_name, idx)
        };
        info!("Starting worker thread: {} scanning '{}'", idx, name);

        handles.push(thread::spawn(move || {
            match scan_worker_thread(
                name.as_str(), loc_compress, loc_use_md5, &loc_pipe,
            ) {
                Err(e) => {
                    error!("Error from spawned 'verify' thread: '{}'", e);
                    loc_pipe.set_completed()?;
                    Err(e)
                }
                Ok(()) => Ok(()),
            }
        }));
    }

    info!(" ... waiting for workers to finish scanning ...");
    for h in handles {
        h.join().unwrap_or_else(|err| {
            warn!("Failed thread join: '{:?}'", err);
            Ok(())
        }).map_err(Relabel::<String>::relabel)?;
    }
    info!(" ... workers are done!");
    pipe.set_completed().map_err(Relabel::<String>::relabel)?;

    let scanned = pipe.collect_until_finished();
    pipe.close();
    info!("Collected {} entries; assembling tree", scanned.len());

    let tree = build_tree(scanned, root_prefix)?;
    Ok(tree)
}

// ─── Tree assembly ───────────────────────────────────────────────────────

enum Pending {
    File      { size: u64, hash: String },
    Directory,
    Symlink   { target: PathBuf },
    Socket,
    Fifo,
    Device,
    Unknown   { error: String },
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn common_ancestor<'a, I: IntoIterator<Item = &'a Path>>(paths: I) -> Option<PathBuf> {
    let mut iter = paths.into_iter();
    let first = iter.next()?;
    let mut acc: Vec<&std::ffi::OsStr> = first.iter().collect();
    for p in iter {
        let comps: Vec<&std::ffi::OsStr> = p.iter().collect();
        let n = acc.iter().zip(comps.iter()).take_while(|(a, b)| a == b).count();
        acc.truncate(n);
        if acc.is_empty() { break; }
    }
    Some(acc.iter().collect::<PathBuf>())
}

fn build_node(
    path:      &Path,
    nodes:     &mut HashMap<PathBuf, Pending>,
    adjacency: &HashMap<PathBuf, Vec<PathBuf>>,
) -> Arc<TreeNode> {
    let pending = nodes.remove(path).expect("node missing during freeze");
    let (node_type, precomputed_hash) = match pending {
        Pending::File { size, hash } => (NodeType::File { size }, Some(hash)),
        Pending::Directory => {
            let mut child_paths = adjacency.get(path).cloned().unwrap_or_default();
            child_paths.sort_by(|a, b| name_of(a).cmp(&name_of(b)));
            let children: Vec<Arc<TreeNode>> = child_paths
                .iter()
                .map(|c| build_node(c, nodes, adjacency))
                .collect();
            (NodeType::Directory { children }, None)
        }
        Pending::Symlink { target }   => (NodeType::Symlink { target }, None),
        Pending::Socket               => (NodeType::Socket {}, None),
        Pending::Fifo                 => (NodeType::Fifo {},   None),
        Pending::Device               => (NodeType::Device {}, None),
        Pending::Unknown { error }    => (NodeType::Unknown { error }, None),
    };

    Arc::new(TreeNode {
        name: name_of(path),
        path: path.to_path_buf(),
        node_type,
        metadata: RwLock::new(None),
        hash:     RwLock::new(precomputed_hash),
    })
}

fn build_tree(
    entries:     Vec<ScannedEntry>,
    root_prefix: Option<&Path>,
) -> Result<Arc<TreeNode>, ArchiverError<String>> {
    let with_abs: Vec<(PathBuf, ScannedEntry)> = entries
        .into_iter()
        .map(|e| {
            let abs = match root_prefix {
                Some(r) => r.join(&e.rel_path),
                None    => e.rel_path.clone(),
            };
            (abs, e)
        })
        .collect();

    if with_abs.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "no entries scanned from archive — empty or unreadable?",
        ).into());
    }

    let root_path = common_ancestor(with_abs.iter().map(|(p, _)| p.as_path()))
        .unwrap_or_else(|| PathBuf::from("."));
    debug!("Verify tree root: {:?}", root_path);

    let mut nodes: HashMap<PathBuf, Pending> = HashMap::new();
    nodes.insert(root_path.clone(), Pending::Directory);

    for (abs, e) in &with_abs {
        // Walk parents up to root, ensuring each exists as a Directory.
        let mut cur = abs.parent();
        while let Some(p) = cur {
            nodes.entry(p.to_path_buf()).or_insert(Pending::Directory);
            if p == root_path { break; }
            cur = p.parent();
        }

        let pending = match &e.kind {
            ScannedKind::File => Pending::File {
                size: e.size,
                hash: e.hash.clone().ok_or_else(|| std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("file entry {:?} has no precomputed hash", abs),
                ))?,
            },
            ScannedKind::Directory          => Pending::Directory,
            ScannedKind::Symlink { target } => Pending::Symlink { target: target.clone() },
            ScannedKind::Socket             => Pending::Socket,
            ScannedKind::Fifo               => Pending::Fifo,
            ScannedKind::Device             => Pending::Device,
            ScannedKind::Unknown { error }  => Pending::Unknown { error: error.clone() },
        };

        // Don't clobber an existing Directory with another Directory.
        match (nodes.get(abs), &pending) {
            (Some(Pending::Directory), Pending::Directory) => {}
            _ => { nodes.insert(abs.clone(), pending); }
        }
    }

    let mut adjacency: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for path in nodes.keys() {
        if path == &root_path { continue; }
        if let Some(parent) = path.parent() {
            adjacency.entry(parent.to_path_buf()).or_default().push(path.clone());
        }
    }

    let tree = build_node(&root_path, &mut nodes, &adjacency);
    Ok(tree)
}
