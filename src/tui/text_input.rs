//! Shared text input state with UTF-8 safe cursor movement and horizontal scrolling

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};

/// State for a single-line text input field.
///
/// The cursor is a byte offset into `text` that is always on a UTF-8 char boundary.
/// Movement methods (`cursor_left`, `cursor_right`, `backspace`, `delete`, etc.)
/// walk char boundaries to avoid panics on multibyte input.
#[derive(Debug, Clone)]
pub struct TextInputState {
    pub text: String,
    /// Byte offset into `text`; always on a UTF-8 char boundary.
    pub cursor: usize,
    /// When true, the entire text is selected. The next character input
    /// replaces all text; navigation keys clear the selection.
    pub select_all: bool,
}

/// Completion strategy for an inline text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionMode {
    None,
    /// Complete filesystem paths in the current process working directory.
    Path,
    /// Complete Tonepoet filename/folder-template variables such as `%ARTIST%`.
    TemplateVariable,
}

const TEMPLATE_VARIABLES: &[&str] = &[
    "%NN%",
    "%TRACKNN%",
    "%N%",
    "%TRACKN%",
    "%TRACK%",
    "%TITLE%",
    "%ARTIST%",
    "%ALBUM_ARTIST%",
    "%ALBUM%",
    "%TITLE_EXTRA%",
    "%DISC%",
    "%FORMAT%",
    "%YEAR%",
    "%GENRE%",
    "%COMPOSER%",
    "%CATALOG%",
    "%SAMPLERATE%",
    "%BITDEPTH%",
    "%ISRC%",
    "%EXT%",
];

impl TextInputState {
    /// Create a new input with the given initial text, cursor at end.
    pub fn new(initial: String) -> Self {
        let cursor = initial.len();
        Self {
            text: initial,
            cursor,
            select_all: false,
        }
    }

    /// Create a new input with all text selected.
    pub fn new_selected(initial: String) -> Self {
        let cursor = initial.len();
        Self {
            text: initial,
            cursor,
            select_all: true,
        }
    }

    /// Create an empty input.
    pub fn empty() -> Self {
        Self::new(String::new())
    }

