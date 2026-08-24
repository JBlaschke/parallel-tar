// SPDX-License-Identifier: AGPL-3.0-or-later

//! `edit-idx` — structurally edit index files: `init` an empty stub, `rm` a
//! subtree, `add` (splice in) a subtree from another index, and `finalize`
//! to re-aggregate metadata and directory hashes.
//!
//! The structural operations never hash or aggregate; they mark the edit
//! path stale (metadata/hash = `None`) and share all unchanged subtrees via
//! `Arc` (the "path copy" pattern), so repeated edits stay cheap and the
//! cost of aggregation is paid once, in `finalize`.

use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use clap::{Arg, ArgAction, Command};
use log::{info, warn};

use ptar_lib::index::tree::{TreeNode, NodeType};
use ptar_lib::index::serialize::{DataFmt, load_tree, save_tree};
use ptar_lib::index::crypto::HashedNodes;

use ptar_lib::index::path::resolve_chain;

// ─── Tree rebuilding helpers ─────────────────────────────────────────────
//
// Children live inside NodeType::Directory by value, so changing them means
// constructing a new TreeNode for that directory. We only ever rebuild the
// nodes on the edit path; unchanged subtrees are shared via Arc — no deep
// clone, no extra hashing.
//
// IMPORTANT: the structural operations (init / add / rm) NEVER compute metadata
// or hashes. They invalidate the edit-path nodes (set metadata and hash to
// None) and leave aggregation to the `finalize` subcommand (in-memory, no file
// I/O) or to `parallel-idx -t` (reads files). This keeps repeated compositions
// cheap — you pay for aggregation once, at the end, rather than after every
// edit.

/// Build a fresh directory node with the given children list. Metadata and hash
/// are reset to None — this marks the node as "stale", to be refilled by a
/// later `finalize` / `parallel-idx -t` pass.
fn rebuild_directory(
    template: &TreeNode,
    new_children: Vec<Arc<TreeNode>>,
) -> Arc<TreeNode> {
    Arc::new(TreeNode {
        name:      template.name.clone(),
        path:      template.path.clone(),
        node_type: NodeType::Directory { children: new_children },
        metadata:  RwLock::new(None),
        hash:      RwLock::new(None),
    })
}

/// Replace the child named `child_name` under `parent` with `replacement`. If
/// `replacement` is None, the child is removed. Returns a new parent
/// `Arc<TreeNode>` with the updated children list (kept sorted by name).
fn replace_child(
    parent:    &TreeNode,
    child_name: &str,
    replacement: Option<Arc<TreeNode>>,
) -> Result<Arc<TreeNode>, String> {
    let children = match &parent.node_type {
        NodeType::Directory { children } => children.clone(),
        _ => return Err(format!(
            "{:?} is not a directory; cannot edit children", parent.path
        )),
    };

    let mut new_children: Vec<Arc<TreeNode>> = children
        .into_iter()
        .filter(|c| c.name != child_name)
        .collect();

    if let Some(repl) = replacement {
        new_children.push(repl);
        new_children.sort_by(|a, b| a.name.cmp(&b.name));
    }

    Ok(rebuild_directory(parent, new_children))
}

/// Insert `new_child` under `parent`. Errors if a child of the same name
/// already exists (per the "source root must be inserted whole" rule).
fn insert_child(
    parent:    &TreeNode,
    new_child: Arc<TreeNode>,
) -> Result<Arc<TreeNode>, String> {
    let children = match &parent.node_type {
        NodeType::Directory { children } => children.clone(),
        _ => return Err(format!(
            "{:?} is not a directory; cannot insert child", parent.path
        )),
    };

    if children.iter().any(|c| c.name == new_child.name) {
        return Err(format!(
            "destination {:?} already has a child named {:?} \
             (source root must be inserted whole; remove first or rename)",
            parent.path, new_child.name
        ));
    }

    let mut new_children = children;
    new_children.push(new_child);
    new_children.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(rebuild_directory(parent, new_children))
}

// ─── Rebuilding the chain from the edit point back to the root ───────────
//
// `resolve_chain` gives us [root, …, target] by Arc. After mutating the last
// element (or its parent's children list), we walk back up rebuilding each
// ancestor's children vector to swap in the rebuilt child. This is the standard
// "path copy" pattern for immutable persistent trees. Every rebuilt ancestor
// gets metadata/hash reset to None (via rebuild_directory).

