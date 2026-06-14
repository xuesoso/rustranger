// Small self-contained helpers ported from ranger's ext/ utilities.

/// Human-readable byte size, ported from ranger/ext/human_readable.py (decimal prefixes).
pub fn human_size(byte_count: u64) -> String {
    if byte_count == 0 {
        return "0".to_string();
    }
    let prefixes = ["B", "k", "M", "G", "T", "P"];
    let unit = 1000.0_f64;
    let mut value = byte_count as f64;
    let mut ind = 0;
    while value >= unit && ind < prefixes.len() - 1 {
        value /= unit;
        ind += 1;
    }
    // Mirror ranger's "%.3g" / "%.4g" significant-figure formatting.
    let sig = if value < 1000.0 { 3 } else { 4 };
    let s = format_sig(value, sig);
    format!("{}{}", s, prefixes[ind])
}

/// Human-readable byte size using binary/IEC (1024-based) units: 1.5K, 1.05M.
pub fn human_size_binary(byte_count: u64) -> String {
    if byte_count == 0 {
        return "0".to_string();
    }
    let prefixes = ["B", "K", "M", "G", "T", "P"];
    let unit = 1024.0_f64;
    let mut value = byte_count as f64;
    let mut ind = 0;
    while value >= unit && ind < prefixes.len() - 1 {
        value /= unit;
        ind += 1;
    }
    let sig = if value < 1000.0 { 3 } else { 4 };
    format!("{}{}", format_sig(value, sig), prefixes[ind])
}

/// Format a float with `sig` significant figures, trimming trailing zeros (like %g).
fn format_sig(value: f64, sig: usize) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    let magnitude = value.abs().log10().floor() as i32;
    let decimals = (sig as i32 - 1 - magnitude).max(0) as usize;
    let s = format!("{:.*}", decimals, value);
    // Trim trailing zeros / dot, like %g.
    if s.contains('.') {
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_string()
    } else {
        s
    }
}

/// Build a `drwxr-xr-x`-style permission string from a unix st_mode.
/// Ported from ranger/container/fsobject.py get_permission_string().
// The `as u32` casts below are no-ops on Linux (mode_t = u32) but required on
// macOS (mode_t = u16); allow the lint that fires only on the Linux build.
#[allow(clippy::unnecessary_cast)]
pub fn permission_string(mode: u32) -> String {
    let mut out = String::with_capacity(10);

    // File-type character. The S_IF* constants are `mode_t`, which is u32 on
    // Linux but u16 on macOS, while `mode` is always u32 (MetadataExt::mode);
    // normalize everything to u32 so this compiles on both.
    let fmt = mode & libc::S_IFMT as u32;
    out.push(match fmt {
        x if x == libc::S_IFDIR as u32 => 'd',
        x if x == libc::S_IFLNK as u32 => 'l',
        x if x == libc::S_IFSOCK as u32 => 's',
        x if x == libc::S_IFIFO as u32 => 'p',
        x if x == libc::S_IFBLK as u32 => 'b',
        x if x == libc::S_IFCHR as u32 => 'c',
        _ => '-',
    });

    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    for (bit, ch) in bits {
        if mode & bit != 0 {
            out.push(ch);
        } else {
            out.push('-');
        }
    }

    // setuid / setgid / sticky overrides.
    let chars: Vec<char> = out.chars().collect();
    let mut chars = chars;
    if mode & 0o4000 != 0 {
        chars[3] = if chars[3] == 'x' { 's' } else { 'S' };
    }
    if mode & 0o2000 != 0 {
        chars[6] = if chars[6] == 'x' { 's' } else { 'S' };
    }
    if mode & 0o1000 != 0 {
        chars[9] = if chars[9] == 'x' { 't' } else { 'T' };
    }
    chars.into_iter().collect()
}

/// Look up a username for a uid via libc getpwuid; falls back to the numeric id.
pub fn username(uid: u32) -> String {
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return uid.to_string();
        }
        cstr_to_string((*pw).pw_name).unwrap_or_else(|| uid.to_string())
    }
}

/// Look up a group name for a gid via libc getgrgid; falls back to the numeric id.
pub fn groupname(gid: u32) -> String {
    unsafe {
        let gr = libc::getgrgid(gid);
        if gr.is_null() {
            return gid.to_string();
        }
        cstr_to_string((*gr).gr_name).unwrap_or_else(|| gid.to_string())
    }
}

