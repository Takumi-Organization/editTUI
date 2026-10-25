//! Grapheme and display-width helpers over a single line. Columns are byte offsets that
//! always sit on a grapheme boundary; "display columns" are what the terminal shows.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const TAB: usize = 4;

/// Byte offset of the grapheme boundary after `col`.
pub fn next_boundary(line: &str, col: usize) -> usize {
    line[col..]
        .graphemes(true)
        .next()
        .map_or(col, |g| col + g.len())
}

/// Byte offset of the grapheme boundary before `col`.
pub fn prev_boundary(line: &str, col: usize) -> usize {
    line[..col]
        .graphemes(true)
        .next_back()
        .map_or(0, |g| col - g.len())
}

/// Columns a grapheme takes when it starts at display column `at`.
pub fn cell_width(g: &str, at: usize) -> usize {
    if g == "\t" {
        TAB - at % TAB
    } else if g.chars().next().is_some_and(char::is_control) {
        1
    } else {
        UnicodeWidthStr::width(g).max(1)
    }
}

/// Display column where byte offset `col` starts.
pub fn display_col(line: &str, col: usize) -> usize {
    line[..col]
        .graphemes(true)
        .fold(0, |acc, g| acc + cell_width(g, acc))
}

/// Byte offset of the grapheme that covers display column `target` (line end if past it).
pub fn col_at_display(line: &str, target: usize) -> usize {
    let mut acc = 0;
    for (i, g) in line.grapheme_indices(true) {
        let w = cell_width(g, acc);
        if acc + w > target {
            return i;
        }
        acc += w;
    }
    line.len()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Start of the next word after `col` (line end if there is none).
pub fn next_word(line: &str, col: usize) -> usize {
    let mut chars = line[col..].char_indices().peekable();
    while chars.next_if(|&(_, c)| is_word(c)).is_some() {}
    while chars.next_if(|&(_, c)| !is_word(c)).is_some() {}
    chars.peek().map_or(line.len(), |&(i, _)| col + i)
}

/// Start of the word before `col` (0 if there is none).
pub fn prev_word(line: &str, col: usize) -> usize {
    let mut chars = line[..col].char_indices().rev().peekable();
    while chars.next_if(|&(_, c)| !is_word(c)).is_some() {}
    while chars.next_if(|&(_, c)| is_word(c)).is_some() {}
    chars.peek().map_or(0, |&(i, c)| i + c.len_utf8())
}

/// Leading blanks of `line`, for auto-indent.
pub fn indent_of(line: &str) -> &str {
    let end = line
        .find(|c: char| c != ' ' && c != '\t')
        .unwrap_or(line.len());
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_follow_graphemes_not_bytes() {
        let s = "ae\u{301}b"; // a, e + combining acute, b
        assert_eq!(next_boundary(s, 1), 4);
        assert_eq!(prev_boundary(s, 4), 1);
        assert_eq!(prev_boundary(s, 0), 0);
        assert_eq!(next_boundary(s, s.len()), s.len());
    }

    #[test]
    fn tabs_and_wide_chars_have_display_width() {
        assert_eq!(display_col("\tx", 1), 4);
        assert_eq!(display_col("a\tx", 2), 4);
        assert_eq!(display_col("日本", 3), 2);
        assert_eq!(col_at_display("日本", 1), 0); // inside the first wide char
        assert_eq!(col_at_display("日本", 2), 3);
        assert_eq!(col_at_display("ab", 9), 2);
    }

    #[test]
    fn words_move_over_identifiers_and_punctuation() {
        let s = "foo_bar, baz";
        assert_eq!(next_word(s, 0), 9);
        assert_eq!(next_word(s, 9), 12);
        assert_eq!(prev_word(s, 12), 9);
        assert_eq!(prev_word(s, 9), 0);
        assert_eq!(prev_word(s, 0), 0);
    }

    #[test]
    fn indent_is_the_leading_blanks() {
        assert_eq!(indent_of("  \tx"), "  \t");
        assert_eq!(indent_of("x"), "");
        assert_eq!(indent_of("   "), "   ");
    }
}