/// Given the resolved chain [root, …, parent_of_edit, edit_target] and
/// the new version of `edit_target` (or None for removal), rebuild every
/// ancestor up to the root. Returns the new root.
fn rebuild_chain(
    chain:       &[Arc<TreeNode>],
    edit_index:  usize,           // position in chain that was modified
    new_node:    Option<Arc<TreeNode>>,  // None == remove
) -> Result<Arc<TreeNode>, String> {
    if edit_index == 0 {
        // The edit was at the root itself; can't remove the root.
        match new_node {
            Some(n) => return Ok(n),
            None    => return Err("cannot remove the tree root".to_string()),
        }
    }

    // Start with the modified node and walk up.
    let mut current_replacement = new_node;
    let mut cursor = edit_index;

    while cursor > 0 {
        let parent = &chain[cursor - 1];
        let old_child_name = &chain[cursor].name;
        let rebuilt_parent = replace_child(
            parent,
            old_child_name,
            current_replacement.take(),
        )?;
        current_replacement = Some(rebuilt_parent);
        cursor -= 1;
    }

    Ok(current_replacement.expect("chain rebuild lost the root"))
}

// ─── Path rewriting under an inserted subtree ────────────────────────────
//
// When we splice in a subtree from another index, its nodes still carry the
// source tree's paths. Walk the subtree and rewrite every `path` to be
// `new_parent_path / (relative to subtree root)`. File-leaf hashes are
// PRESERVED (they're content-based and path-independent); directory hashes and
// all metadata are reset, to be refilled by `finalize`.

fn rewrite_subtree_paths(
    node:      &Arc<TreeNode>,
    new_path:  PathBuf,
) -> Arc<TreeNode> {
    let node_type = match &node.node_type {
        NodeType::Directory { children } => {
            let new_kids: Vec<Arc<TreeNode>> = children
                .iter()
                .map(|c| rewrite_subtree_paths(c, new_path.join(&c.name)))
                .collect();
            NodeType::Directory { children: new_kids }
        }
        // Leaf types: clone the node_type via a fresh match.
        NodeType::File { size }           => NodeType::File { size: *size },
        NodeType::Symlink { target }      => NodeType::Symlink { target: target.clone() },
        NodeType::Socket {}               => NodeType::Socket {},
        NodeType::Fifo {}                 => NodeType::Fifo {},
        NodeType::Device {}               => NodeType::Device {},
        NodeType::Unknown { error }       => NodeType::Unknown { error: error.clone() },
    };

    Arc::new(TreeNode {
        name:      node.name.clone(),
        path:      new_path,
        node_type,
        // Metadata always reset (it's aggregated, so stale after a move).
        metadata:  RwLock::new(None),
        // Preserve file-leaf hashes (content-based, path-independent); reset
        // directory hashes (depend on children-concat, recomputed by finalize).
        hash:      RwLock::new(match &node.node_type {
            NodeType::Directory { .. } => None,
            _ => node.read_hash(),
        }),
    })
}

// ─── Shared utilities ────────────────────────────────────────────────────

/// Count file nodes that lack a hash. Used by `finalize` to report how much
/// work `parallel-idx -t` would still need to do.
fn count_unhashed_files(node: &Arc<TreeNode>) -> usize {
    node.iter_depth_first()
        .filter(|n| matches!(n.node_type, NodeType::File { .. }) && n.read_hash().is_none())
        .count()
}

/// Build a DataFmt from a path string and the JSON flag.
fn fmt_for(path: &str, json: bool) -> DataFmt {
    if json {
        DataFmt::Json(path.to_string())
    } else {
        DataFmt::Idx(path.to_string())
    }
}

// ─── Subcommands: structural (cheap, no metadata/hash computation) ───────

fn cmd_init(matches: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let output = matches.get_one::<PathBuf>("output").unwrap().clone();
    let root   = matches.get_one::<PathBuf>("root");           // Option<&PathBuf>
    let name   = matches.get_one::<String>("name").unwrap().clone();
    let json   = matches.get_flag("json_fmt");

    // Match the convention used by `parallel-idx` on the live filesystem:
    //  - absolute parent given → stub.path = parent.join(name) (full path)
    //  - no parent given       → stub.path = name (relative; matches relative-input idx)
    let stub_path = match root {
        Some(r) => r.join(&name),
        None    => PathBuf::from(&name),
    };

    info!("Creating empty stub: name={:?}, path={:?}", name, stub_path);

    // Empty directory, no metadata, no hash. `finalize` (or parallel-idx -t)
    // will populate these. For an empty stub the aggregation is trivial.
    let stub = Arc::new(TreeNode {
        name,
        path: stub_path,
        node_type: NodeType::Directory { children: Vec::new() },
        metadata:  RwLock::new(None),
        hash:      RwLock::new(None),
    });

    let out_fmt = fmt_for(&output.to_string_lossy(), json);
    info!("Saving: {:?}", out_fmt);
    save_tree(&stub, out_fmt)?;
    Ok(())
}