    /// Insert a character at the cursor and advance the cursor.
    pub fn insert_char(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert a string at the cursor and advance the cursor past it.
    pub fn insert_string(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }


    /// Replace a UTF-8-boundary byte range and move the cursor to the end of
    /// the replacement. Invalid ranges are ignored rather than panicking so
    /// completion helpers remain best-effort UI affordances.
    pub fn replace_range(&mut self, range: std::ops::Range<usize>, replacement: &str) {
        if range.start > range.end
            || range.end > self.text.len()
            || !self.text.is_char_boundary(range.start)
            || !self.text.is_char_boundary(range.end)
        {
            return;
        }
        self.text.replace_range(range.clone(), replacement);
        self.cursor = range.start + replacement.len();
        self.select_all = false;
    }

    /// Replace the full text and clamp the cursor to a valid char boundary.
    pub fn set_text_and_cursor(&mut self, text: String, cursor: usize) {
        self.text = text;
        self.cursor = cursor.min(self.text.len());
        while self.cursor > 0 && !self.text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
        self.select_all = false;
    }

    /// Delete the character before the cursor (Backspace behavior).
    pub fn backspace(&mut self) {
        if let Some(prev) = self.prev_char_boundary() {
            self.text.remove(prev);
            self.cursor = prev;
        }
    }

    /// Delete the character at the cursor (Delete key behavior).
    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    /// Move cursor one char left.
    pub fn cursor_left(&mut self) {
        if let Some(prev) = self.prev_char_boundary() {
            self.cursor = prev;
        }
    }

    /// Move cursor one char right.
    pub fn cursor_right(&mut self) {
        if let Some(next) = self.next_char_boundary() {
            self.cursor = next;
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Find the previous char boundary before the cursor, if any.
    fn prev_char_boundary(&self) -> Option<usize> {
        if self.cursor == 0 {
            return None;
        }
        let mut i = self.cursor - 1;
        while i > 0 && !self.text.is_char_boundary(i) {
            i -= 1;
        }
        Some(i)
    }

    /// Find the next char boundary after the cursor, if any.
    fn next_char_boundary(&self) -> Option<usize> {
        if self.cursor >= self.text.len() {
            return None;
        }
        let mut i = self.cursor + 1;
        while i < self.text.len() && !self.text.is_char_boundary(i) {
            i += 1;
        }
        Some(i)
    }

    /// Walk back from `cursor` skipping whitespace then non-whitespace,
    /// then delete the resulting range. Implements readline `unix-word-rubout`.
    pub fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let original = self.cursor;

        // Phase 1: skip whitespace immediately before cursor
        while self.cursor > 0 {
            let prev = self.prev_char_boundary().unwrap();
            let c = self.text[prev..self.cursor].chars().next().unwrap();
            if !c.is_whitespace() {
                break;
            }
            self.cursor = prev;
        }
        // Phase 2: skip non-whitespace
        while self.cursor > 0 {
            let prev = self.prev_char_boundary().unwrap();
            let c = self.text[prev..self.cursor].chars().next().unwrap();
            if c.is_whitespace() {
                break;
            }
            self.cursor = prev;
        }

        self.text.drain(self.cursor..original);
    }

    /// Walk forward from `cursor` skipping non-whitespace then whitespace,
    /// then delete the resulting range. Cursor stays at its current position.
    pub fn delete_word_forward(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let start = self.cursor;
        let mut end = start;

        // Phase 1: skip non-whitespace at cursor
        while end < self.text.len() {
            let next = next_boundary_after(&self.text, end).unwrap();
            let c = self.text[end..next].chars().next().unwrap();
            if c.is_whitespace() {
                break;
            }
            end = next;
        }
        // Phase 2: skip whitespace
        while end < self.text.len() {
            let next = next_boundary_after(&self.text, end).unwrap();
            let c = self.text[end..next].chars().next().unwrap();
            if !c.is_whitespace() {
                break;
            }
            end = next;
        }

        self.text.drain(start..end);
    }

    /// Delete everything from cursor back to the start of the input.
    pub fn kill_to_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Delete everything from cursor to the end of the input.
    pub fn kill_to_end(&mut self) {
        if self.cursor < self.text.len() {
            self.text.truncate(self.cursor);
        }
    }

    /// Number of display columns from the start of the text to the cursor.
    /// Assumes 1 col per char (correct for ASCII + most Latin; approximate for CJK).
    pub fn cursor_display_col(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    /// Compute a scrolled view of the text for rendering.
    ///
    /// Returns `(visible_text, cursor_col_in_view)` where `visible_text` is a
    /// substring of `width` columns that keeps the cursor in view.
    pub fn view(&self, width: usize) -> (String, u16) {
        if width == 0 {
            return (String::new(), 0);
        }
        let cursor_col = self.cursor_display_col();

        // Scroll so cursor is always visible. When cursor is at col C and width is W,
        // we want scroll = max(0, C - W + 1) so the cursor sits at the right edge.
        let scroll = cursor_col.saturating_sub(width.saturating_sub(1));

        let visible: String = self.text.chars().skip(scroll).take(width).collect();
        let cursor_col_in_view = (cursor_col - scroll) as u16;
        (visible, cursor_col_in_view)
    }
}

/// Walk forward from a byte index, returning the next char boundary.
/// Standalone helper so it can be used without holding a `&mut TextInputState`.
fn next_boundary_after(text: &str, pos: usize) -> Option<usize> {
    if pos >= text.len() {
        return None;
    }
    let mut i = pos + 1;
    while i < text.len() && !text.is_char_boundary(i) {
        i += 1;
    }
    Some(i)
}

/// Handle a key event for a text input. Returns `true` if the key was consumed.
///
/// Plain keys: Char, Backspace, Delete, Left, Right, Home, End.
/// Readline-style Ctrl bindings (case-insensitive on the letter):
///   Ctrl+A=home, Ctrl+E=end, Ctrl+B=left, Ctrl+F=right,
///   Ctrl+H=backspace, Ctrl+D=delete-fwd,
///   Ctrl+W=delete-prev-word, Ctrl+U=kill-to-start, Ctrl+K=kill-to-end.
/// Word-deletion alternatives: Ctrl+Backspace and Alt+Backspace also delete
/// the previous word; Ctrl+Delete and Alt+D delete the next word.
/// Unrecognized Ctrl/Alt combos are ignored (NOT inserted as literal chars).
/// Does NOT handle Enter or Esc — those are overlay-specific.
pub fn handle_text_input_key(input: &mut TextInputState, key: &KeyEvent) -> bool {
    use crossterm::event::KeyModifiers as M;

    // Select-all handling: typing replaces all text, navigation clears selection.
    if input.select_all {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(M::CONTROL) && !key.modifiers.contains(M::ALT) => {
                input.text.clear();
                input.cursor = 0;
                input.select_all = false;
                input.insert_char(c);
                return true;
            }
            KeyCode::Backspace | KeyCode::Delete => {
                input.text.clear();
                input.cursor = 0;
                input.select_all = false;
                return true;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End => {
                input.select_all = false;
                // Fall through to normal handling
            }
            _ => {
                input.select_all = false;
            }
        }
    }

    let ctrl = key.modifiers.contains(M::CONTROL);
    let alt = key.modifiers.contains(M::ALT);

    match (ctrl, alt, key.code) {
        // ── Ctrl+letter / Ctrl+Backspace / Ctrl+Delete ──
        (true, false, KeyCode::Char(c)) => {
            match c.to_ascii_lowercase() {
                'a' => input.cursor_home(),
                'e' => input.cursor_end(),
                'b' => input.cursor_left(),
                'f' => input.cursor_right(),
                'h' => input.backspace(),
                'd' => input.delete(),
                'w' => input.delete_word_back(),
                'u' => input.kill_to_start(),
                'k' => input.kill_to_end(),
                _ => return false,
            }
            true
        }
        (true, false, KeyCode::Backspace) => {
            input.delete_word_back();
            true
        }
        (true, false, KeyCode::Delete) => {
            input.delete_word_forward();
            true
        }
        (true, false, _) => false,

        // ── Alt combos: Alt+Backspace and Alt+D ──
        (false, true, KeyCode::Backspace) => {
            input.delete_word_back();
            true
        }
        (false, true, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'d') => {
            input.delete_word_forward();
            true
        }
        (false, true, _) => false,

        // ── Ctrl+Alt+anything → ignore (system shortcut territory) ──
        (true, true, _) => false,

        // ── Plain keys (SHIFT may be set; that's fine for capitalization) ──
        (false, false, KeyCode::Char(c)) => {
            input.insert_char(c);
            true
        }
        (false, false, KeyCode::Backspace) => {
            input.backspace();
            true
        }
        (false, false, KeyCode::Delete) => {
            input.delete();
            true
        }
        (false, false, KeyCode::Left) => {
            input.cursor_left();
            true
        }
        (false, false, KeyCode::Right) => {
            input.cursor_right();
            true
        }
        (false, false, KeyCode::Home) => {
            input.cursor_home();
            true
        }
        (false, false, KeyCode::End) => {
            input.cursor_end();
            true
        }
        _ => false,
    }
}

/// Apply tab completion according to `mode`. Returns true when the Tab key was
/// consumed, even if no candidate could be inserted.
pub fn apply_tab_completion(input: &mut TextInputState, mode: CompletionMode) -> bool {
    match mode {
        CompletionMode::None => false,
        CompletionMode::Path => apply_path_completion(input, None, true),
        CompletionMode::TemplateVariable => apply_template_variable_completion(input),
    }
}

/// Complete a filesystem path. `base_dir` lets callers complete a bare filename
/// against a known directory (used by inline browse renames) without inserting
/// the directory prefix into the field. When `append_dir_separator` is false,
/// directory candidates are inserted as plain names rather than `name/`.
pub fn apply_path_completion(
    input: &mut TextInputState,
    base_dir: Option<&Path>,
    append_dir_separator: bool,
) -> bool {
    if let Some((text, cursor)) = complete_path_text(
        &input.text,
        input.cursor,
        base_dir,
        append_dir_separator,
    ) {
        input.set_text_and_cursor(text, cursor);
    }
    true
}

/// Complete a `%VARIABLE%` template token at the cursor.
pub fn apply_template_variable_completion(input: &mut TextInputState) -> bool {
    if let Some((text, cursor)) = complete_template_variable_text(&input.text, input.cursor) {
        input.set_text_and_cursor(text, cursor);
    }
    true
}

fn complete_template_variable_text(text: &str, cursor: usize) -> Option<(String, usize)> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let before = &text[..cursor];
    let percent = before.rfind('%')?;
    let partial = &before[percent + 1..];
    if partial.contains('%') {
        return None;
    }
    let wanted = partial.to_ascii_uppercase();
    let mut matches: Vec<&str> = TEMPLATE_VARIABLES
        .iter()
        .copied()
        .filter(|v| v.trim_matches('%').starts_with(&wanted))
        .collect();
    matches.sort_unstable();
    matches.dedup();
    if matches.is_empty() {
        return None;
    }

    let replacement = if matches.len() == 1 {
        matches[0].to_string()
    } else {
        let names: Vec<&str> = matches.iter().map(|m| m.trim_matches('%')).collect();
        let common = common_prefix(&names);
        format!("%{}", common)
    };

    let mut out = String::with_capacity(text.len() + replacement.len());
    out.push_str(&text[..percent]);
    out.push_str(&replacement);
    out.push_str(&text[cursor..]);
    let new_cursor = percent + replacement.len();
    Some((out, new_cursor))
}

fn complete_path_text(
    text: &str,
    cursor: usize,
    base_dir: Option<&Path>,
    append_dir_separator: bool,
) -> Option<(String, usize)> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let before = &text[..cursor];
    let component_start = before
        .char_indices()
        .rev()
        .find_map(|(idx, ch)| if ch == '/' || ch == std::path::MAIN_SEPARATOR { Some(idx + ch.len_utf8()) } else { None })
        .unwrap_or(0);
    let typed = &before[component_start..];
    let prefix_path = &before[..component_start];

    let (scan_dir, visible_prefix) = if let Some(base) = base_dir {
        (base.to_path_buf(), prefix_path.to_string())
    } else {
        let prefix = if prefix_path.is_empty() { "." } else { prefix_path };
        let expanded = expand_tilde(prefix);
        (PathBuf::from(expanded), prefix_path.to_string())
    };

    let mut candidates = Vec::new();
    let read_dir = std::fs::read_dir(&scan_dir).ok()?;
    for entry in read_dir.take(4096).flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(typed) {
            continue;
        }
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        candidates.push((name, is_dir));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.dedup_by(|a, b| a.0 == b.0);
    if candidates.is_empty() {
        return None;
    }

    let names: Vec<&str> = candidates.iter().map(|(name, _)| name.as_str()).collect();
    let completed_name = if candidates.len() == 1 {
        let (name, is_dir) = &candidates[0];
        if *is_dir && append_dir_separator {
            format!("{}{}", name, std::path::MAIN_SEPARATOR)
        } else {
            name.clone()
        }
    } else {
        common_prefix(&names)
    };
    if completed_name == typed {
        return None;
    }

    let mut out = String::with_capacity(text.len() + completed_name.len());
    out.push_str(&visible_prefix);
    out.push_str(&completed_name);
    out.push_str(&text[cursor..]);
    let new_cursor = visible_prefix.len() + completed_name.len();
    Some((out, new_cursor))
}

fn common_prefix(parts: &[&str]) -> String {
    if parts.is_empty() {
        return String::new();
    }
    let mut prefix = String::new();
    for (idx, ch) in parts[0].char_indices() {
        let next = idx + ch.len_utf8();
        let candidate = &parts[0][..next];
        if parts.iter().all(|part| part.starts_with(candidate)) {
            prefix.push(ch);
        } else {
            break;
        }
    }
    prefix
}

fn expand_tilde(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}{}", home, &path[1..]);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_insert_and_cursor() {
        let mut s = TextInputState::empty();
        s.insert_char('h');
        s.insert_char('i');
        assert_eq!(s.text, "hi");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn multibyte_insert_and_cursor() {
        let mut s = TextInputState::empty();
        s.insert_char('é'); // 2 bytes
        s.insert_char('a');
        assert_eq!(s.text, "éa");
        assert_eq!(s.cursor, 3); // 2 + 1
    }