unsafe fn cstr_to_string(ptr: *const libc::c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let bytes = std::ffi::CStr::from_ptr(ptr).to_bytes();
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Format a unix timestamp (seconds since the epoch) as a local-time date string:
/// "YYYY/MM/DD", or "YYYY/MM/DD/HH/MM" when `with_time` is set. Uses libc's
/// `localtime_r` so the local timezone and DST are applied (no chrono dependency).
pub fn format_time(secs: i64, with_time: bool) -> String {
    unsafe {
        let t = secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        let (year, month, day) = (tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday);
        if with_time {
            format!(
                "{:04}/{:02}/{:02}/{:02}/{:02}",
                year, month, day, tm.tm_hour, tm.tm_min
            )
        } else {
            format!("{:04}/{:02}/{:02}", year, month, day)
        }
    }
}

/// Display width of a string, counting wide (East-Asian) codepoints as 2 columns.
/// A compact approximation of ranger/ext/widestring.py good enough for layout.
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Truncate a string to fit `max` display columns, appending an ellipsis if cut.
pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 1 {
        return "~".to_string();
    }
    let budget = max - 1; // reserve a column for the ellipsis
    let mut width = 0;
    let mut out = String::new();
    for ch in s.chars() {
        let w = char_width(ch);
        if width + w > budget {
            break;
        }
        width += w;
        out.push(ch);
    }
    out.push('~');
    out
}

/// Display width of a single character: 0 (control/combining), 1, or 2 (wide).
pub fn char_width(ch: char) -> usize {
    let c = ch as u32;
    if c == 0 {
        return 0;
    }
    // Zero-width control characters.
    if c < 0x20 || (0x7f..0xa0).contains(&c) {
        return 0;
    }
    // Combining marks (approximate ranges).
    if (0x0300..0x0370).contains(&c) || (0x1ab0..0x1b00).contains(&c) || (0x1dc0..0x1e00).contains(&c)
        || (0x20d0..0x2100).contains(&c) || (0xfe20..0xfe30).contains(&c)
    {
        return 0;
    }
    if is_wide(c) {
        2
    } else {
        1
    }
}

fn is_wide(c: u32) -> bool {
    matches!(c,
        0x1100..=0x115f      // Hangul Jamo
        | 0x2e80..=0x303e    // CJK radicals, Kangxi
        | 0x3041..=0x33ff    // Hiragana..CJK symbols
        | 0x3400..=0x4dbf    // CJK Ext A
        | 0x4e00..=0x9fff    // CJK Unified
        | 0xa000..=0xa4cf    // Yi
        | 0xac00..=0xd7a3    // Hangul syllables
        | 0xf900..=0xfaff    // CJK compatibility
        | 0xfe30..=0xfe4f    // CJK compat forms
        | 0xff00..=0xff60    // Fullwidth forms
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1faff  // emoji / symbols
        | 0x20000..=0x3fffd  // CJK Ext B+
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_matches_ranger() {
        assert_eq!(human_size(0), "0");
        assert_eq!(human_size(54), "54B");
        assert_eq!(human_size(1500), "1.5k");
        assert_eq!(human_size(1024 * 1024), "1.05M");
    }

    #[test]
    fn human_size_binary_is_1024_based() {
        assert_eq!(human_size_binary(0), "0");
        assert_eq!(human_size_binary(512), "512B");
        assert_eq!(human_size_binary(1024), "1K");
        assert_eq!(human_size_binary(1024 * 1024), "1M");
        assert_eq!(human_size_binary(1536), "1.5K");
    }

    #[test]
    fn permission_string_basic() {
        // 0o40755 = directory rwxr-xr-x
        assert_eq!(permission_string(0o040755), "drwxr-xr-x");
        // 0o100644 = regular file rw-r--r--
        assert_eq!(permission_string(0o100644), "-rw-r--r--");
        // setuid on an executable file
        assert_eq!(permission_string(0o104755), "-rwsr-xr-x");
    }

    #[test]
    fn width_counts_wide_chars() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("日本"), 4);
        assert_eq!(truncate("abcdef", 4), "abc~");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
