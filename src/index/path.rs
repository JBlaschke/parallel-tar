// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolve user-supplied sub-paths within an index tree.
//!
//! Resolution walks [`TreeNode::name`] segments, *not* the stored
//! `TreeNode::path` field — so `data/LCLS` matches whether the index was
//! built from `/global/projects/data/LCLS` or just `data/LCLS`. This is
//! the convention shared by `view-idx -p`, `diff-idx -p`, and the
//! `edit-idx` path arguments.

use crate::index::tree::TreeNode;

use std::path::{Component, Path};
use std::sync::Arc;

// ─── Path resolution by child name ───────────────────────────────────────
//
// Same convention as diff-idx: walk TreeNode.name segments, not the stored
// path field. Returns the chain of nodes from root to target (inclusive)
// so callers can walk back up for invalidation.

/// Walk a tree by child `name` segments. Returns the full chain
/// `[root, …, target]` on success. Skips `./`, absolute prefixes, and
/// the root-dir component; rejects `..`.
///
/// This is the canonical path-resolution routine for index tooling
/// (diff-idx, edit-idx, etc.) — it works uniformly whether the index
/// was built with absolute or relative paths, because it matches on
/// `TreeNode.name` rather than the stored `TreeNode.path`.
pub fn resolve_chain(
    root: &Arc<TreeNode>,
    rel:  &Path,
) -> Result<Vec<Arc<TreeNode>>, String> {
    let mut chain = vec![Arc::clone(root)];
    for component in rel.components() {
        let name = match component {
            Component::Normal(s) => s.to_string_lossy().into_owned(),
            Component::CurDir => continue,
            Component::RootDir | Component::Prefix(_) => continue,
            Component::ParentDir => return Err(format!(
                "'..' is not allowed in path arguments: {:?}", rel
            )),
        };
        let cur = chain.last().unwrap();
        let next = cur.children().iter().find(|c| c.name == name).cloned();
        match next {
            Some(n) => chain.push(n),
            None    => return Err(format!(
                "path component {:?} not found under {:?}", name, cur.path
            )),
        }
    }
    Ok(chain)
}

// ─── Path resolution ─────────────────────────────────────────────────────
//
// We resolve the user-supplied "path/to/child" against both trees by
// walking child names. This is independent of whether the .idx stored
// absolute or relative paths, which is the right behavior for diffing
// indexes built in different contexts (e.g. a live-tree .idx vs a
// verify .idx without --root).

/// Convenience: resolve and return just the target node. Returns `None`
/// if any component is missing or invalid (errors are swallowed — use
/// `resolve_chain` directly if you need diagnostics).
pub fn resolve(
    root: &Arc<TreeNode>,
    rel:  &Path,
) -> Option<Arc<TreeNode>> {
    resolve_chain(root, rel).ok().and_then(|c| c.last().cloned())
}
