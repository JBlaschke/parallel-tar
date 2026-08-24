// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pretty-printing for index trees (the `view-idx` tree view).

use crate::index::tree::{TreeNode, NodeType};

use std::sync::Arc;

/// Render a tree to stdout in `tree(1)`-style, one node per line with a
/// type icon, human-readable size, and truncated hash. Start a print with
/// `prefix = ""` and `is_last = true`.
pub trait Display {
    fn print_tree(self: &Arc<Self>, prefix: &str, is_last: bool);
    fn print_tree_depth(self: &Arc<Self>, prefix: &str, is_last: bool, max_depth: usize);
}

impl Display for TreeNode {
    /// Pretty print the tree with no depth limit (backward-compatible).
    fn print_tree(self: &Arc<Self>, prefix: &str, is_last: bool) {
        self.print_tree_depth(prefix, is_last, usize::MAX);
    }

    /// Pretty print the tree, descending at most `max_depth` levels below
    /// the current node. `max_depth == 0` prints only this node and
    /// suppresses its children entirely. If children exist but are
    /// hidden by the depth limit, an ellipsis row is shown so users
    /// know there's more underneath.
    fn print_tree_depth(self: &Arc<Self>, prefix: &str, is_last: bool, max_depth: usize) {
        let connector = if is_last { "└── " } else { "├── " };
        let icon: String = match & self.node_type {
            NodeType::File { .. }        => "📄".to_string(),
            NodeType::Directory { .. }   => "📁".to_string(),
            NodeType::Symlink { target } => format!(
                "🔗  {{{}}}", target.to_string_lossy().clone()
            ),
            NodeType::Socket { .. }     => "🔌".to_string(),
            NodeType::Fifo { .. }       => "🚰".to_string(),
            NodeType::Device { .. }     => "💾".to_string(),
            NodeType::Unknown { error } => format!(
                "❓ {{{}}}", error.to_string()
            ),
        };

        let size = self.read_metadata().unwrap_or_default().size;
        let hash = self.read_hash().unwrap_or_default();
        let info_str = format!("({}, {:.16})", format_size(size as u64), hash);

        println!("{}{}{} {} {}", prefix, connector, icon, self.name, info_str);

        if let NodeType::Directory { children } = &self.node_type {
            let new_prefix = format!(
                "{}{}", prefix, if is_last { "    " } else { "│   " }
            );

            if max_depth == 0 {
                // Children exist but we're at the depth cap — show an
                // ellipsis row so users know the listing is truncated.
                if !children.is_empty() {
                    println!("{}└── … ({} entries hidden)", new_prefix, children.len());
                }
                return;
            }

            for (i, child) in children.iter().enumerate() {
                child.print_tree_depth(
                    &new_prefix,
                    i == children.len() - 1,
                    max_depth - 1,
                );
            }
        }
    }
}

/// Format a byte count for humans: `"512 B"`, `"1.50 KB"`, `"2.00 MB"`,
/// `"1689.34 GB"` (binary multiples, two decimals).
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
