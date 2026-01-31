use std::io;
use std::path::{Component, Path, PathBuf};

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