fn cmd_rm(matches: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let input  = matches.get_one::<String>("input").unwrap();
    let output = matches.get_one::<PathBuf>("output").unwrap().clone();
    let target = matches.get_one::<String>("path").unwrap();
    let json   = matches.get_flag("json_fmt");

    let in_fmt = fmt_for(input, json);
    info!("Loading: {:?}", in_fmt);
    let tree = load_tree(in_fmt)?;

    let target_path = PathBuf::from(target);
    let chain = resolve_chain(&tree, &target_path)
        .map_err(|e| -> Box<dyn Error> { e.into() })?;

    info!("Removing {:?} (and its subtree) from index", target_path);
    let edit_index = chain.len() - 1;
    let new_root = rebuild_chain(&chain, edit_index, None)
        .map_err(|e| -> Box<dyn Error> { e.into() })?;

    // No aggregation here — ancestors of the removal are now stale (None). Run
    // `edit-idx finalize` (or `parallel-idx -t`) to make the tree valid.
    info!("Done. Tree has stale ancestors; run 'edit-idx finalize' to aggregate.");

    let out_fmt = fmt_for(&output.to_string_lossy(), json);
    info!("Saving: {:?}", out_fmt);
    save_tree(&new_root, out_fmt)?;
    Ok(())
}

fn cmd_add(matches: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let input        = matches.get_one::<String>("input").unwrap();
    let output       = matches.get_one::<PathBuf>("output").unwrap().clone();
    let source       = matches.get_one::<String>("source").unwrap();
    let dest_parent  = matches.get_one::<String>("at").unwrap();
    let src_subpath  = matches.get_one::<String>("from");  // Option<&String>
    let json         = matches.get_flag("json_fmt");

    let in_fmt  = fmt_for(input, json);
    let src_fmt = fmt_for(source, json);

    info!("Loading destination: {:?}", in_fmt);
    let dest_tree = load_tree(in_fmt)?;
    info!("Loading source:      {:?}", src_fmt);
    let src_tree  = load_tree(src_fmt)?;

    // If --from is given, resolve into the source tree to pick the subtree we
    // actually want to insert. Otherwise use the source root.
    let src_node = match src_subpath {
        None => Arc::clone(&src_tree),
        Some(p) => {
            let chain = resolve_chain(&src_tree, &PathBuf::from(p))
                .map_err(|e| -> Box<dyn Error> {
                    format!("--from: {}", e).into()
                })?;
            chain.last().unwrap().clone()
        }
    };

    // Resolve the *parent* under which we'll insert. The chosen source node is
    // inserted as a child of this parent, using src_node.name.
    let dest_path = PathBuf::from(dest_parent);
    let chain = resolve_chain(&dest_tree, &dest_path)
        .map_err(|e| -> Box<dyn Error> {
            format!("--at: {}", e).into()
        })?;
    let parent = chain.last().unwrap().clone();

    // Rewrite paths under the source so they live at parent.path / src_node.name / ...
    let new_subtree_root_path = parent.path.join(&src_node.name);
    info!(
        "Inserting subtree {:?} under {:?} (new path: {:?})",
        src_node.name, parent.path, new_subtree_root_path
    );
    let relocated = rewrite_subtree_paths(&src_node, new_subtree_root_path);

    let new_parent = insert_child(&parent, relocated)
        .map_err(|e| -> Box<dyn Error> { e.into() })?;

    let edit_index = chain.len() - 1;
    let new_root = rebuild_chain(&chain, edit_index, Some(new_parent))
        .map_err(|e| -> Box<dyn Error> { e.into() })?;

    // No aggregation here — the inserted subtree keeps its file hashes, but
    // directory hashes along the edit path are stale (None). Run `edit-idx
    // finalize` (or `parallel-idx -t`) to make the tree valid.
    info!("Done. Tree has stale ancestors; run 'edit-idx finalize' to aggregate.");

    let out_fmt = fmt_for(&output.to_string_lossy(), json);
    info!("Saving: {:?}", out_fmt);
    save_tree(&new_root, out_fmt)?;
    Ok(())
}

// ─── Subcommand: finalize (in-memory aggregation, no file I/O) ───────────

