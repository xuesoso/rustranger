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

/// Natural ("version") comparison: digit runs compared numerically. Non-digit
/// runs fold ASCII case when `ci` is set, matching the basename comparator.
fn natural_cmp(a: &str, b: &str, ci: bool) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na = take_number(&mut ai);
                    let nb = take_number(&mut bi);
                    match na.cmp(&nb) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                } else {
                    ai.next();
                    bi.next();
                    let (xa, xb) = if ci {
                        (ca.to_ascii_lowercase(), cb.to_ascii_lowercase())
                    } else {
                        (ca, cb)
                    };
                    match xa.cmp(&xb) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                }
            }
        }
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Chars>) -> u128 {
    let mut n: u128 = 0;
    while let Some(c) = it.peek().copied() {
        if c.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((c as u8 - b'0') as u128);
            it.next();
        } else {
            break;
        }
    }
    n
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
}
