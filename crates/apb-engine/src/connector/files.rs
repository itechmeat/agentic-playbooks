//! Attachment file names (D7): the one sanitizer every name an attachment
//! lands under goes through, so a name from a message can never carry a path
//! component, a control character, or a Windows-reserved character into the
//! run's attachment directories.

/// The longest file name an attachment is saved under, in bytes, its
/// extension included.
const NAME_MAX_BYTES: usize = 120;

/// The longest extension (the text after the last `.`) a cut keeps; a longer
/// one is just cut off.
const EXT_MAX_BYTES: usize = 10;

/// Sanitizes an attachment's declared file name to one path segment
/// (D7): the part after the last `/` or `\` survives, control characters and
/// `:*?"<>|` become `_`, spaces and dots are trimmed from both ends, and an
/// empty, `.` or `..` result falls back to `attachment-{n}`. A name over
/// [`NAME_MAX_BYTES`] bytes is cut to that on a char boundary, keeping the
/// extension when it is at most [`EXT_MAX_BYTES`] bytes.
pub(crate) fn safe_file_name(raw: Option<&str>, n: usize) -> String {
    let fallback = || format!("attachment-{n}");
    let Some(raw) = raw else {
        return fallback();
    };
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = trimmed_of(&cleaned);
    match trimmed {
        Some(name) if name.len() <= NAME_MAX_BYTES => name.to_string(),
        Some(name) => cut_to_cap(name),
        None => fallback(),
    }
}

