//! Metadata preservation and the destination-containment safety checks.
//!
//! Containment is the defence against the rsync symlink path-traversal CVE class
//! (CVE-2024-12087/12088): before Ferry writes a file, creates a symlink, or
//! deletes anything, the *real* parent directory of the target must resolve to a
//! location inside the destination root. A pre-existing symlink in the
//! destination that redirects a write outside the root is therefore refused
//! rather than followed.

use std::path::{Component, Path, PathBuf};

use filetime::FileTime;

use crate::{Error, Result};

/// Canonicalize `root`, creating it first if it does not yet exist.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or canonicalized.
pub fn canonical_root(root: &Path) -> Result<PathBuf> {
    if !root.exists() {
        std::fs::create_dir_all(root).map_err(|e| Error::io(root, e))?;
    }
    std::fs::canonicalize(root).map_err(|e| Error::io(root, e))
}

/// Reject a relative path that tries to escape its root via `..` or an absolute
/// component. Source-walk relative paths should never contain these; this is a
/// belt-and-braces check before any join.
///
/// # Errors
///
/// Returns [`Error::Containment`] if `rel` contains a parent-dir or root component.
pub fn check_relative(rel: &Path) -> Result<()> {
    for comp in rel.components() {
        match comp {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Containment(rel.to_path_buf()));
            }
        }
    }
    Ok(())
}

/// Verify that the real parent directory of `target` lies within `root_canon`,
/// returning the concrete path to write/create.
///
/// `target` itself need not exist, but its parent directory must (create
/// directories first). The parent is canonicalized — following any symlinks — and
/// checked against the canonical root.
///
/// # Errors
///
/// Returns [`Error::Containment`] if the resolved parent escapes `root_canon`.
pub fn contained_target(root_canon: &Path, target: &Path) -> Result<PathBuf> {
    let parent = target
        .parent()
        .ok_or_else(|| Error::Containment(target.to_path_buf()))?;
    let parent_canon = std::fs::canonicalize(parent).map_err(|e| Error::io(parent, e))?;
    if !parent_canon.starts_with(root_canon) {
        return Err(Error::Containment(target.to_path_buf()));
    }
    let name = target
        .file_name()
        .ok_or_else(|| Error::Containment(target.to_path_buf()))?;
    Ok(parent_canon.join(name))
}

/// Apply `mode` (Unix permission bits) to `path`. No-op on non-Unix platforms.
///
/// # Errors
///
/// Returns an error if the permissions cannot be set.
#[allow(unused_variables)]
pub fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perm).map_err(|e| Error::io(path, e))?;
    }
    Ok(())
}

/// Set the modification time of `path` (symlinks are followed; use
/// [`set_symlink_mtime`] for the link itself).
///
/// # Errors
///
/// Returns an error if the time cannot be set.
pub fn set_mtime(path: &Path, mtime: FileTime) -> Result<()> {
    filetime::set_file_mtime(path, mtime).map_err(|e| Error::io(path, e))
}

/// Set the modification time of the symlink at `path` without following it.
///
/// # Errors
///
/// Returns an error if the time cannot be set.
pub fn set_symlink_mtime(path: &Path, mtime: FileTime) -> Result<()> {
    filetime::set_symlink_file_times(path, mtime, mtime).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_escape() {
        assert!(check_relative(Path::new("../etc/passwd")).is_err());
        assert!(check_relative(Path::new("a/../../b")).is_err());
        assert!(check_relative(Path::new("a/b/c")).is_ok());
    }

    #[test]
    fn contained_target_blocks_escape_via_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("dst");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let root_canon = canonical_root(&root).unwrap();

        // A symlink inside dst that points outside dst.
        #[cfg(unix)]
        {
            let link = root.join("escape");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            // Writing "through" the link must be refused.
            let target = link.join("evil.txt");
            assert!(matches!(
                contained_target(&root_canon, &target),
                Err(Error::Containment(_))
            ));
        }

        // A normal path inside dst is fine.
        let ok = root.join("file.txt");
        assert!(contained_target(&root_canon, &ok).is_ok());
    }
}
