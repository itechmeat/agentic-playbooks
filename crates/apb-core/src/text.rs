//! Untrusted text shown to a person (agent notes, observed values): the
//! characters that must not reach a terminal or a page, because they drive
//! it or hide what the text says.

/// A control character (Cc), a line or paragraph separator, an invisible
/// format character (Unicode Cf: bidirectional overrides and isolates,
/// zero-width characters, the byte order mark, tag characters) or any
/// other Default_Ignorable_Code_Point (the combining grapheme joiner,
/// Hangul fillers, variation selectors, reserved ignorables). Each can
/// forge lines, reorder what a reader sees or split two visually identical
/// strings.
pub fn is_hidden_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') || is_format(c) || is_ignorable(c)
}

/// Unicode Cf characters outside Default_Ignorable_Code_Point: they render
/// as nothing or as a mark that changes the text around it.
fn is_format(c: char) -> bool {
    matches!(
        c,
        '\u{0600}'..='\u{0605}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
    )
}

/// The Unicode Default_Ignorable_Code_Point property (DerivedCoreProperties):
/// characters a renderer shows as nothing when it does not support them.
fn is_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFF8}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

/// The zero width joiner that glues emoji into one picture.
const ZWJ: char = '\u{200D}';

fn is_variation_selector(c: char) -> bool {
    matches!(c, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}')
}

/// The skin tone modifiers an emoji in a joined sequence may carry.
fn is_emoji_modifier(c: char) -> bool {
    matches!(c, '\u{1F3FB}'..='\u{1F3FF}')
}

/// Close to Unicode Extended_Pictographic: the blocks emoji live in, so a
/// joiner between two of them is an emoji sequence, not a hidden split.
fn is_pictographic(c: char) -> bool {
    matches!(
        c,
        '\u{00A9}'
            | '\u{00AE}'
            | '\u{203C}'
            | '\u{2049}'
            | '\u{2122}'
            | '\u{2139}'
            | '\u{2194}'..='\u{21AA}'
            | '\u{231A}'..='\u{23FF}'
            | '\u{24C2}'
            | '\u{25AA}'..='\u{27BF}'
            | '\u{2934}'..='\u{2935}'
            | '\u{2B05}'..='\u{2B55}'
            | '\u{3030}'
            | '\u{303D}'
            | '\u{3297}'
            | '\u{3299}'
            | '\u{1F000}'..='\u{1FAFF}'
            | '\u{1FC00}'..='\u{1FFFD}'
    )
}

/// Whether `chars[i]` is hidden where it stands. A variation selector
/// directly after a visible base character picks how that character looks
/// (`⚠️`, `❤️`), and a joiner between two emoji makes one picture of them
/// (a family emoji): both are part of what the reader sees. Standing alone,
/// at the start or after another hidden character they hide nothing but
/// themselves, so they stay hidden like every other [`is_hidden_char`].
fn hidden_at(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    if !is_hidden_char(c) {
        return false;
    }
    if is_variation_selector(c) {
        let base = i.checked_sub(1).map(|p| chars[p]);
        return !base.is_some_and(|b| !is_hidden_char(b) && !is_emoji_modifier(b));
    }
    if c == ZWJ {
        let before = chars[..i]
            .iter()
            .rev()
            .find(|b| !is_variation_selector(**b) && !is_emoji_modifier(**b));
        let after = chars.get(i + 1);
        return !(before.is_some_and(|b| is_pictographic(*b))
            && after.is_some_and(|a| is_pictographic(*a)));
    }
    true
}

/// Whether `s` holds a hidden character where it stands (see
/// [`is_hidden_char`]; emoji presentation selectors and joiners inside an
/// emoji are not hidden).
pub fn has_hidden(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    (0..chars.len()).any(|i| hidden_at(&chars, i))
}

/// `s` with every hidden character (see [`has_hidden`]) replaced by `with`.
pub fn replace_hidden(s: &str, with: char) -> String {
    let chars: Vec<char> = s.chars().collect();
    (0..chars.len())
        .map(|i| if hidden_at(&chars, i) { with } else { chars[i] })
        .collect()
}

