// SPDX-License-Identifier: AGPL-3.0-or-later

//! Filesystem helpers for the archiver: work-list enumeration and
//! tar-header permission modes.

use crate::archive::error::ArchiverError;

// File system
use std::fs::{Metadata, symlink_metadata};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
// Logging
use log::warn;
// Import for Unix-specific permissions
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt; 
// Tar files
use tar::Header;

/// Recursively enumerate everything under `folder_path` (files,
/// directories, and symlinks — followed if `follow_links`) as path strings.
/// This is the work list `create` distributes to its workers when not
/// building from a tree file.
pub fn find_files(
        folder_path: &PathBuf, follow_links: bool
    ) -> Result<Vec<String>, ArchiverError<String>> {

    let mut files: Vec<String> = Vec::new();
    for entry in WalkDir::new(folder_path).follow_links(follow_links) {
        let entry = entry?;
        let path = entry.path();

        files.push(
            path.to_str().unwrap_or_else(
                || {
                    warn!(
                        "Couldn't convert '{:?}' to str. Defaulting to \"\"",
                        path
                    );
                    ""
                }
            ).to_string()
        );
    }

    Ok(files)
}

/// Whether `path_str` is a symlink (without following it). Stat failures
/// are logged and treated as "not a symlink".
pub fn is_symlink(path_str: &str) -> bool {
    let path = Path::new(& path_str);
    path.symlink_metadata().map(
        |metadata| metadata.file_type().is_symlink()
    ).unwrap_or_else(
        |err| {
            warn!(
                "'is_symlink({})' returned '{}', defaulting to 'false'",
                path_str, err
            );
            false
        }
    )
}

/// Convert a Unix permission mode (e.g. 0o755) to "rwxr-xr-x".
fn mode_to_string(mode: u32) -> String {
    // We only care about the lowest 9 bits: u=rwx, g=rwx, o=rwx
    let bits = mode & 0o777;

    let mut s = String::with_capacity(9);
    for i in (0..9).rev() {
        let on = (bits & (1 << i)) != 0;
        let ch = match i % 3 {
            2 => if on { 'r' } else { '-' },
            1 => if on { 'w' } else { '-' },
            0 => if on { 'x' } else { '-' },
            _ => unreachable!(),
        };
        s.push(ch);
    }
    s
}

/// Pick a conservative default mode by file type: `0o700` for directories,
/// `0o777` for symlinks (the tar convention), `0o600` for regular files.
/// Used on platforms without Unix mode bits, or when a path can't be
/// statted.
pub fn default_mode_for_path(md: &Metadata) -> u32 {
    let ft = md.file_type();

    if ft.is_dir() {
        0o700 // rwx------ (dirs need x to traverse)
    } else if ft.is_symlink() {
        0o777 // common for symlinks in tar archives
    } else {
        0o600 // rw------- (regular file)
    }
}

/// Set `header`'s mode from `path`'s on-disk permissions, falling back to
/// [`default_mode_for_path`] (or `0o600` if the path can't be statted at
/// all) with a warning rather than failing.
pub fn set_mode_from_path_or_default(header: &mut Header, path: &String) {
    let md = match symlink_metadata(path) {
        Ok(md) => md,
        Err(e) => {
            warn!(
                "Failed to read metadata for '{}' ({}); defaulting to: '{}'",
                path, e, mode_to_string(0o600)
            );
            // If we can't even stat it, choose a safe file default.
            header.set_mode(0o600);
            return;
        }
    };

    let mode: u32 = {
        #[cfg(unix)]
        {
            md.permissions().mode()
        }
        #[cfg(not(unix))]
        {
            let default_mode = default_mode_for_path(&md);
            warn!(
                "'{}' platform has no Unix file mode bits; using defaults: {}",
                path, mode_to_string(default_mode)
            );
            default_mode
        }
    };
    header.set_mode(mode);
}
