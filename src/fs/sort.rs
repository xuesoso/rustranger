// Sorting, ported from ranger/container/directory.py sort_dict.

use crate::fs::fsobject::Entry;
use std::cmp::Ordering;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Natural,
    Basename,
    Size,
    Mtime,
    Ctime,
    Atime,
    Type,
    Extension,
    Random,
}

impl SortKey {
    pub fn from_str(s: &str) -> Option<SortKey> {
        Some(match s {
            "natural" => SortKey::Natural,
            "basename" => SortKey::Basename,
            "size" => SortKey::Size,
            "mtime" => SortKey::Mtime,
            "ctime" => SortKey::Ctime,
            "atime" => SortKey::Atime,
            "type" => SortKey::Type,
            "extension" => SortKey::Extension,
            "random" => SortKey::Random,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            SortKey::Natural => "natural",
            SortKey::Basename => "basename",
            SortKey::Size => "size",
            SortKey::Mtime => "mtime",
            SortKey::Ctime => "ctime",
            SortKey::Atime => "atime",
            SortKey::Type => "type",
            SortKey::Extension => "extension",
            SortKey::Random => "random",
        }
    }
}

pub struct SortOptions {
    pub key: SortKey,
    pub reverse: bool,
    pub directories_first: bool,
    pub case_insensitive: bool,
}

/// Sort `entries` in place. The comparator is allocation-free: case-insensitive
/// comparison lowercases ASCII on the fly instead of allocating a lowercased
/// String per comparison (the old `to_lowercase()` cost O(N log N) allocations).
pub fn sort_entries(entries: &mut [Entry], opts: &SortOptions) {
    entries.sort_by(|a, b| {
        let mut ord = compare(a, b, opts);
        if opts.reverse {
            ord = ord.reverse();
        }
        // directories_first is applied *after* reverse so dirs always stay on top.
        if opts.directories_first && a.is_dir() != b.is_dir() {
            return if a.is_dir() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        ord
    });
}

fn compare(a: &Entry, b: &Entry, opts: &SortOptions) -> Ordering {
    let ci = opts.case_insensitive;
    let by_name = || ci_cmp(&a.name, &b.name, ci);
    match opts.key {
        SortKey::Basename => by_name(),
        SortKey::Natural => natural_cmp(&a.name, &b.name, ci),
        SortKey::Size => a.size.cmp(&b.size).then_with(by_name),
        SortKey::Mtime => a.mtime.cmp(&b.mtime).then_with(by_name),
        SortKey::Ctime => a.ctime.cmp(&b.ctime).then_with(by_name),
        SortKey::Atime => a.atime.cmp(&b.atime).then_with(by_name),
        SortKey::Extension => {
            let ea = a.extension().unwrap_or("");
            let eb = b.extension().unwrap_or("");
            ci_cmp(ea, eb, ci).then_with(by_name)
        }
        SortKey::Type => type_rank(a).cmp(&type_rank(b)).then_with(by_name),
        SortKey::Random => fnv1a(&a.name).cmp(&fnv1a(&b.name)),
    }
}

/// Integer rank for the type sort, preserving the previous alphabetical-by-name
/// grouping (BlockDevice, CharDevice, Dir, Fifo, File, Socket, Symlink, Unknown).
fn type_rank(e: &Entry) -> u8 {
    use crate::fs::fsobject::FType;
    match e.ftype {
        FType::BlockDevice => 0,
        FType::CharDevice => 1,
        FType::Dir => 2,
        FType::Fifo => 3,
        FType::File => 4,
        FType::Socket => 5,
        FType::Symlink => 6,
        FType::Unknown => 7,
    }
}

/// Allocation-free string compare; ASCII letters are folded to lowercase on the
/// fly when `ci` is set. Non-ASCII characters compare by codepoint.
fn ci_cmp(a: &str, b: &str, ci: bool) -> Ordering {
    if !ci {
        return a.cmp(b);
    }
    let mut ai = a.chars();
    let mut bi = b.chars();
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let (lx, ly) = (x.to_ascii_lowercase(), y.to_ascii_lowercase());
                match lx.cmp(&ly) {
                    Ordering::Equal => continue,
                    other => return other,
                }
            }
        }
    }
}

/// Natural ("version") comparison: digit runs compared numerically (leading
/// zeros ignored, arbitrary length), other bytes with ASCII case folding when
/// `ci` is set — matching the basename comparator.
///
/// Operates on bytes, not decoded chars: UTF-8 is an order-preserving encoding,
/// so byte order equals codepoint order for the non-ASCII bytes we never fold.
/// Skipping the char-decode/peekable machinery cut the *default* sort's cost to
/// nearly that of the plain basename comparator (see PERFORMANCE.md #5) — the
/// comparator is no longer the sort's bottleneck.
fn natural_cmp(a: &str, b: &str, ci: bool) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    loop {
        match (a.get(i), b.get(j)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(&ca), Some(&cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    // Compare the digit runs as numbers without parsing them:
                    // after skipping leading zeros, more significant digits =
                    // larger; equal lengths fall to the first differing digit.
                    let (sa, ea) = digit_run(a, i);
                    let (sb, eb) = digit_run(b, j);
                    let ord = (ea - sa)
                        .cmp(&(eb - sb))
                        .then_with(|| a[sa..ea].cmp(&b[sb..eb]));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    i = ea;
                    j = eb;
                } else {
                    let (fa, fb) = if ci {
                        (ca.to_ascii_lowercase(), cb.to_ascii_lowercase())
                    } else {
                        (ca, cb)
                    };
                    match fa.cmp(&fb) {
                        Ordering::Equal => {
                            i += 1;
                            j += 1;
                        }
                        other => return other,
                    }
                }
            }
        }
    }
}

