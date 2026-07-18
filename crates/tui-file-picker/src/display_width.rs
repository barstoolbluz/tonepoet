//! Shared terminal display-column measurement and fitting helpers.
//!
//! Wide CJK/fullwidth characters consume two cells and combining marks consume
//! zero. Ambiguous East Asian Width remains terminal-policy dependent and is
//! intentionally outside this module's contract; `unicode-width` supplies one
//! internally consistent policy for the workspace.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[must_use]
pub fn width(text: &str) -> usize {
    text.width()
}

#[must_use]
pub fn char_width(ch: char) -> usize {
    ch.width().unwrap_or(0)
}

fn pad(mut text: String, used: usize, target: usize, right_align: bool) -> String {
    if used >= target {
        return text;
    }
    let spaces = " ".repeat(target - used);
    if right_align {
        text.insert_str(0, &spaces);
    } else {
        text.push_str(&spaces);
    }
    text
}

fn prefix_within(text: &str, limit: usize) -> (String, usize) {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = char_width(ch);
        if ch_width == 0 && out.is_empty() {
            // Never start a fitted fragment with an orphan combining mark.
            continue;
        }
        if used.saturating_add(ch_width) > limit {
            break;
        }
        out.push(ch);
        used = used.saturating_add(ch_width);
    }
    (out, used)
}

fn suffix_within(text: &str, limit: usize) -> (String, usize) {
    let mut reversed = Vec::new();
    let mut used = 0usize;
    for ch in text.chars().rev() {
        let ch_width = char_width(ch);
        if used.saturating_add(ch_width) > limit {
            break;
        }
        reversed.push(ch);
        used = used.saturating_add(ch_width);
    }
    // Reverse traversal sees combining marks before their base. If the base
    // failed to fit, those marks would become the suffix's first code points.
    while reversed.last().is_some_and(|ch| char_width(*ch) == 0) {
        reversed.pop();
    }
    let out = reversed.into_iter().rev().collect::<String>();
    (out.clone(), width(&out))
}

/// Truncate at the right edge, appending an ellipsis on overflow.
#[must_use]
pub fn truncate_right(text: &str, target: usize) -> String {
    if width(text) <= target {
        return text.to_string();
    }
    match target {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let (mut out, _) = prefix_within(text, target - 1);
            out.push('…');
            out
        }
    }
}

/// Truncate at the left edge, prepending an ellipsis on overflow.
#[must_use]
pub fn truncate_left(text: &str, target: usize) -> String {
    if width(text) <= target {
        return text.to_string();
    }
    match target {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let (tail, _) = suffix_within(text, target - 1);
            format!("…{tail}")
        }
    }
}

/// Truncate in the middle, preserving both ends when possible.
#[must_use]
pub fn truncate_middle(text: &str, target: usize) -> String {
    if width(text) <= target {
        return text.to_string();
    }
    if target <= 1 {
        return truncate_right(text, target);
    }
    let remaining = target - 1;
    let left_limit = remaining.div_ceil(2);
    let right_limit = remaining / 2;
    let (left, _) = prefix_within(text, left_limit);
    let (right, _) = suffix_within(text, right_limit);
    format!("{left}…{right}")
}

/// Fit to exactly `target` cells, truncating at the right edge and padding on
/// the requested side.
#[must_use]
pub fn pad_or_truncate(text: &str, target: usize, right_align: bool) -> String {
    let fitted = truncate_right(text, target);
    let used = width(&fitted);
    pad(fitted, used, target, right_align)
}

/// Fit a prefix to exactly `target` cells without adding an ellipsis.
#[must_use]
pub fn fit_prefix(text: &str, target: usize) -> String {
    let (out, used) = prefix_within(text, target);
    pad(out, used, target, false)
}

/// Fit a name-like value to exactly `target` cells, retaining its prefix.
#[must_use]
pub fn fit_start(text: &str, target: usize) -> String {
    let fitted = truncate_right(text, target);
    let used = width(&fitted);
    pad(fitted, used, target, false)
}

/// Fit a path-like value to exactly `target` cells, retaining its suffix.
#[must_use]
pub fn fit_end(text: &str, target: usize) -> String {
    let fitted = truncate_left(text, target);
    let used = width(&fitted);
    pad(fitted, used, target, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_are_display_column_exact_for_wide_and_combining_text() {
        for sample in ["plain", "25 ・ 8P-5137", "日本語", "e\u{301}lan"] {
            for target in 0..=12 {
                assert!(width(&truncate_right(sample, target)) <= target);
                assert!(width(&truncate_left(sample, target)) <= target);
                assert!(width(&truncate_middle(sample, target)) <= target);
                assert_eq!(width(&pad_or_truncate(sample, target, false)), target);
                assert_eq!(width(&pad_or_truncate(sample, target, true)), target);
                assert_eq!(width(&fit_prefix(sample, target)), target);
                assert_eq!(width(&fit_start(sample, target)), target);
                assert_eq!(width(&fit_end(sample, target)), target);
            }
        }
    }

    #[test]
    fn fitted_suffix_does_not_start_with_an_orphan_combining_mark() {
        assert!(!fit_end("界e\u{301}", 2).trim_start().starts_with('\u{301}'));
    }
}
