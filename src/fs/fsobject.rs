// A single filesystem object (file/dir/link/...), ported from
// ranger/container/fsobject.py + file.py. One Entry is built per lstat.

use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FType {
    Dir,
    File,
    Symlink,
    Fifo,
    Socket,
    BlockDevice,
    CharDevice,
    Unknown,
}

#[derive(Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    /// True if the lstat succeeded (entry is readable/accessible).
    pub accessible: bool,
    pub is_link: bool,
    /// For symlinks: whether the link target exists.
    pub link_ok: bool,
    /// Effective type (for a symlink, reflects the *target* type, like ranger).
    pub ftype: FType,
    pub size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: i64,
    pub ctime: i64,
    pub atime: i64,
    pub executable: bool,
    pub marked: bool,
}

impl Entry {
    pub fn load(path: PathBuf) -> Entry {
        let name = file_name_of(&path);
        match fs::symlink_metadata(&path) {
            Ok(lmeta) => {
                let is_link = lmeta.file_type().is_symlink();
                // For symlinks, resolve the target to determine effective type/size.
                let (meta, link_ok) = if is_link {
                    match fs::metadata(&path) {
                        Ok(m) => (m, true),
                        Err(_) => (lmeta.clone(), false),
                    }
                } else {
                    (lmeta.clone(), true)
                };

                let ftype = classify(&meta);
                let mode = meta.mode();
                let executable = matches!(ftype, FType::File) && (mode & 0o111 != 0);

                Entry {
                    path,
                    name,
                    accessible: true,
                    is_link,
                    link_ok,
                    ftype,
                    size: meta.size(),
                    mode,
                    uid: meta.uid(),
                    gid: meta.gid(),
                    mtime: meta.mtime(),
                    ctime: meta.ctime(),
                    atime: meta.atime(),
                    executable,
                    marked: false,
                }
            }
            Err(_) => Entry {
                path,
                name,
                accessible: false,
                is_link: false,
                link_ok: false,
                ftype: FType::Unknown,
                size: 0,
                mode: 0,
                uid: 0,
                gid: 0,
                mtime: 0,
                ctime: 0,
                atime: 0,
                executable: false,
                marked: false,
            },
        }
    }

    pub fn is_dir(&self) -> bool {
        self.ftype == FType::Dir
    }

    pub fn extension(&self) -> Option<&str> {
        // Match on the basename, ignoring a leading dot (so ".bashrc" has no ext).
        let trimmed = self.name.trim_start_matches('.');
        trimmed.rsplit_once('.').map(|(_, ext)| ext)
    }
}

fn classify(meta: &fs::Metadata) -> FType {
    let ft = meta.file_type();
    if ft.is_dir() {
        FType::Dir
    } else if ft.is_file() {
        FType::File
    } else if ft.is_symlink() {
        FType::Symlink
    } else if ft.is_fifo() {
        FType::Fifo
    } else if ft.is_socket() {
        FType::Socket
    } else if ft.is_block_device() {
        FType::BlockDevice
    } else if ft.is_char_device() {
        FType::CharDevice
    } else {
        FType::Unknown
    }
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}