fn cmd_finalize(matches: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let input  = matches.get_one::<String>("input").unwrap();
    let output = matches.get_one::<PathBuf>("output").unwrap().clone();
    let json   = matches.get_flag("json_fmt");
    let md5    = matches.get_flag("use_md5");

    let in_fmt = fmt_for(input, json);
    info!("Loading: {:?}", in_fmt);
    let tree = load_tree(in_fmt)?;

    // Aggregate metadata bottom-up (size, file/dir counts). Pure in-memory
    // arithmetic over the in-tree NodeType::File { size } fields — no disk.
    info!("Aggregating metadata ...");
    tree.compute_metadata()?;

    // Aggregate directory hashes from children (children-concat algorithm). A
    // directory's hash is Some(..) iff every descendant file has a hash;
    // otherwise it (and every ancestor) is None. This NEVER reads files —
    // missing file hashes propagate None upward rather than triggering I/O.
    //
    // NOTE: --md5 must match the algorithm the *file* hashes were computed
    // with. The directory concat is hashed with this algorithm, so mixing (e.g.
    // SHA-256 file hashes finalized with --md5) yields a root hash that won't
    // match a consistent parallel-idx run.
    info!("Aggregating directory hashes ...");
    match tree.compute_hashes(md5)? {
        Some(h) => info!("Root hash: '{}'", h),
        None    => {
            let n = count_unhashed_files(&tree);
            warn!(
                "Root hash incomplete: {} file(s) lack hashes. \
                 Run 'parallel-idx -t' to compute them.", n
            );
        }
    }

    let out_fmt = fmt_for(&output.to_string_lossy(), json);
    info!("Saving: {:?}", out_fmt);
    save_tree(&tree, out_fmt)?;
    Ok(())
}

// ─── main ────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    // Structural ops (init/add/rm) don't hash, so they don't take --md5. Shared
    // args for the structural ops that read+write an index.
    let io_args = || vec![
        Arg::new("input")
            .short('f').long("file")
            .value_name("INDEX")
            .help("Input index file")
            .required(true),
        Arg::new("output")
            .short('o').long("out")
            .value_name("OUTPUT")
            .help("Output index file (required)")
            .value_parser(clap::value_parser!(PathBuf))
            .required(true),
        Arg::new("json_fmt")
            .short('j').long("json")
            .help("Files are JSON, not msgpack-idx")
            .action(ArgAction::SetTrue),
    ];

    let cli = Command::new("Index edit tool for Parallel Tar")
        .version("1.0")
        .author("Johannes Blaschke")
        .about(
            "Structurally edit .idx / .etr index files. The add/rm/init \
             operations are cheap and never compute hashes or metadata; run \
             'finalize' afterwards to aggregate (in-memory), or 'parallel-idx \
             -t' to also compute missing file hashes."
        )
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("init")
                .about("Create an empty stub index that can be populated via 'add'")
                .arg(
                    Arg::new("output")
                        .short('o').long("out")
                        .value_name("OUTPUT")
                        .help("Output index file (required)")
                        .value_parser(clap::value_parser!(PathBuf))
                        .required(true),
                )
                .arg(
                    Arg::new("root")
                        .short('r').long("root")
                        .value_name("PATH")
                        .help("Optional filesystem parent path where the named directory lives")
                        .value_parser(clap::value_parser!(PathBuf)),
                )
                .arg(
                    Arg::new("name")
                        .short('n').long("name")
                        .value_name("NAME")
                        .help("Name of the top-level directory (becomes the tree root name)")
                        .required(true),
                )
                .arg(
                    Arg::new("json_fmt")
                        .short('j').long("json")
                        .help("Write output as JSON")
                        .action(ArgAction::SetTrue),
                ),
        )
        .subcommand(
            Command::new("rm")
                .about("Remove a path (and everything under it) from the index")
                .args(io_args())
                .arg(
                    Arg::new("path")
                        .short('p').long("path")
                        .value_name("PATH")
                        .help("Sub-path to remove (matched by child name)")
                        .required(true),
                ),
        )
        .subcommand(
            Command::new("add")
                .about("Insert a source index as a subtree of the destination index")
                .args(io_args())
                .arg(
                    Arg::new("source")
                        .short('s').long("source")
                        .value_name("SOURCE")
                        .help("Source index file (its root becomes the new child)")
                        .required(true),
                )
                .arg(
                    Arg::new("at")
                        .long("at")
                        .value_name("PATH")
                        .help("Destination parent path under which to insert (matched by child name)")
                        .required(true),
                )
                .arg(
                    Arg::new("from")
                        .long("from")
                        .value_name("PATH")
                        .help("Sub-path within the source index to insert (matched by child name; defaults to the source root)")
                        .required(false),
                ),
        )
        .subcommand(
            Command::new("finalize")
                .about(
                    "Aggregate metadata and directory hashes in-memory (no file \
                     I/O). Use after a series of add/rm/init operations to produce \
                     a valid tree. Files lacking hashes leave their ancestors \
                     unhashed; use 'parallel-idx -t' to compute file hashes."
                )
                .args(io_args())
                .arg(
                    Arg::new("use_md5")
                        .short('m').long("md5")
                        .help("Hash directory concats with MD5 (must match the algorithm the file hashes were computed with)")
                        .action(ArgAction::SetTrue),
                ),
        )
        .get_matches();

    match cli.subcommand() {
        Some(("init",     m)) => cmd_init(m),
        Some(("rm",       m)) => cmd_rm(m),
        Some(("add",      m)) => cmd_add(m),
        Some(("finalize", m)) => cmd_finalize(m),
        _ => unreachable!("subcommand_required is set"),
    }
}
