// SPDX-License-Identifier: AGPL-3.0-or-later

//! `diff-idx` — side-by-side, one-level-deep comparison of two index files.
//!
//! Children are aligned by name; hashes decide `==` vs `!=`, and one-sided
//! entries are marked `<` / `>`. Use `-p` to drill into whichever differing
//! subtree needs investigating next.

use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Arg, ArgAction, Command};

use ptar_lib::index::tree::{TreeNode, NodeType};
use ptar_lib::index::serialize::{DataFmt, load_tree};
use ptar_lib::index::display::format_size;

use ptar_lib::index::path::resolve;

// ─── ANSI helpers ────────────────────────────────────────────────────────
//
// Raw escape codes; no new crate. We only color the hash text, per spec.
// If stdout isn't a tty the codes still print fine into a pipe — most
// modern pagers (less -R) handle them; redirect to a file and they show
// as literal bytes, which is the standard tradeoff for no-dep coloring.

const C_RED:   &str = "\x1b[31m";
const C_GREEN: &str = "\x1b[32m";
const C_RESET: &str = "\x1b[0m";

fn paint(text: &str, color: &str) -> String {
    format!("{}{}{}", color, text, C_RESET)
}

// ─── Row rendering (single level) ────────────────────────────────────────
//
// Each side of the diff is a column of fixed width. A row is one entry
// from one side (left, right, or both). The icon comes from the node's
// NodeType, matching the viewer's print_tree style.

fn icon_for(node: &TreeNode) -> String {
    match &node.node_type {
        NodeType::File { .. }      => "📄".to_string(),
        NodeType::Directory { .. } => "📁".to_string(),
        NodeType::Symlink { target } => {
            format!("🔗 {{{}}}", target.to_string_lossy())
        }
        NodeType::Socket {}        => "🔌".to_string(),
        NodeType::Fifo {}          => "🚰".to_string(),
        NodeType::Device {}        => "💾".to_string(),
        NodeType::Unknown { error } => format!("❓ {{{}}}", error),
    }
}

/// Renders the cell text for one side, *without* color codes. We keep
/// the size+hash slot fixed-width so columns line up across rows, then
/// color-wrap the hash slice at the end.
///
/// Layout (column width = `width`):
///
/// ```text
///     "<icon> <name> (<size>, <hash16>)"
/// ```
///
/// Truncated/padded to `width` chars. Hash is always the last 16+2 chars
/// before the closing paren, so we can locate it for coloring.
fn format_cell(node: Option<&Arc<TreeNode>>, width: usize) -> (String, Option<(usize, usize)>) {
    match node {
        None => (" ".repeat(width), None),
        Some(n) => {
            let size = n.read_metadata().unwrap_or_default().size;
            let hash = n.read_hash().unwrap_or_default();
            let hash16 = format!("{:.16}", hash);
            let info = format!("({}, {})", format_size(size as u64), hash16);
            let head = format!("{} {} ", icon_for(n), n.name);

            // Build the raw cell, padded/truncated to `width`.
            let mut raw = format!("{}{}", head, info);
            // Locate the hash within `raw` *before* padding — these
            // byte offsets stay valid because padding is appended.
            let hash_start = raw.rfind(&hash16);
            let hash_end   = hash_start.map(|s| s + hash16.len());

            // Truncate or pad. Use char-aware truncation to avoid
            // splitting multi-byte sequences (the icons are multi-byte).
            let char_len = raw.chars().count();
            if char_len > width {
                // Truncate to width, but if the hash extends past
                // truncation we lose color anchors — accept that.
                raw = raw.chars().take(width).collect();
                let raw_byte_len = raw.len();
                let anchors = match (hash_start, hash_end) {
                    (Some(s), Some(e)) if e <= raw_byte_len => Some((s, e)),
                    _ => None,
                };
                return (raw, anchors);
            } else {
                let pad = width - char_len;
                raw.push_str(&" ".repeat(pad));
            }

            let anchors = match (hash_start, hash_end) {
                (Some(s), Some(e)) => Some((s, e)),
                _ => None,
            };
            (raw, anchors)
        }
    }
}

/// Apply color to the hash byte range within a cell string.
fn colorize_hash(cell: String, anchors: Option<(usize, usize)>, color: &str) -> String {
    match anchors {
        None => cell,
        Some((s, e)) => {
            let mut out = String::with_capacity(cell.len() + 16);
            out.push_str(&cell[..s]);
            out.push_str(&paint(&cell[s..e], color));
            out.push_str(&cell[e..]);
            out
        }
    }
}

// ─── The diff ────────────────────────────────────────────────────────────

/// Compare two nodes by hash. None means at least one side has no hash
/// computed (shouldn't happen for valid .idx files; treat as "differ").
fn hashes_match(a: &Arc<TreeNode>, b: &Arc<TreeNode>) -> bool {
    match (a.read_hash(), b.read_hash()) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

fn marker(left: Option<&Arc<TreeNode>>, right: Option<&Arc<TreeNode>>) -> &'static str {
    match (left, right) {
        (Some(l), Some(r)) => if hashes_match(l, r) { " == " } else { " != " },
        (Some(_), None)    => " <  ",
        (None,    Some(_)) => "  > ",
        (None,    None)    => "    ",
    }
}

