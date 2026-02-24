// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::collections::HashMap;

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

#[derive(Clone, Copy, Debug)]
pub struct DirMode {
    mode: u32,
    // deterministic conflict resolution if multiple tars set the same dir
    priority: usize, 
}

#[derive(Default, Debug)]
pub struct DirPlan {
    // Final directory mode requested by explained tar entries.
    desired: HashMap<PathBuf, DirMode>,
    // Original mode for dirs we temporarily chmod'd to be writable.
    original: HashMap<PathBuf, u32>,
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

#[cfg(unix)]
pub fn ensure_owner_writable(dir: &Path, plan: &mut DirPlan) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Ok(());
    }

    let mode = meta.permissions().mode();

    // Need write+execute on a directory to create entries inside it.
    let need_bits = 0o300;
    if (mode & need_bits) != need_bits {
        plan.original.entry(dir.to_path_buf()).or_insert(mode);
        fs::set_permissions(dir, fs::Permissions::from_mode(mode | need_bits))?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn ensure_owner_writable(dir: &Path, _plan: &mut DirPlan) -> io::Result<()> {
    // Windows doesn't use POSIX modes; "read-only" is a flag.
    let meta = fs::metadata(dir)?;
    if !meta.is_dir() {
        return Ok(());
    }
    let mut perm = meta.permissions();
    if perm.readonly() {
        perm.set_readonly(false);
        fs::set_permissions(dir, perm)?;
    }
    Ok(())
}

pub fn record_desired_dir_mode(
            plan: &mut DirPlan, dir: PathBuf, mode: u32, priority: usize
        ) {
    // Deterministic: highest priority wins (priority can be index in input list).
    match plan.desired.get(&dir) {
        Some(existing) if existing.priority > priority => {}
        _ => {
            plan.desired.insert(dir, DirMode { mode, priority });
        }
    }
}

pub fn finalize_directory_permissions(plan: DirPlan) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Apply final perms deepest-first.
        let mut dirs: Vec<PathBuf> = plan.desired.keys().cloned().collect();
        dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        dirs.dedup();

        for dir in dirs {
            if let Some(spec) = plan.desired.get(&dir) {
                fs::set_permissions(&dir, fs::Permissions::from_mode(spec.mode))?;
            }
        }

        // Restore dirs we temporarily chmod'd, unless overridden by a desired mode.
        for (dir, orig_mode) in plan.original {
            if !plan.desired.contains_key(&dir) {
                fs::set_permissions(&dir, fs::Permissions::from_mode(orig_mode))?;
            }
        }
    }

    #[cfg(windows)]
    {
        // Emulate tar crate behavior: mark readonly if no owner-write bit.
        for (dir, spec) in plan.desired {
            let readonly = (spec.mode & 0o200) == 0;
            let mut perm = fs::metadata(&dir)?.permissions();
            perm.set_readonly(readonly);
            fs::set_permissions(&dir, perm)?;
        }
    }

    Ok(())
}