    #[test]
    fn backspace_over_multibyte() {
        let mut s = TextInputState::new("café".to_string());
        assert_eq!(s.cursor, 5); // "café" is 5 bytes
        s.backspace(); // should remove é (2 bytes)
        assert_eq!(s.text, "caf");
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn cursor_left_over_multibyte() {
        let mut s = TextInputState::new("a日".to_string()); // 1 + 3 bytes
        assert_eq!(s.cursor, 4);
        s.cursor_left();
        assert_eq!(s.cursor, 1); // back to after 'a'
        s.cursor_left();
        assert_eq!(s.cursor, 0);
        s.cursor_left();
        assert_eq!(s.cursor, 0); // no-op
    }

    #[test]
    fn view_empty() {
        let s = TextInputState::empty();
        let (view, col) = s.view(10);
        assert_eq!(view, "");
        assert_eq!(col, 0);
    }

    #[test]
    fn view_fits() {
        let s = TextInputState::new("hello".to_string());
        let (view, col) = s.view(10);
        assert_eq!(view, "hello");
        assert_eq!(col, 5);
    }

    #[test]
    fn view_scrolled_to_end() {
        let mut s = TextInputState::new("0123456789".to_string());
        s.cursor_end();
        let (view, col) = s.view(5);
        // cursor at col 10, width 5, scroll = 10 - 5 + 1 = 6
        // visible: chars[6..11] = "6789"... but only 4 chars left, take 5 gives "6789"
        // Actually "0123456789".chars().skip(6).take(5) = "6789" (4 chars)
        assert_eq!(view, "6789");
        assert_eq!(col, 4); // 10 - 6
    }

    #[test]
    fn view_cursor_at_start_long_text() {
        let mut s = TextInputState::new("0123456789".to_string());
        s.cursor_home();
        let (view, col) = s.view(5);
        assert_eq!(view, "01234");
        assert_eq!(col, 0);
    }

    // ── delete_word_back / delete_word_forward / kill_to_* ──

    #[test]
    fn delete_word_back_simple() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor_end();
        s.delete_word_back();
        assert_eq!(s.text, "hello ");
        assert_eq!(s.cursor, 6);
    }

