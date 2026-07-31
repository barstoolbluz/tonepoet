//! Shared text input state with UTF-8 safe cursor movement and horizontal scrolling

use crossterm::event::{KeyCode, KeyEvent};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static TEXT_INPUT_CLIPBOARD: OnceLock<Mutex<String>> = OnceLock::new();
static TEXT_INPUT_CLIPBOARD_PUBLISH_HOOK: OnceLock<fn(&str)> = OnceLock::new();

thread_local! {
    /// Optional thread-scoped clipboard used by tests that need an atomic
    /// setup/dispatch/assertion sequence without contending on process-global
    /// clipboard state. Production callers never install an override.
    static SCOPED_TEXT_INPUT_CLIPBOARD: RefCell<Option<String>> = RefCell::new(None);
    static SCOPED_TEXT_INPUT_CLIPBOARD_PUBLISH_HOOK: RefCell<Option<Box<dyn Fn(&str)>>> = RefCell::new(None);
}

fn shared_text_input_clipboard() -> &'static Mutex<String> {
    TEXT_INPUT_CLIPBOARD.get_or_init(|| Mutex::new(String::new()))
}

/// Replace the process-wide text clipboard shared by every picker/editor text
/// input. Poisoning cannot make clipboard access permanently unavailable: the
/// recovered value is replaced atomically under the same mutex.
pub fn write_shared_text_clipboard(text: impl Into<String>) {
    let text = text.into();
    let handled_scoped = SCOPED_TEXT_INPUT_CLIPBOARD.with(|scoped| {
        let mut scoped = scoped.borrow_mut();
        if scoped.is_some() {
            *scoped = Some(text.clone());
            true
        } else {
            false
        }
    });
    if !handled_scoped {
        let mut clipboard = shared_text_input_clipboard()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *clipboard = text.clone();
    }

    let handled_scoped_hook = SCOPED_TEXT_INPUT_CLIPBOARD_PUBLISH_HOOK.with(|hook| {
        let hook = hook.borrow();
        if let Some(hook) = hook.as_ref() {
            hook(&text);
            true
        } else {
            false
        }
    });
    if !handled_scoped && !handled_scoped_hook {
        if let Some(hook) = TEXT_INPUT_CLIPBOARD_PUBLISH_HOOK.get() {
            hook(&text);
        }
    }
}

/// Install the host's best-effort system-clipboard publisher. The first
/// installation wins for the process lifetime; repeated startup calls are
/// harmless and cannot replace an established authority.
pub fn set_shared_clipboard_publish_hook(hook: fn(&str)) -> bool {
    TEXT_INPUT_CLIPBOARD_PUBLISH_HOOK.set(hook).is_ok()
}

/// Read an exact snapshot of the process-wide text clipboard.
pub fn read_shared_text_clipboard() -> String {
    if let Some(value) = SCOPED_TEXT_INPUT_CLIPBOARD.with(|scoped| scoped.borrow().clone()) {
        return value;
    }
    let clipboard = shared_text_input_clipboard()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    clipboard.clone()
}

/// Run a closure with a thread-scoped shared clipboard. This is intended for
/// deterministic tests that exercise production clipboard routes in parallel.
/// The previous scoped value is restored even if the closure panics.
#[doc(hidden)]
pub fn with_scoped_shared_text_clipboard<R>(
    initial: impl Into<String>,
    f: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<String>);

    impl Drop for Restore {
        fn drop(&mut self) {
            SCOPED_TEXT_INPUT_CLIPBOARD.with(|scoped| {
                scoped.replace(self.0.take());
            });
        }
    }

    let previous = SCOPED_TEXT_INPUT_CLIPBOARD.with(|scoped| {
        scoped.replace(Some(initial.into()))
    });
    let _restore = Restore(previous);
    f()
}

/// Run a closure with a thread-scoped publication hook. This is a deterministic
/// test seam for copy/cut paths and never changes the process-global hook.
#[doc(hidden)]
pub fn with_scoped_shared_text_clipboard_publish_hook<R>(
    hook: impl Fn(&str) + 'static,
    f: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<Box<dyn Fn(&str)>>);

    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            SCOPED_TEXT_INPUT_CLIPBOARD_PUBLISH_HOOK.with(|hook| {
                *hook.borrow_mut() = previous;
            });
        }
    }

    let previous = SCOPED_TEXT_INPUT_CLIPBOARD_PUBLISH_HOOK.with(|slot| {
        slot.borrow_mut().replace(Box::new(hook))
    });
    let _restore = Restore(previous);
    f()
}

/// State for a single-line text input field.
///
/// The cursor is a byte offset into `text` that is always on a UTF-8 char boundary.
/// Movement methods (`cursor_left`, `cursor_right`, `backspace`, `delete`, etc.)
/// walk char boundaries to avoid panics on multibyte input.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TextInputSnapshot {
    text: String,
    cursor: usize,
    select_all: bool,
    selection_anchor: Option<usize>,
}

const TEXT_INPUT_HISTORY_LIMIT: usize = 128;

