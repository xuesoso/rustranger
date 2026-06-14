// Synchronous filesystem operations + helpers. Ported from ranger's
// ext/safe_path.py, ext/relative_symlink.py and actions.py delete/mkdir/rename.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const SUFFIX: &str = "_";

/// Return a destination path that does not collide with an existing file,
/// appending `_`, then `_0`, `_1`, ... Ported from ext/safe_path.py.
pub fn get_safe_path(dst: &Path) -> PathBuf {
    if !dst.exists() {
        return dst.to_path_buf();
    }
    let base = dst.to_string_lossy().into_owned();
    let mut candidate = if base.ends_with(SUFFIX) {
        base.clone()
    } else {
        let c = format!("{}{}", base, SUFFIX);
        if !Path::new(&c).exists() {
            return PathBuf::from(c);
        }
        c
    };
    let mut n = 0u64;
    loop {
        let test = format!("{}{}", candidate, n);
        if !Path::new(&test).exists() {
            return PathBuf::from(test);
        }
        n += 1;
        // keep candidate stable; only n changes
        let _ = &mut candidate;
    }
}

/// Recursively delete a path. Directories use remove_dir_all; everything else
/// (files, symlinks, special files) uses remove_file so symlinks aren't followed.
pub fn delete_path(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

pub fn mkdir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

pub fn touch(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new().create(true).append(true).open(path)?;
    Ok(())
}

pub fn rename(src: &Path, dst: &Path) -> io::Result<()> {
    if let Some(parent) = dst.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::rename(src, dst)
}

pub fn chmod(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Create a symlink in `dest_dir` pointing at `src`. When `relative`, the link
/// target is computed relative to dest_dir (ranger ext/relative_symlink.py).
pub fn symlink_into(src: &Path, dest_dir: &Path, relative: bool) -> io::Result<()> {
    let name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let link_path = get_safe_path(&dest_dir.join(name));
    let target = if relative {
        relative_path(dest_dir, src)
    } else {
        src.to_path_buf()
    };
    std::os::unix::fs::symlink(target, link_path)
}

pub fn hardlink_into(src: &Path, dest_dir: &Path) -> io::Result<()> {
    let name = src
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let link_path = get_safe_path(&dest_dir.join(name));
    fs::hard_link(src, link_path)
}

/// Compute a path to `target` relative to directory `from`.
fn relative_path(from: &Path, target: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = target.components().collect();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = PathBuf::new();
    for _ in common..from.len() {
        result.push("..");
    }
    for comp in &to[common..] {
        result.push(comp.as_os_str());
    }
    if result.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_path_avoids_collisions() {
        let dir = std::env::temp_dir().join(format!("rr_safe_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let target = dir.join("x");
        // Nonexistent path returns unchanged.
        assert_eq!(get_safe_path(&target), target);
        // Existing path gets a suffix.
        fs::write(&target, b"a").unwrap();
        let p1 = get_safe_path(&target);
        assert_eq!(p1, dir.join("x_"));
        fs::write(&p1, b"a").unwrap();
        let p2 = get_safe_path(&target);
        assert_eq!(p2, dir.join("x_0"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_path_computation() {
        assert_eq!(
            relative_path(Path::new("/a/b/c"), Path::new("/a/b/x/y")),
            PathBuf::from("../x/y")
        );
        assert_eq!(
            relative_path(Path::new("/a/b"), Path::new("/a/b/z")),
            PathBuf::from("z")
        );
    }
}
