// SPDX-License-Identifier: AGPL-3.0-or-later

//! Lexical path analysis and sanitization.
//!
//! [`analyze_path`] decides where the archiver should `chdir` before adding
//! entries (absolute inputs are archived from their parent directory);
//! [`sanitize_rel_path`] is the path-traversal defense applied to every tar
//! entry during extract/verify; and [`DirPlan`] records directory permission
//! fix-ups that must be deferred until extraction is complete (a read-only
//! directory can't have files created inside it).

use std::io;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::collections::HashMap;
// Logging
use log::{debug};

/// Analyze an input path string.
///
/// Returns (base_dir, path_part):
///
/// 1) If `input` is absolute:
///    - base_dir = Some(parent_of_input)
///    - path_part = last path component (the "lowest possible relative path")
///
/// 2) If `input` is relative AND (after resolving `.`/`..` against the current
///    dir) it stays within the current directory:
///    - base_dir = None
///    - path_part = relative path from current dir
///
/// 3) If `input` is relative BUT it escapes the current directory:
///    - resolve to an absolute path and treat it like case (1)
///
/// Notes:
/// - This is a *lexical* normalization (it does NOT resolve symlinks and does
///   not touch the filesystem). If you need symlink-aware behavior, use
///   `std::fs::canonicalize` (but it requires paths to exist).
pub fn analyze_path(input: &str) -> io::Result<(Option<PathBuf>, PathBuf)> {
    let s = input.trim();
    let p = Path::new(s);

    // Absolute input: split into (parent, leaf)
    if p.is_absolute() {
        let abs = lexical_normalize(p);
        return Ok(split_absolute(&abs));
    }

    // Relative input: resolve against CWD
    let cwd = lexical_normalize(&std::env::current_dir()?);
    let abs = lexical_normalize(&cwd.join(p));

    // If the resolved absolute stays within CWD, keep it "workspace-relative":
    // return None and "<relative-from-cwd>"
    if abs.starts_with(&cwd) {
        // starts_with => strip_prefix should succeed
        let rel_from_cwd = abs.strip_prefix(&cwd)
            .unwrap_or_else(|_| Path::new(""));
        return Ok((None, rel_from_cwd.to_path_buf()));
    }

    // Otherwise it escaped the current folder: treat as absolute-style
    Ok(split_absolute(&abs))
}

/// Split an absolute path into (Some(parent), leaf_name). If the path has no
/// leaf (e.g. "/" or "C:\\"), leaf becomes ".".
fn split_absolute(abs: &Path) -> (Option<PathBuf>, PathBuf) {
    let parent = abs
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| abs.to_path_buf());

    let leaf = abs
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    (Some(parent), leaf)
}

/// Lexically normalizes a path by removing `.` and resolving `..` where
/// possible. This does NOT access the filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();

    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // pop() stops at root/prefix for absolute paths If nothing to
                // pop and original path is relative, keep leading ".."
                if !out.pop() && !path.is_absolute() {
                    out.push("..");
                }
            }
            Component::Normal(c) => out.push(c),
            // Preserve platform-specific prefix/root (Windows drive letters,
            // UNC, etc.)
            Component::RootDir | Component::Prefix(_) => out.push(
                comp.as_os_str()
            )
        }
    }

    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// Final permissions for one directory, as recorded from a tar header.
///
/// On Unix this is the full mode; on Windows only a read-only flag can be
/// expressed. `priority` breaks ties deterministically when several tar
/// shards carry an entry for the same directory.
#[derive(Clone, Copy, Debug)]
pub struct DirMode {
    #[cfg(unix)]
    mode: u32,
    #[cfg(windows)]
    readonly: bool,
    // deterministic conflict resolution if multiple tars set the same dir
    priority: usize,
}

/// Deferred directory-permission plan built up during extraction.
///
/// Directories whose archived mode is not writable are left writable while
/// files are extracted into them; their restrictive modes are registered here
/// (via [`set_chmod_plan`]) and applied in one deepest-first pass at the end
/// (via [`apply_chmod_plan`]).
#[derive(Default, Debug)]
pub struct DirPlan {
    // Original mode for dirs we temporarily chmod'd to be writable.
    original: HashMap<PathBuf, DirMode>,
    pub dir_lock: HashMap<PathBuf, bool>
}

/// Sanitize like tar::Entry::unpack_in does:
/// - ignores absolute/root components
/// - rejects any ".."
pub fn sanitize_rel_path(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => continue,
            Component::ParentDir => return None,
            Component::Normal(p) => out.push(p),
        }
    }
    if out.as_os_str().is_empty() { None } else { Some(out) }
}