#[derive(Debug, Clone)]
pub struct TextInputState {
    pub text: String,
    /// Byte offset into `text`; always on a UTF-8 char boundary.
    pub cursor: usize,
    /// When true, the entire text is selected. The next character input
    /// replaces all text; navigation keys clear the selection.
    pub select_all: bool,
    /// Byte offset where selection started. Selection is active when this is
    /// Some and differs from `cursor`; both endpoints are kept on UTF-8
    /// boundaries.
    pub selection_anchor: Option<usize>,
    /// Internal per-field clipboard for terminal environments where the host
    /// clipboard is unavailable to the TUI.
    pub clipboard: String,
    undo_history: Vec<TextInputSnapshot>,
    redo_history: Vec<TextInputSnapshot>,
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

/// Boundary semantics for modified cursor/selection movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextBoundaryMode {
    /// Whitespace-delimited words, suitable for ordinary names and text fields.
    Word,
    /// Filesystem path segments. Separators form explicit boundaries.
    PathSegment,
}

impl Default for TextBoundaryMode {
    fn default() -> Self {
        Self::Word
    }
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
            selection_anchor: None,
            clipboard: String::new(),
            undo_history: Vec::new(),
            redo_history: Vec::new(),
        }
    }

    /// Create a new input with all text selected.
    pub fn new_selected(initial: String) -> Self {
        let cursor = initial.len();
        Self {
            text: initial,
            cursor,
            select_all: true,
            selection_anchor: Some(0),
            clipboard: String::new(),
            undo_history: Vec::new(),
            redo_history: Vec::new(),
        }
    }

    /// Create an empty input.
    pub fn empty() -> Self {
        Self::new(String::new())
    }

    fn snapshot(&self) -> TextInputSnapshot {
        TextInputSnapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            select_all: self.select_all,
            selection_anchor: self.selection_anchor,
        }
    }

    fn restore_snapshot(&mut self, snapshot: TextInputSnapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor.min(self.text.len());
        while self.cursor > 0 && !self.text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
        self.select_all = snapshot.select_all;
        self.selection_anchor = snapshot
            .selection_anchor
            .filter(|anchor| *anchor <= self.text.len() && self.text.is_char_boundary(*anchor));
    }

    fn push_bounded(history: &mut Vec<TextInputSnapshot>, snapshot: TextInputSnapshot) {
        if history.len() == TEXT_INPUT_HISTORY_LIMIT {
            history.remove(0);
        }
        history.push(snapshot);
    }

    fn record_edit(&mut self, before: TextInputSnapshot) -> bool {
        if before.text == self.text {
            return false;
        }
        Self::push_bounded(&mut self.undo_history, before);
        self.redo_history.clear();
        true
    }

    /// Undo the most recent text mutation in this field.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo_history.pop() else {
            return false;
        };
        let current = self.snapshot();
        Self::push_bounded(&mut self.redo_history, current);
        self.restore_snapshot(previous);
        true
    }

    /// Redo the most recently undone text mutation in this field.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_history.pop() else {
            return false;
        };
        let current = self.snapshot();
        Self::push_bounded(&mut self.undo_history, current);
        self.restore_snapshot(next);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_history.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_history.is_empty()
    }

    pub fn selection_range(&self) -> Option<std::ops::Range<usize>> {
        if self.select_all && !self.text.is_empty() {
            return Some(0..self.text.len());
        }
        let anchor = self.selection_anchor?;
        if anchor == self.cursor {
            return None;
        }
        let (start, end) = if anchor < self.cursor { (anchor, self.cursor) } else { (self.cursor, anchor) };
        if self.text.is_char_boundary(start) && self.text.is_char_boundary(end) {
            Some(start..end)
        } else {
            None
        }
    }

    pub fn has_selection(&self) -> bool {
        self.selection_range().is_some()
    }

    pub fn clear_selection(&mut self) {
        self.select_all = false;
        self.selection_anchor = None;
    }

    pub fn select_all_text(&mut self) {
        self.cursor = self.text.len();
        self.select_all = true;
        self.selection_anchor = Some(0);
    }

    fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection_range() else {
            self.clear_selection();
            return false;
        };
        let start = range.start;
        self.text.replace_range(range, "");
        self.cursor = start;
        self.clear_selection();
        true
    }

    pub fn copy_selection(&mut self) -> bool {
        let copied = match self.selection_range() {
            Some(range) => self.text[range].to_string(),
            None if self.text.is_empty() => return false,
            None => self.text.clone(),
        };
        self.clipboard = copied.clone();
        write_shared_text_clipboard(copied);
        true
    }

    pub fn cut_selection(&mut self) -> bool {
        if !self.has_selection() || !self.copy_selection() {
            return false;
        }
        let before = self.snapshot();
        self.delete_selection();
        self.record_edit(before)
    }

    pub fn can_paste(&self) -> bool {
        if !self.clipboard.is_empty() {
            return true;
        }
        !read_shared_text_clipboard().is_empty()
    }

    pub fn paste_clipboard(&mut self) -> bool {
        let shared = read_shared_text_clipboard();
        if !shared.is_empty() {
            self.clipboard = shared;
        }
        if self.clipboard.is_empty() {
            return false;
        }
        let clipboard = self.clipboard.clone();
        self.insert_string(&clipboard);
        true
    }

    /// Transform the current selection, or the entire value when no selection
    /// is active. The transformed range remains selected so repeated case
    /// commands are deterministic even when Unicode case mapping changes the
    /// byte length (for example, `ß` -> `SS`).
    pub fn transform_selection_or_all(
        &mut self,
        transform: impl FnOnce(&str) -> String,
    ) -> bool {
        let before = self.snapshot();
        let had_selection = self.has_selection();
        let range = self.selection_range().unwrap_or(0..self.text.len());
        if !self.text.is_char_boundary(range.start) || !self.text.is_char_boundary(range.end) {
            return false;
        }
        let replacement = transform(&self.text[range.clone()]);
        let start = range.start;
        self.text.replace_range(range, &replacement);
        self.selection_anchor = Some(start);
        self.cursor = start + replacement.len();
        self.select_all = !had_selection && start == 0 && self.cursor == self.text.len();
        self.record_edit(before)
    }

    /// Insert a character at the cursor and advance the cursor.
    pub fn insert_char(&mut self, c: char) {
        let before = self.snapshot();
        self.delete_selection();
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        self.record_edit(before);
    }

    /// Insert a string at the cursor and advance the cursor past it.
    pub fn insert_string(&mut self, s: &str) {
        if s.is_empty() && !self.has_selection() {
            return;
        }
        let before = self.snapshot();
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.record_edit(before);
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
        let before = self.snapshot();
        self.text.replace_range(range.clone(), replacement);
        self.cursor = range.start + replacement.len();
        self.clear_selection();
        self.record_edit(before);
    }

    /// Replace the full text and clamp the cursor to a valid char boundary.
    pub fn set_text_and_cursor(&mut self, text: String, cursor: usize) {
        let before = self.snapshot();
        self.text = text;
        self.cursor = cursor.min(self.text.len());
        while self.cursor > 0 && !self.text.is_char_boundary(self.cursor) {
            self.cursor -= 1;
        }
        self.clear_selection();
        self.record_edit(before);
    }

    /// Delete the character before the cursor (Backspace behavior).
    pub fn backspace(&mut self) {
        let before = self.snapshot();
        if self.delete_selection() {
            self.record_edit(before);
            return;
        }
        if let Some(prev) = self.prev_char_boundary() {
            self.text.remove(prev);
            self.cursor = prev;
        }
        self.record_edit(before);
    }

    /// Delete the character at the cursor (Delete key behavior).
    pub fn delete(&mut self) {
        let before = self.snapshot();
        if self.delete_selection() {
            self.record_edit(before);
            return;
        }
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
        self.record_edit(before);
    }

    /// Move cursor one char left.
    pub fn cursor_left(&mut self) {
        self.clear_selection();
        if let Some(prev) = self.prev_char_boundary() {
            self.cursor = prev;
        }
    }

    /// Move cursor one char right.
    pub fn cursor_right(&mut self) {
        self.clear_selection();
        if let Some(next) = self.next_char_boundary() {
            self.cursor = next;
        }
    }

    pub fn cursor_home(&mut self) {
        self.clear_selection();
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.clear_selection();
        self.cursor = self.text.len();
    }

    pub fn cursor_word_left(&mut self) {
        self.clear_selection();
        self.move_word_left(false);
    }

    pub fn cursor_word_right(&mut self) {
        self.clear_selection();
        self.move_word_right(false);
    }

    pub fn extend_left(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        if let Some(prev) = self.prev_char_boundary() {
            self.cursor = prev;
        }
        self.select_all = false;
    }

    pub fn extend_right(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        if let Some(next) = self.next_char_boundary() {
            self.cursor = next;
        }
        self.select_all = false;
    }

    pub fn extend_word_left(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.move_word_left(true);
    }

    pub fn extend_word_right(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.move_word_right(true);
    }

    pub fn extend_path_segment_left(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.move_path_segment_left(true);
    }

    pub fn extend_path_segment_right(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.move_path_segment_right(true);
    }

    fn move_path_segment_left(&mut self, keep_selection: bool) {
        if self.cursor == 0 {
            return;
        }
        // Cross separators immediately to land at the previous segment, then
        // scan to that segment's beginning. This mirrors standard path editors.
        while self.cursor > 0 {
            let prev = self.prev_char_boundary().expect("cursor has previous boundary");
            let c = self.text[prev..self.cursor].chars().next().expect("one char");
            if !is_path_separator(c) {
                break;
            }
            self.cursor = prev;
        }
        while self.cursor > 0 {
            let prev = self.prev_char_boundary().expect("cursor has previous boundary");
            let c = self.text[prev..self.cursor].chars().next().expect("one char");
            if is_path_separator(c) {
                break;
            }
            self.cursor = prev;
        }
        if !keep_selection {
            self.clear_selection();
        }
        self.select_all = false;
    }

    fn move_path_segment_right(&mut self, keep_selection: bool) {
        if self.cursor >= self.text.len() {
            return;
        }
        while self.cursor < self.text.len() {
            let next = next_boundary_after(&self.text, self.cursor).expect("cursor has next boundary");
            let c = self.text[self.cursor..next].chars().next().expect("one char");
            self.cursor = next;
            if is_path_separator(c) {
                break;
            }
        }
        while self.cursor < self.text.len() {
            let next = next_boundary_after(&self.text, self.cursor).expect("cursor has next boundary");
            let c = self.text[self.cursor..next].chars().next().expect("one char");
            if !is_path_separator(c) {
                break;
            }
            self.cursor = next;
        }
        if !keep_selection {
            self.clear_selection();
        }
        self.select_all = false;
    }

    fn move_word_left(&mut self, keep_selection: bool) {
        if self.cursor == 0 {
            return;
        }
        while self.cursor > 0 {
            let prev = self.prev_char_boundary().unwrap();
            let c = self.text[prev..self.cursor].chars().next().unwrap();
            if !c.is_whitespace() {
                break;
            }
            self.cursor = prev;
        }
        while self.cursor > 0 {
            let prev = self.prev_char_boundary().unwrap();
            let c = self.text[prev..self.cursor].chars().next().unwrap();
            if c.is_whitespace() {
                break;
            }
            self.cursor = prev;
        }
        if !keep_selection {
            self.clear_selection();
        }
    }

    fn move_word_right(&mut self, keep_selection: bool) {
        if self.cursor >= self.text.len() {
            return;
        }
        while self.cursor < self.text.len() {
            let next = next_boundary_after(&self.text, self.cursor).unwrap();
            let c = self.text[self.cursor..next].chars().next().unwrap();
            self.cursor = next;
            if c.is_whitespace() {
                break;
            }
        }
        while self.cursor < self.text.len() {
            let next = next_boundary_after(&self.text, self.cursor).unwrap();
            let c = self.text[self.cursor..next].chars().next().unwrap();
            if !c.is_whitespace() {
                break;
            }
            self.cursor = next;
        }
        if !keep_selection {
            self.clear_selection();
        }
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
        let before = self.snapshot();
        if self.delete_selection() {
            self.record_edit(before);
            return;
        }
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
        self.record_edit(before);
    }

    /// Walk forward from `cursor` skipping non-whitespace then whitespace,
    /// then delete the resulting range. Cursor stays at its current position.
    pub fn delete_word_forward(&mut self) {
        let before = self.snapshot();
        if self.delete_selection() {
            self.record_edit(before);
            return;
        }
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
        self.record_edit(before);
    }

    /// Delete everything from cursor back to the start of the input.
    pub fn kill_to_start(&mut self) {
        let before = self.snapshot();
        if self.delete_selection() {
            self.record_edit(before);
            return;
        }
        if self.cursor == 0 {
            return;
        }
        self.text.drain(..self.cursor);
        self.cursor = 0;
        self.record_edit(before);
    }

    /// Delete everything from cursor to the end of the input.
    pub fn kill_to_end(&mut self) {
        let before = self.snapshot();
        if self.delete_selection() {
            self.record_edit(before);
            return;
        }
        if self.cursor < self.text.len() {
            self.text.truncate(self.cursor);
        }
        self.record_edit(before);
    }

    /// Number of terminal display columns from the start of the text to the
    /// cursor byte offset. Wide glyphs consume two cells and combining marks
    /// consume zero, matching the renderer's shared width policy.
    pub fn cursor_display_col(&self) -> usize {
        crate::display_width::width(&self.text[..self.cursor])
    }

    /// Compute the UTF-8 byte range visible in a field of `width` terminal
    /// cells and the cursor column within that range.
    pub fn view_range(&self, width: usize) -> (std::ops::Range<usize>, u16) {
        if width == 0 {
            return (0..0, 0);
        }
        let cursor_col = self.cursor_display_col();
        let desired_start = cursor_col.saturating_sub(width.saturating_sub(1));

        let mut start_byte = 0usize;
        let mut start_col = 0usize;
        if desired_start > 0 {
            let mut col = 0usize;
            let mut candidate = self.cursor;
            let mut candidate_col = cursor_col;
            for (byte, ch) in self.text.char_indices() {
                let cell_width = crate::display_width::char_width(ch);
                if col >= desired_start && cell_width > 0 {
                    candidate = byte;
                    candidate_col = col;
                    break;
                }
                col = col.saturating_add(cell_width);
            }
            start_byte = candidate.min(self.cursor);
            start_col = candidate_col.min(cursor_col);
        }

        let mut used = 0usize;
        let mut end_byte = start_byte;
        for (relative_byte, ch) in self.text[start_byte..].char_indices() {
            let cell_width = crate::display_width::char_width(ch);
            if used.saturating_add(cell_width) > width {
                break;
            }
            used = used.saturating_add(cell_width);
            end_byte = start_byte + relative_byte + ch.len_utf8();
        }
        let cursor_col_in_view = cursor_col.saturating_sub(start_col).min(width) as u16;
        (start_byte..end_byte, cursor_col_in_view)
    }

    /// Compute a scrolled view of the text for rendering.
    ///
    /// Returns `(visible_text, cursor_col_in_view)` where `visible_text` fits
    /// within `width` terminal cells and keeps the cursor visible. Scrolling
    /// never begins in the middle of a wide glyph or with an orphan combining
    /// mark.
    pub fn view(&self, width: usize) -> (String, u16) {
        let (range, cursor_col) = self.view_range(width);
        (self.text[range].to_string(), cursor_col)
    }

    /// Resolve a terminal-cell column in the currently visible field to a
    /// UTF-8 byte boundary in the full input text.
    ///
    /// The mapping uses the same horizontal-scroll and display-width policy as
    /// [`Self::view_range`]. Clicking the first cell of a wide glyph places the
    /// cursor before it; clicking its later cell places the cursor after it.
    /// Columns outside the field clamp to the nearest visible boundary.
    pub fn byte_index_for_view_column(&self, width: usize, column: usize) -> usize {
        if width == 0 {
            return self.cursor.min(self.text.len());
        }

        let (range, _) = self.view_range(width);
        let target = column.min(width);
        let mut display_col = 0usize;
        let mut last_boundary = range.start;

        for (relative_byte, ch) in self.text[range.clone()].char_indices() {
            let byte = range.start + relative_byte;
            let next = byte + ch.len_utf8();
            let cell_width = crate::display_width::char_width(ch);

            if cell_width == 0 {
                last_boundary = next;
                continue;
            }
            if target <= display_col {
                return byte;
            }
            let cell_end = display_col.saturating_add(cell_width);
            if target < cell_end {
                let offset = target.saturating_sub(display_col);
                return if offset.saturating_mul(2) < cell_width {
                    byte
                } else {
                    next
                };
            }

            display_col = cell_end;
            last_boundary = next;
        }

        last_boundary.min(range.end)
    }

    /// Place the cursor from a mouse column and clear any prior selection.
    pub fn place_cursor_from_view_column(&mut self, width: usize, column: usize) {
        self.cursor = self.byte_index_for_view_column(width, column);
        self.clear_selection();
    }

    /// Begin a mouse-drag selection at a display column. A simple click leaves
    /// an empty anchor which is collapsed when the button is released.
    pub fn begin_mouse_selection(&mut self, width: usize, column: usize) {
        let cursor = self.byte_index_for_view_column(width, column);
        self.cursor = cursor;
        self.select_all = false;
        self.selection_anchor = Some(cursor);
    }

    /// Extend an in-progress mouse selection to a display column.
    pub fn drag_mouse_selection(&mut self, width: usize, column: usize) {
        let anchor = self.selection_anchor.unwrap_or(self.cursor);
        self.cursor = self.byte_index_for_view_column(width, column);
        self.select_all = false;
        self.selection_anchor = Some(anchor);
    }

    /// Finish a mouse selection. A click without movement becomes an ordinary
    /// insertion cursor rather than retaining an empty selection anchor.
    pub fn end_mouse_selection(&mut self) {
        if self.selection_anchor == Some(self.cursor) {
            self.selection_anchor = None;
        }
        self.select_all = false;
    }

}