/// Bounds of the significant digits in the run starting at `start`:
/// (first non-zero digit, one past the run). An all-zero run yields an empty
/// significant slice, so "000" and "0" compare equal, like the numbers they are.
fn digit_run(s: &[u8], start: usize) -> (usize, usize) {
    let mut i = start;
    while i < s.len() && s[i] == b'0' {
        i += 1;
    }
    let sig = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    (sig, i)
}

fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn natural_orders_numbers() {
        assert_eq!(natural_cmp("file2", "file10", true), Ordering::Less);
        assert_eq!(natural_cmp("file10", "file2", true), Ordering::Greater);
        assert_eq!(natural_cmp("a", "a", true), Ordering::Equal);
        assert_eq!(natural_cmp("img1", "img1a", true), Ordering::Less);
        // Leading zeros compare as the numbers they are.
        assert_eq!(natural_cmp("file02", "file2", true), Ordering::Equal);
        assert_eq!(natural_cmp("a000", "a0", true), Ordering::Equal);
        assert_eq!(natural_cmp("file010", "file9", true), Ordering::Greater);
        // Numbers longer than any machine integer still order correctly.
        let big_hi = format!("x{}1", "9".repeat(45));
        let big_lo = format!("x{}0", "9".repeat(45));
        assert_eq!(natural_cmp(&big_hi, &big_lo, true), Ordering::Greater);
        // Non-ASCII names compare by codepoint (byte order == codepoint order).
        assert_eq!(natural_cmp("café2", "café10", true), Ordering::Less);
    }

    #[test]
    fn ci_cmp_is_case_insensitive() {
        assert_eq!(ci_cmp("Apple", "apple", true), Ordering::Equal);
        assert_eq!(ci_cmp("Apple", "banana", true), Ordering::Less);
        assert_eq!(ci_cmp("Apple", "apple", false), Ordering::Less); // 'A' < 'a'
        assert_eq!(natural_cmp("File2", "file10", true), Ordering::Less);
    }

    #[test]
    fn from_str_roundtrip() {
        for key in ["natural", "size", "mtime", "type", "extension", "random"] {
            assert_eq!(SortKey::from_str(key).unwrap().name(), key);
        }
        assert!(SortKey::from_str("bogus").is_none());
    }

    // ---- full sort_entries ordering (regression guards) -------------------

    use crate::fs::fsobject::{Entry, FType};

    fn ent(name: &str, size: u64, ftype: FType) -> Entry {
        Entry {
            path: std::path::PathBuf::from(name),
            name: name.to_string(),
            accessible: true,
            is_link: false,
            link_ok: true,
            ftype,
            size,
            mode: 0,
            uid: 0,
            gid: 0,
            mtime: 0,
            mtime_ns: 0,
            ctime: 0,
            atime: 0,
            created: 0,
            executable: false,
            marked: false,
        }
    }

    fn opts(key: SortKey, reverse: bool) -> SortOptions {
        SortOptions {
            key,
            reverse,
            directories_first: true,
            case_insensitive: true,
        }
    }

    fn names(v: &[Entry]) -> Vec<&str> {
        v.iter().map(|e| e.name.as_str()).collect()
    }

    fn sample() -> Vec<Entry> {
        // Deliberately unsorted, mixed case, mixed type.
        vec![
            ent("file10.txt", 10, FType::File),
            ent("dir2", 0, FType::Dir),
            ent("File2.txt", 30, FType::File),
            ent("dir10", 0, FType::Dir),
            ent("file1.log", 20, FType::File),
        ]
    }

    #[test]
    fn natural_sort_dirs_first_case_insensitive() {
        let mut v = sample();
        sort_entries(&mut v, &opts(SortKey::Natural, false));
        // dirs first (natural: 2 < 10), then files natural+ci (1 < 2 < 10).
        assert_eq!(
            names(&v),
            ["dir2", "dir10", "file1.log", "File2.txt", "file10.txt"]
        );
    }

    #[test]
    fn natural_sort_reverse_keeps_dirs_first() {
        let mut v = sample();
        sort_entries(&mut v, &opts(SortKey::Natural, true));
        // reverse flips within each group, but dirs still precede files.
        assert_eq!(
            names(&v),
            ["dir10", "dir2", "file10.txt", "File2.txt", "file1.log"]
        );
    }

    #[test]
    fn basename_sort_is_lexicographic_ci() {
        let mut v = sample();
        sort_entries(&mut v, &opts(SortKey::Basename, false));
        // ci lexicographic: "dir10" < "dir2" ('1' < '2'); files compare byte-wise
        // after "file1" => '.' (0x2e) < '0' (0x30) so file1.log < file10.txt < File2.txt.
        assert_eq!(
            names(&v),
            ["dir10", "dir2", "file1.log", "file10.txt", "File2.txt"]
        );
    }

    #[test]
    fn size_sort_ascending_with_name_tiebreak() {
        let mut v = vec![
            ent("b.txt", 5, FType::File),
            ent("a.txt", 5, FType::File),
            ent("big.txt", 99, FType::File),
        ];
        sort_entries(&mut v, &opts(SortKey::Size, false));
        // equal sizes tie-break by name (a before b), then larger size.
        assert_eq!(names(&v), ["a.txt", "b.txt", "big.txt"]);
    }

    #[test]
    fn extension_sort_groups_by_ext_then_name() {
        let mut v = vec![
            ent("z.rs", 0, FType::File),
            ent("a.txt", 0, FType::File),
            ent("a.rs", 0, FType::File),
            ent("noext", 0, FType::File),
        ];
        sort_entries(&mut v, &opts(SortKey::Extension, false));
        // "" (noext) < "rs" < "txt"; within rs, a before z.
        assert_eq!(names(&v), ["noext", "a.rs", "z.rs", "a.txt"]);
    }
}