    #[test]
    fn delete_word_back_skips_trailing_whitespace() {
        let mut s = TextInputState::new("hello world  ".to_string());
        s.cursor_end();
        s.delete_word_back();
        // Skips trailing whitespace then "world", leaves "hello "
        assert_eq!(s.text, "hello ");
        assert_eq!(s.cursor, 6);
    }

    #[test]
    fn delete_word_back_at_start() {
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_home();
        s.delete_word_back();
        assert_eq!(s.text, "hello");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn delete_word_back_only_whitespace() {
        let mut s = TextInputState::new("   ".to_string());
        s.cursor_end();
        s.delete_word_back();
        assert_eq!(s.text, "");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn delete_word_back_multibyte() {
        let mut s = TextInputState::new("café world".to_string());
        s.cursor_end();
        s.delete_word_back();
        assert_eq!(s.text, "café ");
        // "café " is 6 bytes (5 for café, 1 for space)
        assert_eq!(s.cursor, 6);
        s.delete_word_back();
        assert_eq!(s.text, "");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn delete_word_forward_simple() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor_home();
        s.delete_word_forward();
        // Deletes "hello " (word + trailing whitespace), leaves "world"
        assert_eq!(s.text, "world");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn delete_word_forward_at_end() {
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_end();
        s.delete_word_forward();
        assert_eq!(s.text, "hello");
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn delete_word_forward_multibyte() {
        let mut s = TextInputState::new("café 日本語".to_string());
        s.cursor_home();
        s.delete_word_forward();
        // "café " (6 bytes) deleted, leaves "日本語"
        assert_eq!(s.text, "日本語");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn kill_to_start_basic() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor = 6; // after "hello "
        s.kill_to_start();
        assert_eq!(s.text, "world");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn kill_to_start_at_start_noop() {
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_home();
        s.kill_to_start();
        assert_eq!(s.text, "hello");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn kill_to_end_basic() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor = 5; // after "hello"
        s.kill_to_end();
        assert_eq!(s.text, "hello");
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn kill_to_end_at_end_noop() {
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_end();
        s.kill_to_end();
        assert_eq!(s.text, "hello");
        assert_eq!(s.cursor, 5);
    }

    // ── handle_text_input_key dispatch ──

    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn ctrl_a_moves_to_home() {
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_end();
        let consumed =
            handle_text_input_key(&mut s, &key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(consumed);
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn ctrl_e_moves_to_end() {
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_home();
        handle_text_input_key(&mut s, &key(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(s.cursor, 5);
    }

    #[test]
    fn ctrl_w_deletes_word_back() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor_end();
        handle_text_input_key(&mut s, &key(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(s.text, "hello ");
    }

    #[test]
    fn ctrl_backspace_deletes_word_back() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor_end();
        handle_text_input_key(&mut s, &key(KeyCode::Backspace, KeyModifiers::CONTROL));
        assert_eq!(s.text, "hello ");
    }

    #[test]
    fn alt_backspace_deletes_word_back() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor_end();
        handle_text_input_key(&mut s, &key(KeyCode::Backspace, KeyModifiers::ALT));
        assert_eq!(s.text, "hello ");
    }

    #[test]
    fn ctrl_u_kills_to_start() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor = 6;
        handle_text_input_key(&mut s, &key(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(s.text, "world");
    }

    #[test]
    fn ctrl_k_kills_to_end() {
        let mut s = TextInputState::new("hello world".to_string());
        s.cursor = 5;
        handle_text_input_key(&mut s, &key(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(s.text, "hello");
    }

    #[test]
    fn unknown_ctrl_letter_is_ignored() {
        // Pre-fix bug: Ctrl+X used to insert literal 'x'. Now it should be a no-op.
        let mut s = TextInputState::new("hi".to_string());
        s.cursor_end();
        let consumed =
            handle_text_input_key(&mut s, &key(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(!consumed);
        assert_eq!(s.text, "hi");
    }

    #[test]
    fn shift_letter_still_inserts() {
        // Capitalized chars (SHIFT modifier set) must still be inserted normally.
        let mut s = TextInputState::empty();
        handle_text_input_key(&mut s, &key(KeyCode::Char('A'), KeyModifiers::SHIFT));
        assert_eq!(s.text, "A");
    }

    #[test]
    fn ctrl_shift_letter_uses_lowercase_binding() {
        // Ctrl+Shift+A should still trigger cursor_home (the Ctrl binding wins).
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_end();
        handle_text_input_key(
            &mut s,
            &key(
                KeyCode::Char('A'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn template_completion_unique_variable() {
        let mut s = TextInputState::new("%ART".to_string());
        assert!(apply_tab_completion(&mut s, CompletionMode::TemplateVariable));
        assert_eq!(s.text, "%ARTIST%");
        assert_eq!(s.cursor, "%ARTIST%".len());
    }

    #[test]
    fn template_completion_ambiguous_keeps_common_prefix() {
        let mut s = TextInputState::new("%AL".to_string());
        assert!(apply_tab_completion(&mut s, CompletionMode::TemplateVariable));
        assert_eq!(s.text, "%ALBUM");
    }

    #[test]
    fn path_completion_unique_file_against_temp_filesystem() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("track-one.flac"), b"audio").expect("fixture");
        std::fs::write(temp.path().join("other.flac"), b"audio").expect("fixture");

        let mut input = TextInputState::new(temp.path().join("tra").display().to_string());
        assert!(apply_tab_completion(&mut input, CompletionMode::Path));
        assert_eq!(input.text, temp.path().join("track-one.flac").display().to_string());
        assert_eq!(input.cursor, input.text.len());
    }

    #[test]
    fn path_completion_appends_separator_for_unique_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(temp.path().join("Album One")).expect("fixture");

        let mut input = TextInputState::new(temp.path().join("Alb").display().to_string());
        assert!(apply_tab_completion(&mut input, CompletionMode::Path));
        assert_eq!(
            input.text,
            format!(
                "{}{}",
                temp.path().join("Album One").display(),
                std::path::MAIN_SEPARATOR
            )
        );
        assert_eq!(input.cursor, input.text.len());
    }

    #[test]
    fn path_completion_common_prefix_for_multiple_matches() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("cover-front.jpg"), b"image").expect("fixture");
        std::fs::write(temp.path().join("cover-back.jpg"), b"image").expect("fixture");

        let mut input = TextInputState::new(temp.path().join("co").display().to_string());
        assert!(apply_tab_completion(&mut input, CompletionMode::Path));
        assert_eq!(input.text, temp.path().join("cover-").display().to_string());
        assert_eq!(input.cursor, input.text.len());
    }

    #[test]
    fn browse_rename_completion_uses_base_dir_without_inserting_prefix() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("track-one.flac"), b"audio").expect("fixture");

        let mut input = TextInputState::new("tra".to_string());
        assert!(apply_path_completion(&mut input, Some(temp.path()), false));
        assert_eq!(input.text, "track-one.flac");
        assert_eq!(input.cursor, "track-one.flac".len());
    }

}