/// Walk forward from a byte index, returning the next char boundary.
/// Standalone helper so it can be used without holding a `&mut TextInputState`.
fn is_path_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

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
/// Standard selection plus readline-style Ctrl bindings (case-insensitive):
///   Ctrl+A or Alt+L=select all; Ctrl+/ or Ctrl+_=deselect;
///   Ctrl+Z or Alt+Z=undo; Ctrl+Y, Ctrl+Shift+Z, or Alt+Y=redo;
///   Ctrl+E=end, Ctrl+B=left, Ctrl+F=right,
///   Ctrl+H=backspace, Ctrl+D=delete-fwd,
///   Ctrl+W=delete-prev-word, Ctrl+U=kill-to-start, Ctrl+K=kill-to-end.
/// Word-deletion alternatives: Ctrl+Backspace and Alt+Backspace also delete
/// the previous word; Ctrl+Delete and Alt+D delete the next word.
/// Unrecognized Ctrl/Alt combos are ignored (NOT inserted as literal chars).
/// Does NOT handle Enter or Esc — those are overlay-specific.
pub fn handle_text_input_key(input: &mut TextInputState, key: &KeyEvent) -> bool {
    handle_text_input_key_with_boundaries(input, key, TextBoundaryMode::Word)
}

/// Handle a key using explicit modified-movement boundary semantics.
pub fn handle_text_input_key_with_boundaries(
    input: &mut TextInputState,
    key: &KeyEvent,
    boundary_mode: TextBoundaryMode,
) -> bool {
    use crossterm::event::KeyModifiers as M;

    let ctrl = key.modifiers.contains(M::CONTROL);
    let alt = key.modifiers.contains(M::ALT);
    let shift = key.modifiers.contains(M::SHIFT);

    // Select-all handling: typing replaces all text, destructive keys delete it,
    // movement collapses the selection unless Shift is extending it.
    if input.select_all {
        match key.code {
            KeyCode::Char(c) if !ctrl && !alt => {
                input.insert_char(c);
                return true;
            }
            KeyCode::Backspace => {
                input.backspace();
                return true;
            }
            KeyCode::Delete => {
                input.delete();
                return true;
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End if !shift => {
                input.clear_selection();
            }
            _ => {}
        }
    }

    match (ctrl, alt, shift, key.code) {
        // Standard text-editor select all plus an Alt+L alternative for
        // terminal multiplexers that reserve Ctrl+A.
        (true, false, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'a') => {
            input.select_all_text();
            true
        }
        (false, true, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'l') => {
            input.select_all_text();
            true
        }

        // Ctrl+/ is transmitted as 0x1F. Crossterm reports that byte as either
        // Ctrl+'/' or Ctrl+'_' depending on the terminal; both clear selection
        // without moving the cursor.
        (true, false, _, KeyCode::Char('/')) | (true, false, _, KeyCode::Char('_')) => {
            input.clear_selection();
            true
        }

        // Per-field edit history. Ctrl+Shift+Z and Ctrl+Y redo; Alt+Z/Y
        // provide terminal-multiplexer-safe alternatives.
        (true, false, true, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'z') => {
            input.redo();
            true
        }
        (true, false, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'z') => {
            input.undo();
            true
        }
        (true, false, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'y') => {
            input.redo();
            true
        }
        (false, true, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'z') => {
            input.undo();
            true
        }
        (false, true, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'y') => {
            input.redo();
            true
        }

        // Clipboard commands.
        (true, false, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'c') => {
            input.copy_selection()
        }
        (true, false, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'x') => {
            input.cut_selection()
        }
        (true, false, _, KeyCode::Char(c))
            if c.eq_ignore_ascii_case(&'v') || c.eq_ignore_ascii_case(&'p') =>
        {
            input.paste_clipboard()
        }

        // Readline-style movement/editing.
        (true, false, _, KeyCode::Char(c)) => {
            match c.to_ascii_lowercase() {
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
        (true, false, false, KeyCode::Left) => {
            input.cursor_word_left();
            true
        }
        (true, false, false, KeyCode::Right) => {
            input.cursor_word_right();
            true
        }
        (true, false, true, KeyCode::Left) => {
            match boundary_mode {
                TextBoundaryMode::Word => input.extend_word_left(),
                TextBoundaryMode::PathSegment => input.extend_path_segment_left(),
            }
            true
        }
        (true, false, true, KeyCode::Right) => {
            match boundary_mode {
                TextBoundaryMode::Word => input.extend_word_right(),
                TextBoundaryMode::PathSegment => input.extend_path_segment_right(),
            }
            true
        }
        (true, false, _, KeyCode::Home) => {
            if shift {
                if input.selection_anchor.is_none() {
                    input.selection_anchor = Some(input.cursor);
                }
                input.cursor = 0;
                input.select_all = false;
            } else {
                input.cursor_home();
            }
            true
        }
        (true, false, _, KeyCode::End) => {
            if shift {
                if input.selection_anchor.is_none() {
                    input.selection_anchor = Some(input.cursor);
                }
                input.cursor = input.text.len();
                input.select_all = false;
            } else {
                input.cursor_end();
            }
            true
        }
        (true, false, _, KeyCode::Backspace) => {
            input.delete_word_back();
            true
        }
        (true, false, _, KeyCode::Delete) => {
            input.delete_word_forward();
            true
        }
        (true, false, _, _) => false,

        // Alt combos: Alt+Backspace and Alt+D.
        (false, true, _, KeyCode::Backspace) => {
            input.delete_word_back();
            true
        }
        (false, true, _, KeyCode::Char(c)) if c.eq_ignore_ascii_case(&'d') => {
            input.delete_word_forward();
            true
        }
        (false, true, _, _) => false,
        (true, true, _, _) => false,

        // Plain/Shift editing and selection.
        (false, false, _, KeyCode::Char(c)) => {
            input.insert_char(c);
            true
        }
        (false, false, _, KeyCode::Backspace) => {
            input.backspace();
            true
        }
        (false, false, _, KeyCode::Delete) => {
            input.delete();
            true
        }
        (false, false, true, KeyCode::Left) => {
            input.extend_left();
            true
        }
        (false, false, true, KeyCode::Right) => {
            input.extend_right();
            true
        }
        (false, false, false, KeyCode::Left) => {
            input.cursor_left();
            true
        }
        (false, false, false, KeyCode::Right) => {
            input.cursor_right();
            true
        }
        (false, false, _, KeyCode::Home) => {
            if shift {
                if input.selection_anchor.is_none() {
                    input.selection_anchor = Some(input.cursor);
                }
                input.cursor = 0;
                input.select_all = false;
            } else {
                input.cursor_home();
            }
            true
        }
        (false, false, _, KeyCode::End) => {
            if shift {
                if input.selection_anchor.is_none() {
                    input.selection_anchor = Some(input.cursor);
                }
                input.cursor = input.text.len();
                input.select_all = false;
            } else {
                input.cursor_end();
            }
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

    let (scan_dir, visible_prefix) = if prefix_path.starts_with('~')
        || Path::new(prefix_path).is_absolute()
    {
        let prefix = if prefix_path.is_empty() { "." } else { prefix_path };
        let expanded = expand_tilde(prefix);
        (PathBuf::from(expanded), prefix_path.to_string())
    } else if let Some(base) = base_dir {
        let relative_prefix = if prefix_path.is_empty() { "." } else { prefix_path };
        (base.join(relative_prefix), prefix_path.to_string())
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
    fn ctrl_a_selects_all_text() {
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_end();
        let consumed =
            handle_text_input_key(&mut s, &key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert!(consumed);
        assert_eq!(s.selection_range(), Some(0..5));
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
        // Ctrl+Shift+A still selects all text (the Ctrl binding wins).
        let mut s = TextInputState::new("hello".to_string());
        s.cursor_end();
        handle_text_input_key(
            &mut s,
            &key(
                KeyCode::Char('A'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        assert_eq!(s.selection_range(), Some(0..5));
    }

    #[test]
    fn text_edit_history_undo_redo_is_utf8_safe() {
        let mut s = TextInputState::new("café".to_string());
        s.insert_char('日');
        assert_eq!(s.text, "café日");
        assert!(s.can_undo());
        assert!(s.undo());
        assert_eq!(s.text, "café");
        assert_eq!(s.cursor, "café".len());
        assert!(s.redo());
        assert_eq!(s.text, "café日");
        assert_eq!(s.cursor, "café日".len());
    }

    #[test]
    fn new_edit_after_undo_invalidates_redo() {
        let mut s = TextInputState::new("one".to_string());
        s.insert_string(" two");
        assert!(s.undo());
        s.insert_string(" three");
        assert!(!s.can_redo());
        assert!(!s.redo());
        assert_eq!(s.text, "one three");
    }

    #[test]
    fn selection_replacement_is_one_undo_step() {
        let mut s = TextInputState::new_selected("old".to_string());
        s.insert_string("new");
        assert_eq!(s.text, "new");
        assert!(s.undo());
        assert_eq!(s.text, "old");
        assert_eq!(s.selection_range(), Some(0..3));
    }

    #[test]
    fn ctrl_and_alt_history_bindings_are_available() {
        let mut s = TextInputState::new("a".to_string());
        s.insert_char('b');
        assert!(handle_text_input_key(
            &mut s,
            &key(KeyCode::Char('z'), KeyModifiers::CONTROL),
        ));
        assert_eq!(s.text, "a");
        assert!(handle_text_input_key(
            &mut s,
            &key(KeyCode::Char('y'), KeyModifiers::ALT),
        ));
        assert_eq!(s.text, "ab");
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


    #[test]
    fn path_completion_prefixed_relative_path_uses_supplied_base_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("Music").join("Album")).expect("fixture");

        let mut input = TextInputState::new("Music/Al".to_string());
        input.cursor_end();

        apply_path_completion(&mut input, Some(tmp.path()), true);

        assert_eq!(input.text, format!("Music{}Album{}", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR));
        assert_eq!(input.cursor, input.text.len());
    }

    #[test]
    fn display_view_counts_wide_and_combining_glyphs_in_terminal_cells() {
        let mut input = TextInputState::new("A日本e\u{301}Z".to_string());
        input.cursor_end();

        assert_eq!(input.cursor_display_col(), 7);
        let (visible, cursor_col) = input.view(4);
        assert_eq!(crate::display_width::width(&visible), 2);
        assert_eq!(visible, "e\u{301}Z");
        assert_eq!(cursor_col, 2);
    }

    #[test]
    fn display_view_never_begins_with_orphan_combining_mark() {
        let mut input = TextInputState::new("界e\u{301}x".to_string());
        input.cursor_end();

        let (visible, cursor_col) = input.view(2);
        assert_eq!(visible, "x");
        assert_eq!(cursor_col, 1);
        assert!(!visible.starts_with('\u{301}'));
    }

    #[test]
    fn mouse_column_mapping_uses_scrolled_display_cells() {
        let mut input = TextInputState::new("ab日本z".to_string());
        input.cursor_end();

        let (range, _) = input.view_range(4);
        assert_eq!(&input.text[range.clone()], "本z");
        assert_eq!(input.byte_index_for_view_column(4, 0), range.start);
        assert_eq!(input.byte_index_for_view_column(4, 1), range.start + "本".len());
        assert_eq!(input.byte_index_for_view_column(4, 2), range.start + "本".len());
        assert_eq!(input.byte_index_for_view_column(4, 3), range.end);
    }

    #[test]
    fn mouse_click_and_drag_follow_windows_selection_contract() {
        let mut input = TextInputState::new_selected("track.flac".to_string());

        input.begin_mouse_selection(32, 2);
        assert!(!input.has_selection());
        assert_eq!(input.cursor, 2);
        input.drag_mouse_selection(32, 7);
        assert_eq!(input.selection_range(), Some(2..7));
        input.end_mouse_selection();
        assert_eq!(&input.text[input.selection_range().expect("drag selection")], "ack.f");

        input.begin_mouse_selection(32, 4);
        input.end_mouse_selection();
        assert!(!input.has_selection());
        assert_eq!(input.cursor, 4);
    }

    #[test]
    fn path_completion_absolute_path_ignores_supplied_base_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("AbsoluteCandidate")).expect("fixture");
        let other = tempfile::tempdir().expect("other");

        let prefix = tmp.path().join("Abs").to_string_lossy().to_string();
        let mut input = TextInputState::new(prefix);
        input.cursor_end();

        apply_path_completion(&mut input, Some(other.path()), true);

        assert!(input.text.ends_with(&format!("AbsoluteCandidate{}", std::path::MAIN_SEPARATOR)));
        assert_eq!(input.cursor, input.text.len());
    }

    #[test]
    fn ctrl_p_pastes_the_in_app_text_clipboard_like_ctrl_v() {
        let mut input = TextInputState::new_selected("old".to_string());
        input.clipboard = "replacement".to_string();

        assert!(handle_text_input_key(
            &mut input,
            &key(KeyCode::Char('p'), KeyModifiers::CONTROL),
        ));
        assert_eq!(input.text, "replacement");
    }

    #[test]
    fn ctrl_a_selects_all_and_typing_replaces_selection() {
        let mut s = TextInputState::new("hello".to_string());
        handle_text_input_key(&mut s, &key(KeyCode::Char('a'), KeyModifiers::CONTROL));
        assert_eq!(s.selection_range(), Some(0..5));
        handle_text_input_key(&mut s, &key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(s.text, "x");
    }

    #[test]
    fn path_segment_extension_respects_separators() {
        let mut s = TextInputState::new("/home/user/Music".to_string());
        handle_text_input_key_with_boundaries(
            &mut s,
            &key(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            TextBoundaryMode::PathSegment,
        );
        assert_eq!(s.selection_range().map(|range| &s.text[range]), Some("Music"));
        handle_text_input_key_with_boundaries(
            &mut s,
            &key(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            TextBoundaryMode::PathSegment,
        );
        assert_eq!(s.selection_range().map(|range| &s.text[range]), Some("user/Music"));
    }


    #[test]
    fn alt_l_is_a_terminal_safe_select_all_alias_and_alt_a_is_unbound() {
        let mut input = TextInputState::new("Blue Öyster Cult".to_string());
        input.cursor = 4;
        assert!(handle_text_input_key(
            &mut input,
            &key(KeyCode::Char('l'), KeyModifiers::ALT),
        ));
        assert_eq!(input.selection_range(), Some(0..input.text.len()));

        input.clear_selection();
        assert!(!handle_text_input_key(
            &mut input,
            &key(KeyCode::Char('a'), KeyModifiers::ALT),
        ));
        assert_eq!(input.selection_range(), None);
    }

    #[test]
    fn ctrl_slash_and_ctrl_underscore_both_deselect_without_moving_cursor() {
        for reported in ['/', '_'] {
            let mut input = TextInputState::new_selected("selected".to_string());
            input.cursor = 3;
            assert!(handle_text_input_key(
                &mut input,
                &key(KeyCode::Char(reported), KeyModifiers::CONTROL),
            ));
            assert_eq!(input.selection_range(), None);
            assert_eq!(input.cursor, 3);
        }
    }

    #[test]
    fn transform_selection_or_all_is_unicode_safe_and_keeps_range_selected() {
        let mut input = TextInputState::new("Blue Öyster straße".to_string());
        input.selection_anchor = Some("Blue ".len());
        input.cursor = input.text.len();

        assert!(input.transform_selection_or_all(str::to_uppercase));
        assert_eq!(input.text, "Blue ÖYSTER STRASSE");
        assert_eq!(
            input.selection_range().map(|range| &input.text[range]),
            Some("ÖYSTER STRASSE"),
        );
    }

    #[test]
    fn transform_without_selection_targets_the_whole_value() {
        let mut input = TextInputState::new("Mixed Case".to_string());
        input.cursor = 2;

        assert!(input.transform_selection_or_all(str::to_lowercase));
        assert_eq!(input.text, "mixed case");
        assert_eq!(input.selection_range(), Some(0..input.text.len()));
    }

    #[test]
    fn public_shared_clipboard_api_round_trips_exact_text() {
        with_scoped_shared_text_clipboard("", || {
            let payload = "TITLE\nBehind the Lines\nDuchess";
            write_shared_text_clipboard(payload);
            assert_eq!(read_shared_text_clipboard(), payload);
        });
    }

    #[test]
    fn copy_without_selection_publishes_the_whole_field() {
        with_scoped_shared_text_clipboard("stale", || {
            let mut input = TextInputState::new("whole field".to_string());
            input.cursor = 3;

            assert!(input.copy_selection());
            assert_eq!(input.clipboard, "whole field");
            assert_eq!(read_shared_text_clipboard(), "whole field");
            assert_eq!(input.text, "whole field");
            assert_eq!(input.cursor, 3);
        });
    }

    #[test]
    fn cut_without_selection_refuses_without_copying_or_mutating() {
        with_scoped_shared_text_clipboard("shared", || {
            let mut input = TextInputState::new("whole field".to_string());
            input.clipboard = "local".to_string();
            input.cursor = 4;

            assert!(!input.cut_selection());
            assert_eq!(input.text, "whole field");
            assert_eq!(input.cursor, 4);
            assert_eq!(input.clipboard, "local");
            assert_eq!(read_shared_text_clipboard(), "shared");
        });
    }

    #[test]
    fn paste_prefers_newer_shared_text_over_stale_field_local_text() {
        with_scoped_shared_text_clipboard("copied in field A", || {
            let mut input = TextInputState::new_selected("field B".to_string());
            input.clipboard = "stale field B copy".to_string();

            assert!(input.paste_clipboard());
            assert_eq!(input.text, "copied in field A");
            assert_eq!(input.clipboard, "copied in field A");
        });
    }

    #[test]
    fn text_input_copy_and_cut_publish_through_the_shared_hook() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let published = Rc::new(RefCell::new(Vec::<String>::new()));
        let hook_published = Rc::clone(&published);
        with_scoped_shared_text_clipboard("", || {
            with_scoped_shared_text_clipboard_publish_hook(
                move |text| hook_published.borrow_mut().push(text.to_string()),
                || {
                    let mut input = TextInputState::new_selected("path/to/album".to_string());
                    assert!(input.copy_selection());
                    assert_eq!(read_shared_text_clipboard(), "path/to/album");

                    input.select_all_text();
                    assert!(input.cut_selection());
                    assert!(input.text.is_empty());
                },
            );
        });
        assert_eq!(
            published.borrow().as_slice(),
            &["path/to/album".to_string(), "path/to/album".to_string()]
        );
    }

    #[test]
    fn scoped_shared_clipboards_are_isolated_between_parallel_tests() {
        use std::sync::{Arc, Barrier};

        let barrier = Arc::new(Barrier::new(2));
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            with_scoped_shared_text_clipboard("first", || {
                first_barrier.wait();
                let before = read_shared_text_clipboard();
                write_shared_text_clipboard("first-updated");
                first_barrier.wait();
                let after = read_shared_text_clipboard();
                (before, after)
            })
        });
        let second = std::thread::spawn(move || {
            with_scoped_shared_text_clipboard("second", || {
                barrier.wait();
                let before = read_shared_text_clipboard();
                write_shared_text_clipboard("second-updated");
                barrier.wait();
                let after = read_shared_text_clipboard();
                (before, after)
            })
        });

        // Join both workers before asserting. If isolation regresses, both
        // rendezvous still complete and the test fails normally instead of
        // stranding one worker forever at the second barrier.
        let first_observed = first.join().expect("first scoped clipboard thread");
        let second_observed = second.join().expect("second scoped clipboard thread");
        assert_eq!(
            first_observed,
            ("first".to_string(), "first-updated".to_string())
        );
        assert_eq!(
            second_observed,
            ("second".to_string(), "second-updated".to_string())
        );
    }

}