/// Register `dir`'s archived `mode` in the [`DirPlan`] if it is too
/// restrictive to extract into (missing write or execute for the owner).
/// A higher-`priority` registration for the same directory wins.
#[cfg(unix)]
pub fn set_chmod_plan(
            plan: &mut DirPlan, dir: &Path, mode: u32, priority: usize
        ) -> io::Result<()> {

    // Need write+execute on a directory to create entries inside it.
    let need_bits = 0o300;
    if (mode & need_bits) != need_bits {
        match plan.original.get(&dir.to_path_buf()) {
            Some(existing) if existing.priority > priority => {
                debug!(
                    "Destination path '{}' already processed: '{}', '{}'",
                    dir.to_string_lossy(), existing.mode, mode
                );
                return Ok(());
            }
            _ => {
                debug!(
                    "Registering mode for '{}': '{}'",
                    dir.to_string_lossy(), mode,
                );
                plan.original.insert(
                    dir.to_path_buf(), DirMode { mode, priority }
                );
            }
        }
    }
    Ok(())
}

/// Register `dir` in the [`DirPlan`] as read-only if the archived `mode` has
/// no write bits at all. A higher-`priority` registration for the same
/// directory wins.
#[cfg(windows)]
pub fn set_chmod_plan(
            plan: &mut DirPlan, dir: &Path, mode: u32, priority: usize
        ) -> io::Result<()> {

    // Windows doesn't use POSIX modes; "read-only" is a flag. We're inferring
    // this from no write permissions for user, group, or others in the tar
    // header's mode byte.
    if (mode & 0o222) == 0 {
        match plan.original.get(&dir.to_path_buf()) {
            Some(existing) if existing.priority > priority => {
                debug!(
                    "Destination path '{}' already processed: 'readonly={}'",
                    dir.to_string_lossy(), existing.readonly
                );
                return Ok(());
            }
            _ => {
                debug!(
                    "Registering mode for '{}': 'readonly=true'",
                    dir.to_string_lossy(),
                );
                plan.original.insert(
                    dir.to_path_buf(), 
                    DirMode { readonly: true, priority: priority }
                );
            }
        }
    }
    Ok(())
}

/// Apply every mode registered in the [`DirPlan`], deepest paths first (a
/// directory made read-only too early would block updates to its children).
/// Call once, after all extraction workers have finished.
#[cfg(unix)]
pub fn apply_chmod_plan(plan: DirPlan) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Apply final perms deepest-first (deepest first because can't alter child
    // perms if parent is RO).
    let mut dirs: Vec<PathBuf> = plan.original.keys().cloned().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    dirs.dedup();

    for dir in dirs {
        if let Some(spec) = plan.original.get(&dir) {
            fs::set_permissions( &dir, fs::Permissions::from_mode(spec.mode))?;
        }
    }

    Ok(())
}

/// Apply every read-only flag registered in the [`DirPlan`], deepest paths
/// first. Call once, after all extraction workers have finished.
#[cfg(windows)]
pub fn apply_chmod_plan(plan: DirPlan) -> io::Result<()> {
    // Apply final perms deepest-first (deepest first because can't alter child
    // perms if parent is RO).
    let mut dirs: Vec<PathBuf> = plan.original.keys().cloned().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    dirs.dedup();

    for dir in dirs {
        if let Some(spec) = plan.original.get(&dir) {
            let mut perm = fs::metadata(&dir)?.permissions();
            perm.set_readonly(spec.readonly);
            fs::set_permissions(&dir, perm)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::sanitize_rel_path;
    use std::path::{Path, PathBuf};

    // `sanitize_rel_path` is the path-traversal defense applied to every tar
    // entry path during extract/verify -- an entry must never be able to
    // escape the destination directory.

    #[test]
    fn plain_relative_path_passes_through() {
        assert_eq!(
            sanitize_rel_path(Path::new("a/b/c.txt")),
            Some(PathBuf::from("a/b/c.txt"))
        );
    }

    #[test]
    fn absolute_path_is_made_relative() {
        assert_eq!(
            sanitize_rel_path(Path::new("/etc/passwd")),
            Some(PathBuf::from("etc/passwd"))
        );
    }

    #[test]
    fn parent_dir_component_is_rejected() {
        // Anywhere in the path -- leading, embedded, or trailing
        assert_eq!(sanitize_rel_path(Path::new("../evil")), None);
        assert_eq!(sanitize_rel_path(Path::new("a/../../evil")), None);
        assert_eq!(sanitize_rel_path(Path::new("a/b/..")), None);
        assert_eq!(sanitize_rel_path(Path::new("/../evil")), None);
    }

    #[test]
    fn cur_dir_components_are_stripped() {
        assert_eq!(
            sanitize_rel_path(Path::new("./a/./b")),
            Some(PathBuf::from("a/b"))
        );
    }

    #[test]
    fn paths_with_no_normal_components_are_rejected() {
        assert_eq!(sanitize_rel_path(Path::new("")), None);
        assert_eq!(sanitize_rel_path(Path::new(".")), None);
        assert_eq!(sanitize_rel_path(Path::new("/")), None);
        assert_eq!(sanitize_rel_path(Path::new("./.")), None);
    }
}
