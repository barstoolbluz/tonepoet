//! Shared text input state with UTF-8 safe cursor movement and horizontal scrolling

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
}

impl TextInputState {
    /// Create a new input with the given initial text, cursor at end.
    pub fn new(initial: String) -> Self {
        let cursor = initial.len();
        Self {
            text: initial,
            cursor,
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

/// Handle a key event for a text input. Returns `true` if the key was consumed.
///
/// Handles: Char, Backspace, Delete, Left, Right, Home, End.
/// Does NOT handle Enter or Esc — those are overlay-specific.
pub fn handle_text_input_key(input: &mut TextInputState, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(c) => {
            input.insert_char(c);
            true
        }
        KeyCode::Backspace => {
            input.backspace();
            true
        }
        KeyCode::Delete => {
            input.delete();
            true
        }
        KeyCode::Left => {
            input.cursor_left();
            true
        }
        KeyCode::Right => {
            input.cursor_right();
            true
        }
        KeyCode::Home => {
            input.cursor_home();
            true
        }
        KeyCode::End => {
            input.cursor_end();
            true
        }
        _ => false,
    }
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
}