/// The name with spaces and dots trimmed from both ends; `None` when nothing
/// is left (an empty, all-space, all-dot, `.` or `..` name).
fn trimmed_of(cleaned: &str) -> Option<&str> {
    let trimmed = cleaned.trim_matches(|c| c == ' ' || c == '.');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Cuts `name` to [`NAME_MAX_BYTES`] bytes on a char boundary. The text after
/// the last `.` (when at most [`EXT_MAX_BYTES`] bytes) is kept as the
/// extension, so the cut never eats it.
fn cut_to_cap(name: &str) -> String {
    if let Some((stem, ext)) = split_extension(name) {
        let room = NAME_MAX_BYTES - ext.len() - 1;
        let stem_cut = boundary_cut(stem, room);
        return format!("{stem_cut}.{ext}");
    }
    boundary_cut(name, NAME_MAX_BYTES)
}

/// The `(stem, extension)` of `name` when it carries a `.` whose right side
/// is a keepable extension (at most [`EXT_MAX_BYTES`] bytes, and not the
/// whole name).
fn split_extension(name: &str) -> Option<(&str, &str)> {
    let at = name.rfind('.')?;
    let (stem, ext) = (&name[..at], &name[at + 1..]);
    if stem.is_empty() || ext.is_empty() || ext.len() > EXT_MAX_BYTES {
        return None;
    }
    Some((stem, ext))
}

/// The longest prefix of `s` of at most `cap` bytes that ends on a char
/// boundary.
fn boundary_cut(s: &str, cap: usize) -> String {
    let mut end = cap.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The next name to try after `name` was taken: the `-k` suffix before the
/// extension the name carries (when it has one), so `a.txt` becomes
/// `a-2.txt`.
pub(crate) fn numbered_name(name: &str, k: u32) -> String {
    match name.rfind('.') {
        Some(at) if at > 0 && at + 1 < name.len() => {
            format!("{}-{k}.{}", &name[..at], &name[at + 1..])
        }
        _ => format!("{name}-{k}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_path_components() {
        assert_eq!(safe_file_name(Some("../../evil.sh"), 1), "evil.sh");
        assert_eq!(
            safe_file_name(Some("/abs/path/report.pdf"), 1),
            "report.pdf"
        );
        assert_eq!(
            safe_file_name(Some("C:\\Users\\x\\doc.docx"), 1),
            "doc.docx"
        );
        assert_eq!(safe_file_name(Some("a\\..\\b.txt"), 1), "b.txt");
    }

    #[test]
    fn replaces_controls_and_reserved_characters() {
        assert_eq!(safe_file_name(Some("a\u{0}b\u{7}.txt"), 1), "a_b_.txt");
        assert_eq!(safe_file_name(Some("line\nbreak.txt"), 1), "line_break.txt");
        assert_eq!(safe_file_name(Some("a:*?\"<>|b.txt"), 1), "a_______b.txt");
        // A reserved character in a dropped component stays dropped.
        assert_eq!(safe_file_name(Some("..\\q?x\\ok.bin"), 1), "ok.bin");
    }

    #[test]
    fn trims_spaces_and_dots_and_refuses_dot_names() {
        assert_eq!(safe_file_name(Some("..hidden.."), 1), "hidden");
        assert_eq!(safe_file_name(Some("  spaced name  "), 1), "spaced name");
        assert_eq!(safe_file_name(Some("."), 4), "attachment-4");
        assert_eq!(safe_file_name(Some(".."), 4), "attachment-4");
        assert_eq!(safe_file_name(Some(""), 2), "attachment-2");
        assert_eq!(safe_file_name(Some("   "), 2), "attachment-2");
        assert_eq!(safe_file_name(Some("///"), 2), "attachment-2");
        assert_eq!(safe_file_name(Some("..\\..\\"), 2), "attachment-2");
        assert_eq!(safe_file_name(None, 3), "attachment-3");
    }

    #[test]
    fn keeps_a_fitting_name_untouched() {
        let long_ok = "x".repeat(NAME_MAX_BYTES);
        assert_eq!(safe_file_name(Some(&long_ok), 1), long_ok);
        let with_ext = format!("{}.pdf", "y".repeat(NAME_MAX_BYTES - 4));
        assert_eq!(safe_file_name(Some(&with_ext), 1), with_ext);
    }

    #[test]
    fn cuts_to_120_bytes_keeping_a_short_extension() {
        let name = format!("{}.pdf", "x".repeat(130));
        let cut = safe_file_name(Some(&name), 1);
        assert_eq!(cut.len(), NAME_MAX_BYTES);
        assert!(cut.ends_with(".pdf"), "{cut:?}");
        assert!(cut.starts_with("xxxx"), "{cut:?}");
        // A multi-dot name keeps only the last segment as its extension.
        let name = format!("{}.tar.gz", "x".repeat(130));
        let cut = safe_file_name(Some(&name), 1);
        assert_eq!(cut.len(), NAME_MAX_BYTES);
        assert!(cut.ends_with(".gz"), "{cut:?}");
    }

    #[test]
    fn cuts_a_long_extension_plainly() {
        let name = format!("{}.{}", "x".repeat(130), "y".repeat(12));
        let cut = safe_file_name(Some(&name), 1);
        assert_eq!(cut.len(), NAME_MAX_BYTES);
        // The extension was too long to keep, so the cut is plain: no dot
        // survives in the first 130 bytes of x's.
        assert!(!cut.contains('.'), "{cut:?}");
    }

    #[test]
    fn cuts_a_multi_byte_name_on_a_char_boundary() {
        let name = format!("{}.txt", "\u{e9}".repeat(80));
        let cut = safe_file_name(Some(&name), 1);
        assert_eq!(cut.len(), NAME_MAX_BYTES, "{cut:?}");
        assert!(cut.ends_with(".txt"), "{cut:?}");
        assert!(cut.chars().all(|c| c == '\u{e9}') || cut.contains('.'));
        // Every byte boundary the cut chose is a char boundary: the string
        // round-trips without replacement characters.
        assert!(!cut.contains('\u{fffd}'), "{cut:?}");
        assert_eq!(
            cut.matches('\u{e9}').count(),
            58,
            "58 two-byte chars fill 116 bytes"
        );
    }

    #[test]
    fn a_dotted_stem_still_counts_as_an_extension_holder() {
        // `a.bashrc` over the cap: the extension is `bashrc` (6 bytes), so
        // the stem `a` is kept whole.
        let name = format!("a.{}", "b".repeat(130));
        let cut = safe_file_name(Some(&name), 1);
        assert!(cut.starts_with('a'), "{cut:?}");
        assert!(cut.ends_with(&"b".repeat(10)), "{cut:?}");
        assert_eq!(cut.len(), NAME_MAX_BYTES);
    }

    #[test]
    fn numbered_names_keep_the_extension() {
        assert_eq!(numbered_name("a.txt", 2), "a-2.txt");
        assert_eq!(numbered_name("archive.tar.gz", 3), "archive.tar-3.gz");
        assert_eq!(numbered_name("noext", 4), "noext-4");
        assert_eq!(numbered_name("dot.end.", 5), "dot.end.-5");
        assert_eq!(numbered_name(".hidden", 6), ".hidden-6");
    }
}
