// SPDX-License-Identifier: AGPL-3.0-or-later
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use clap::{Arg, ArgAction, Command};
use log::info;

use ptar_lib::index::tree::{TreeNode, NodeType};
use ptar_lib::index::serialize::{DataFmt, load_tree, save_tree};
use ptar_lib::index::crypto::HashedNodes;

use ptar_lib::index::path::resolve_chain;

// ─── Tree rebuilding helpers ─────────────────────────────────────────────
//
// Children live inside NodeType::Directory by value, so changing them
// means constructing a new TreeNode for that directory. We only ever
// rebuild the nodes on the edit path; unchanged subtrees are shared via
// Arc — no deep clone, no extra hashing.

/// Build a fresh directory node with the given children list. Metadata
/// and hash are reset to None so the next compute_* pass refills them.
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

/// Replace the child named `child_name` under `parent` with `replacement`.
/// If `replacement` is None, the child is removed. Returns a new parent
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
// `resolve_chain` gives us [root, …, target] by Arc. After mutating the
// last element (or its parent's children list), we walk back up rebuilding
// each ancestor's children vector to swap in the rebuilt child. This is
// the standard "path copy" pattern for immutable persistent trees.

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
// When we splice in a subtree from another index, its nodes still carry
// the source tree's paths. Walk the subtree and rewrite every `path` to
// be `new_parent_path / (relative to subtree root)`. The hash and
// metadata fields are reset along the way so the parent recompute will
// fill them fresh.

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
        // Preserve the precomputed hash on leaves (it's content-based and
        // path-independent). Directory hashes are reset because they
        // depend on the children-concat algorithm and we're rebuilding.
        metadata:  RwLock::new(None),
        hash:      RwLock::new(match &node.node_type {
            NodeType::Directory { .. } => None,
            _ => node.read_hash(),
        }),
    })
}

// ─── Recompute pass ──────────────────────────────────────────────────────
//
// compute_metadata and compute_hashes both walk the tree and short-circuit
// on cached values. By resetting only the ancestor chain (which we did
// during rebuild_chain via rebuild_directory), the recompute only touches
// those ancestors plus their direct children — everything else uses cache.

fn finalize(tree: &Arc<TreeNode>, use_md5: bool) -> Result<(), Box<dyn Error>> {
    info!("Recomputing metadata ...");
    tree.compute_metadata()?;
    info!("Recomputing hashes ...");
    let root_hash = tree.compute_hashes(use_md5)?;
    info!("New root hash: '{}'", root_hash);
    Ok(())
}

// ─── Subcommands ─────────────────────────────────────────────────────────

fn cmd_rm(matches: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let input  = matches.get_one::<String>("input").unwrap();
    let output = matches.get_one::<PathBuf>("output").unwrap().clone();
    let target = matches.get_one::<String>("path").unwrap();
    let json   = matches.get_flag("json_fmt");
    let md5    = matches.get_flag("use_md5");

    let in_fmt = if json {
        DataFmt::Json(input.to_string())
    } else {
        DataFmt::Idx(input.to_string())
    };
    info!("Loading: {:?}", in_fmt);
    let tree = load_tree(in_fmt)?;

    let target_path = PathBuf::from(target);
    let chain = resolve_chain(&tree, &target_path)
        .map_err(|e| -> Box<dyn Error> { e.into() })?;

    info!("Removing {:?} (and its subtree) from index", target_path);
    let edit_index = chain.len() - 1;
    let new_root = rebuild_chain(&chain, edit_index, None)
        .map_err(|e| -> Box<dyn Error> { e.into() })?;

    finalize(&new_root, md5)?;

    let out_fmt = if json {
        DataFmt::Json(output.to_string_lossy().into_owned())
    } else {
        DataFmt::Idx(output.to_string_lossy().into_owned())
    };
    info!("Saving: {:?}", out_fmt);
    save_tree(&new_root, out_fmt)?;
    Ok(())
}

fn cmd_add(matches: &clap::ArgMatches) -> Result<(), Box<dyn Error>> {
    let input        = matches.get_one::<String>("input").unwrap();
    let output       = matches.get_one::<PathBuf>("output").unwrap().clone();
    let source       = matches.get_one::<String>("source").unwrap();
    let dest_parent  = matches.get_one::<String>("at").unwrap();
    let src_subpath  = matches.get_one::<String>("from");  // NEW: Option<&String>
    let json         = matches.get_flag("json_fmt");
    let md5          = matches.get_flag("use_md5");

    let in_fmt  = if json { DataFmt::Json(input.to_string()) }  else { DataFmt::Idx(input.to_string()) };
    let src_fmt = if json { DataFmt::Json(source.to_string()) } else { DataFmt::Idx(source.to_string()) };

    info!("Loading destination: {:?}", in_fmt);
    let dest_tree = load_tree(in_fmt)?;
    info!("Loading source:      {:?}", src_fmt);
    let src_tree  = load_tree(src_fmt)?;

    // NEW: if --from is given, resolve into the source tree to pick the
    // subtree we actually want to insert. Otherwise use the source root.
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

    // Resolve the *parent* under which we'll insert. The chosen source
    // node is inserted as a child of this parent, using src_node.name.
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

    finalize(&new_root, md5)?;

    let out_fmt = if json {
        DataFmt::Json(output.to_string_lossy().into_owned())
    } else {
        DataFmt::Idx(output.to_string_lossy().into_owned())
    };
    info!("Saving: {:?}", out_fmt);
    save_tree(&new_root, out_fmt)?;
    Ok(())
}

// ─── main ────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let common_args = || vec![
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
        Arg::new("use_md5")
            .long("md5")
            .help("Use MD5 instead of SHA-256 when recomputing hashes")
            .action(ArgAction::SetTrue),
    ];

    let cli = Command::new("Index edit tool for Parallel Tar")
        .version("1.0")
        .author("Johannes Blaschke")
        .about("Remove or insert subtrees in a .idx / .etr index file.")
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("rm")
                .about("Remove a path (and everything under it) from the index")
                .args(common_args())
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
                .args(common_args())
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
        .get_matches();

    match cli.subcommand() {
        Some(("rm",  m)) => cmd_rm(m),
        Some(("add", m)) => cmd_add(m),
        _ => unreachable!("subcommand_required is set"),
    }
}