/// `s` with every hidden character replaced by `?`, for a terminal line.
pub fn visible(s: &str) -> String {
    replace_hidden(s, '?')
}

/// `s` without its terminal escape sequences: CSI (`ESC [ ... final`, the
/// colors and cursor moves), OSC (`ESC ] ... BEL` or `ESC \\`) and the
/// short `ESC x` forms. For reading a tool's output as text: a colored
/// line then parses like a plain one. A lone trailing ESC is dropped.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.next() {
            // Parameter and intermediate bytes, then one final byte.
            Some('[') => {
                for c in it.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && it.peek() == Some(&'\\') {
                        it.next();
                        break;
                    }
                }
            }
            // `ESC ( B` and the like: intermediate bytes, then a final one.
            Some(c) if (' '..='/').contains(&c) => {
                for c in it.by_ref() {
                    if !(' '..='/').contains(&c) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_colors_and_titles_and_keeps_the_text() {
        assert_eq!(
            strip_ansi("test a ... \u{1b}[31mFAILED\u{1b}[0m"),
            "test a ... FAILED"
        );
        assert_eq!(
            strip_ansi("\u{1b}[1m\u{1b}[92m   Compiling\u{1b}[0m x"),
            "   Compiling x"
        );
        assert_eq!(strip_ansi("a\u{1b}]0;title\u{7}b"), "ab");
        assert_eq!(strip_ansi("a\u{1b}]8;;url\u{1b}\\b"), "ab");
        assert_eq!(strip_ansi("a\u{1b}(Bb\u{1b}"), "ab");
        assert_eq!(strip_ansi("plain [31m text"), "plain [31m text");
    }

    #[test]
    fn controls_separators_and_format_characters_are_hidden() {
        for c in [
            '\n', '\u{1b}', '\u{7f}', '\u{202E}', '\u{2066}', '\u{200B}', '\u{FEFF}', '\u{2028}',
        ] {
            assert!(is_hidden_char(c), "{:?}", c);
        }
        for c in ['a', ' ', '-', '\u{e9}'] {
            assert!(!is_hidden_char(c), "{:?}", c);
        }
        assert_eq!(visible("a\u{202E}b\nc"), "a?b?c");
    }

    #[test]
    fn default_ignorable_characters_outside_cf_are_hidden() {
        for c in [
            '\u{034F}',
            '\u{115F}',
            '\u{1160}',
            '\u{17B4}',
            '\u{180B}',
            '\u{180F}',
            '\u{3164}',
            '\u{FE00}',
            '\u{FE0F}',
            '\u{FFA0}',
            '\u{E0100}',
            '\u{E01EF}',
        ] {
            assert!(is_hidden_char(c), "{:?}", c);
        }
        assert_eq!(visible("\u{FE0F}admin"), "?admin");
    }

    #[test]
    fn emoji_selectors_and_joiners_inside_an_emoji_are_shown() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        for s in [
            "\u{26A0}\u{FE0F} disk",
            "\u{2764}\u{FE0F}",
            family,
            "\u{1F441}\u{FE0F}\u{200D}\u{1F5E8}\u{FE0F}",
        ] {
            assert!(!has_hidden(s), "{s:?}");
            assert_eq!(visible(s), s);
        }
    }

    #[test]
    fn a_selector_or_joiner_standing_alone_stays_hidden() {
        for s in [
            "\u{FE0F}",
            "\u{FE0F}a",
            "a\u{FE0F}\u{FE0F}",
            "\u{200B}\u{FE0F}",
            "\u{200D}\u{1F468}",
            "a\u{200D}b",
            "\u{1F468}\u{200D}",
        ] {
            assert!(has_hidden(s), "{s:?}");
        }
        assert_eq!(visible("\u{FE0F}"), "?");
        assert_eq!(visible("a\u{200D}b"), "a?b");
        assert_eq!(replace_hidden("x\u{FE0F}\u{FE0F}", ' '), "x\u{FE0F} ");
    }
}