/// Print a single side-by-side row.
fn print_row(
    left:  Option<&Arc<TreeNode>>,
    right: Option<&Arc<TreeNode>>,
    width: usize,
) {
    let (lc, la) = format_cell(left,  width);
    let (rc, ra) = format_cell(right, width);
    let same = match (left, right) {
        (Some(l), Some(r)) => hashes_match(l, r),
        _ => false,
    };
    let color = if same { C_GREEN } else { C_RED };

    // Only color hashes that actually exist (i.e. side is present).
    let lc = if left.is_some()  { colorize_hash(lc, la, color) } else { lc };
    let rc = if right.is_some() { colorize_hash(rc, ra, color) } else { rc };

    println!("{} {} {}", lc, marker(left, right), rc);
}

/// Diff one level: the node itself, then aligned children.
///
/// Children are aligned by name via a sorted merge — both children
/// lists are already sorted by name (per `fs.rs::from_path`), so a
/// linear two-pointer merge is sufficient.
fn diff_one_level(
    left:  &Arc<TreeNode>,
    right: &Arc<TreeNode>,
    width: usize,
) {
    // Header rule and the parent row.
    println!("{}", "─".repeat(width * 2 + 5));
    print_row(Some(left), Some(right), width);
    println!("{}", "─".repeat(width * 2 + 5));

    let lc = left.children();
    let rc = right.children();
    let (mut i, mut j) = (0usize, 0usize);

    while i < lc.len() || j < rc.len() {
        match (lc.get(i), rc.get(j)) {
            (Some(l), Some(r)) => {
                match l.name.cmp(&r.name) {
                    std::cmp::Ordering::Equal => {
                        print_row(Some(l), Some(r), width);
                        i += 1; j += 1;
                    }
                    std::cmp::Ordering::Less => {
                        // l has no counterpart on the right yet.
                        print_row(Some(l), None, width);
                        i += 1;
                    }
                    std::cmp::Ordering::Greater => {
                        print_row(None, Some(r), width);
                        j += 1;
                    }
                }
            }
            (Some(l), None) => { print_row(Some(l), None, width); i += 1; }
            (None, Some(r)) => { print_row(None, Some(r), width); j += 1; }
            (None, None) => unreachable!(),
        }
    }
}

// ─── main ────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn Error>> {
    let args = Command::new("Index diff tool for Parallel Tar")
        .version("1.0")
        .author("Johannes Blaschke")
        .about("Side-by-side diff of two .idx (or .json) index files.")
        .arg(
            Arg::new("left")
                .value_name("LEFT")
                .help("Left-hand index file")
                .required(true)
                .index(1),
        )
        .arg(
            Arg::new("right")
                .value_name("RIGHT")
                .help("Right-hand index file")
                .required(true)
                .index(2),
        )
        .arg(
            Arg::new("path")
                .short('p')
                .long("path")
                .value_name("PATH")
                .help("Sub-path within both trees to diff (matched by child name)")
                .required(false),
        )
        .arg(
            Arg::new("json_fmt")
                .short('j')
                .long("json")
                .help("Input files are JSON, not msgpack-idx")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("width")
                .short('w')
                .long("width")
                .value_name("CHARS")
                .help("Width of each side column in characters (default 40)")
                .value_parser(clap::value_parser!(usize))
                .required(false),
        )
        .get_matches();

    let left_path:  &String = args.get_one::<String>("left").unwrap();
    let right_path: &String = args.get_one::<String>("right").unwrap();
    let json_fmt:    bool   = args.get_flag("json_fmt");
    let width:       usize  = args.get_one::<usize>("width").copied().unwrap_or(40);

    let mk_fmt = |p: &str| -> DataFmt {
        if json_fmt { DataFmt::Json(p.to_string()) } else { DataFmt::Idx(p.to_string()) }
    };

    println!("Loading LEFT:  {:?}", mk_fmt(left_path));
    let left_tree  = load_tree(mk_fmt(left_path))?;
    println!("Loading RIGHT: {:?}", mk_fmt(right_path));
    let right_tree = load_tree(mk_fmt(right_path))?;

    // Resolve sub-path if given.
    let (left_node, right_node) = match args.get_one::<String>("path") {
        None => (left_tree.clone(), right_tree.clone()),
        Some(p) => {
            let rel = PathBuf::from(p);
            let l = resolve(&left_tree, &rel).ok_or_else(|| {
                format!("path {:?} not found in LEFT tree", rel)
            })?;
            let r = resolve(&right_tree, &rel).ok_or_else(|| {
                format!("path {:?} not found in RIGHT tree", rel)
            })?;
            (l, r)
        }
    };

    diff_one_level(&left_node, &right_node, width);

    // Summary
    let same = hashes_match(&left_node, &right_node);
    println!();
    if same {
        println!("Subtrees match: {}", paint("OK", C_GREEN));
    } else {
        println!("Subtrees differ: {}", paint("MISMATCH", C_RED));
    }

    Ok(())
}
