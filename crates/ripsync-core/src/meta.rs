//! Metadata preservation and the destination-containment safety checks.
//!
//! Containment is the defence against the rsync symlink path-traversal CVE class
//! (CVE-2024-12087/12088): before ripsync writes a file, creates a symlink, or
//! deletes anything, the *real* parent directory of the target must resolve to a
//! location inside the destination root. A pre-existing symlink in the
//! destination that redirects a write outside the root is therefore refused
//! rather than followed.

use std::path::{Component, Path, PathBuf};

use filetime::FileTime;

use crate::{Error, Result};

/// Emit a one-time warning that a POSIX-only metadata facility is unavailable on
/// this platform (Windows). The operation degrades to a no-op; mtime is still
/// preserved.
#[cfg(not(unix))]
fn warn_once(flag: &std::sync::atomic::AtomicBool, what: &str) {
    use std::sync::atomic::Ordering;
    if !flag.swap(true, Ordering::Relaxed) {
        tracing::warn!("{what} is not supported on this platform; skipping (mtime is preserved)");
    }
}

/// The kind of a stat-ed entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTypeKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Symlink,
    /// Anything else (socket, FIFO, device) — unsupported.
    Other,
}

/// Minimal metadata ripsync needs per entry: type, size, mtime, mode, and
/// inode/device (for hardlink detection and the persistent index).
#[derive(Debug, Clone, Copy)]
pub struct MinMeta {
    /// Entry kind.
    pub kind: FileTypeKind,
    /// Byte length (files only).
    pub len: u64,
    /// Modification time of the entry itself (links not followed).
    pub mtime: FileTime,
    /// Unix permission+type bits.
    pub mode: u32,
    /// Inode number.
    pub ino: u64,
    /// Device id.
    pub dev: u64,
    /// User id.
    pub uid: u32,
    /// Group id.
    pub gid: u32,
}

/// Fetch [`MinMeta`] for `path` without following a final symlink.
///
/// On Linux this uses `statx` with a minimal field mask (cheaper than a full
/// `stat`); elsewhere it falls back to `symlink_metadata`.
///
/// # Errors
///
/// Returns an error if the path cannot be stat-ed.
#[cfg(target_os = "linux")]
pub fn meta_min(path: &Path) -> Result<MinMeta> {
    use rustix::fs::{AtFlags, CWD, StatxFlags, statx};

    const S_IFMT: u32 = 0o170_000;

    let mask = StatxFlags::TYPE
        | StatxFlags::MODE
        | StatxFlags::SIZE
        | StatxFlags::MTIME
        | StatxFlags::INO
        | StatxFlags::UID
        | StatxFlags::GID;
    let stx = statx(CWD, path, AtFlags::SYMLINK_NOFOLLOW, mask)
        .map_err(|e| Error::io(path, std::io::Error::from_raw_os_error(e.raw_os_error())))?;

    let mode = u32::from(stx.stx_mode);
    let kind = match mode & S_IFMT {
        0o100_000 => FileTypeKind::File,
        0o040_000 => FileTypeKind::Dir,
        0o120_000 => FileTypeKind::Symlink,
        _ => FileTypeKind::Other,
    };
    let mtime = FileTime::from_unix_time(stx.stx_mtime.tv_sec, stx.stx_mtime.tv_nsec as u32);
    let dev = rustix::fs::makedev(stx.stx_dev_major, stx.stx_dev_minor);
    Ok(MinMeta {
        kind,
        len: stx.stx_size,
        mtime,
        mode,
        ino: stx.stx_ino,
        dev,
        uid: stx.stx_uid,
        gid: stx.stx_gid,
    })
}

/// Portable fallback using `symlink_metadata`.
///
/// # Errors
///
/// Returns an error if the path cannot be stat-ed.
#[cfg(not(target_os = "linux"))]
pub fn meta_min(path: &Path) -> Result<MinMeta> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    let ftype = meta.file_type();
    let kind = if ftype.is_symlink() {
        FileTypeKind::Symlink
    } else if ftype.is_dir() {
        FileTypeKind::Dir
    } else if ftype.is_file() {
        FileTypeKind::File
    } else {
        FileTypeKind::Other
    };
    #[cfg(unix)]
    let (mode, ino, dev, uid, gid) = {
        use std::os::unix::fs::MetadataExt;
        (meta.mode(), meta.ino(), meta.dev(), meta.uid(), meta.gid())
    };
    #[cfg(not(unix))]
    let (mode, ino, dev, uid, gid) = (0u32, 0u64, 0u64, 0u32, 0u32);
    Ok(MinMeta {
        kind,
        len: meta.len(),
        mtime: FileTime::from_last_modification_time(&meta),
        mode,
        ino,
        dev,
        uid,
        gid,
    })
}

/// Set selected ownership fields. On non-Unix platforms this is a no-op.
///
/// # Errors
///
/// Returns an error if ownership was requested but could not be changed.
pub fn set_owner_group(
    path: &Path,
    uid: u32,
    gid: u32,
    owner: bool,
    group: bool,
    follow: bool,
) -> Result<()> {
    #[cfg(unix)]
    if owner || group {
        use rustix::fs::{AtFlags, CWD, Gid, Uid};

        let uid = owner.then(|| Uid::from_raw(uid));
        let gid = group.then(|| Gid::from_raw(gid));
        let flags = if follow {
            AtFlags::empty()
        } else {
            AtFlags::SYMLINK_NOFOLLOW
        };
        rustix::fs::chownat(CWD, path, uid, gid, flags)
            .map_err(|error| Error::io(path, std::io::Error::from(error)))?;
    }
    #[cfg(not(unix))]
    {
        if owner || group {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            warn_once(&WARNED, "owner/group preservation");
        }
        let _ = (path, uid, gid, follow);
    }
    Ok(())
}

/// Copy selected extended attributes from `src` to `dst`.
///
/// POSIX ACLs are represented by dedicated system xattrs on Linux and by the
/// system security attribute on macOS. `xattrs` and `acls` select those groups
/// independently.
///
/// # Errors
///
/// Returns an error if selected attributes cannot be listed, read, or written.
pub fn copy_xattrs(src: &Path, dst: &Path, xattrs: bool, acls: bool) -> Result<()> {
    #[cfg(unix)]
    if xattrs || acls {
        for name in xattr::list(src).map_err(|error| Error::io(src, error))? {
            let is_acl = is_acl_xattr(&name);
            if (is_acl && !acls) || (!is_acl && !xattrs) {
                continue;
            }
            if let Some(value) = xattr::get(src, &name).map_err(|error| Error::io(src, error))? {
                xattr::set(dst, &name, &value).map_err(|error| Error::io(dst, error))?;
            }
        }
    }
    #[cfg(not(unix))]
    {
        if xattrs || acls {
            static WARNED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            warn_once(&WARNED, "extended attributes / POSIX ACLs");
        }
        let _ = (src, dst);
    }
    Ok(())
}

#[cfg(unix)]
fn is_acl_xattr(name: &std::ffi::OsStr) -> bool {
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(name);
    bytes.starts_with(b"system.posix_acl_") || bytes == b"com.apple.system.Security"
}

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
        // strip setuid/setgid and file-type bits; propagating them from a remote sender is a security risk
        let perm = std::fs::Permissions::from_mode(mode & !0o6000 & 0o7777);
        std::fs::set_permissions(path, perm).map_err(|e| Error::io(path, e))?;
    }
    #[cfg(not(unix))]
    {
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        warn_once(&WARNED, "POSIX permission bits");
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
