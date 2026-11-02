//! A one-line text buffer with a real insertion caret — the single editing
//! model shared by every typed field in the UI (the unlock/lock passphrase, the
//! `:` palette, the inline detail edit, the memo composer and its search bar,
//! the vault `/` filter and every form field).
//!
//! The caret is a CHAR index in `0..=char_count`, never a byte offset: Italian
//! text is full of multi-byte letters (`à è é ì ò ù`) and a byte-index edit
//! would split one down the middle and corrupt the string. Every mutation here
//! lands on a `char_indices` boundary, so the buffer is always valid UTF-8.
//!
//! The stored text is a secret as often as not (a passphrase, a password field,
//! a half-written memo that is vault content). So the value is wiped on drop and
//! by [`TextInput::clear`]/[`Zeroize`]; the caret index is not secret and is not
//! (its position leaks nothing — the mask never changes length with it). There
//! is deliberately no `Clone` (like [`crate::model::SecretString`]): a secret
//! must not be copied by accident — callers that truly need a copy take one off
//! [`TextInput::as_str`] and own its wiping.

use ratatui::crossterm::event::KeyCode;
use zeroize::Zeroize;

/// What routing a key through [`edit_key`] did to a [`TextInput`]: moved the
/// caret (the text is untouched), changed the text, or was not an editing key at
/// all (the caller handles it — Enter, Esc, a navigation arrow, …). The split
/// lets a caller re-run a search or reset a selection on `Changed` alone, and
/// leave both be when the caret merely moved.
#[derive(PartialEq, Eq, Debug)]
pub enum Edit {
    Moved,
    Changed,
    Ignored,
}

/// Routes the caret-movement and deletion keys every text field shares
/// (←/→/Home/End, Backspace, Delete) to `input`. Char insertion is the caller's
/// (a printable char is filtered for modifier chords first at each site), as is
/// anything this returns [`Edit::Ignored`] for.
pub fn edit_key(input: &mut TextInput, code: KeyCode) -> Edit {
    match code {
        KeyCode::Left => {
            input.left();
            Edit::Moved
        }
        KeyCode::Right => {
            input.right();
            Edit::Moved
        }
        KeyCode::Home => {
            input.home();
            Edit::Moved
        }
        KeyCode::End => {
            input.end();
            Edit::Moved
        }
        KeyCode::Backspace => {
            input.backspace();
            Edit::Changed
        }
        KeyCode::Delete => {
            input.delete();
            Edit::Changed
        }
        _ => Edit::Ignored,
    }
}

/// A line of text being edited, plus where the caret sits within it.
#[derive(Default)]
pub struct TextInput {
    /// The edited text. Private so the caret invariant (a char boundary in
    /// `0..=char_count`) can never be violated from outside.
    value: String,
    /// Caret position as a CHAR index, `0..=self.char_count()`.
    caret: usize,
}

impl TextInput {
    /// An empty buffer, caret at 0.
    pub fn new() -> TextInput {
        TextInput::default()
    }

    /// The edited text (read-only). The [`std::ops::Deref`] below routes every
    /// `&str` method through here too, so `input.trim()`, `input.is_empty()`,
    /// `input.chars()` and friends all read the value directly.
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// The caret as a CHAR index in `0..=char_count` — what the renderer splits
    /// the line on to draw the caret mid-string.
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The number of CHARS (not bytes) in the value — the caret's upper bound.
    pub fn char_count(&self) -> usize {
        self.value.chars().count()
    }

    /// Replaces the whole value and parks the caret at the END (so a prefill
    /// then types like it always had — appending, as before the caret existed).
    /// The old value is wiped first: a prefill can overwrite a secret.
    pub fn set(&mut self, s: impl Into<String>) {
        self.value.zeroize();
        self.value = s.into();
        self.caret = self.char_count();
    }

    /// Wipes the value (zeroized — it may be a secret) and resets the caret.
    pub fn clear(&mut self) {
        self.value.zeroize();
        self.value.clear();
        self.caret = 0;
    }

    // ---- editing (every offset lands on a char boundary) ------------------

    /// The byte offset of char index `idx` (the value's byte length once `idx`
    /// reaches the end) — the one place a char index becomes a byte index.
    fn byte_at(&self, idx: usize) -> usize {
        self.value
            .char_indices()
            .nth(idx)
            .map(|(b, _)| b)
            .unwrap_or(self.value.len())
    }

    /// Inserts `ch` at the caret and steps the caret past it.
    pub fn insert(&mut self, ch: char) {
        let at = self.byte_at(self.caret);
        self.value.insert(at, ch);
        self.caret += 1;
    }

    /// Deletes the char BEFORE the caret (Backspace); a no-op at the start.
    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        self.caret -= 1;
        let at = self.byte_at(self.caret);
        self.value.remove(at);
    }

    /// Deletes the char AT the caret (forward Delete); a no-op at the end. The
    /// caret does not move — the text to its right slides left under it.
    pub fn delete(&mut self) {
        if self.caret >= self.char_count() {
            return;
        }
        let at = self.byte_at(self.caret);
        self.value.remove(at);
    }

    /// Moves the caret one char left (clamped at the start).
    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    /// Moves the caret one char right (clamped at the end).
    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.char_count());
    }

    /// Jumps the caret to the start of the line.
    pub fn home(&mut self) {
        self.caret = 0;
    }

    /// Jumps the caret to the end of the line.
    pub fn end(&mut self) {
        self.caret = self.char_count();
    }

    // ---- rendering --------------------------------------------------------

    /// The line split at the caret for drawing, after scrolling a `cols`-wide
    /// window so the caret stays in view: `(before, after)` are the visible text
    /// on each side of the caret and the caller draws its own caret glyph
    /// between them. One column is reserved for that glyph, so `before` +
    /// `after` never exceed `cols - 1` chars and the whole thing (caret glyph
    /// included) fits the box. When the value fits, nothing is scrolled.
    pub fn view(&self, cols: usize) -> (String, String) {
        window_line(&self.value, self.caret, cols)
    }
}

/// The [`TextInput::view`] windowing over a bare `line` and a caret COLUMN
/// within it: `(before, after)` are the visible chars on each side of the caret
/// after scrolling a `cols`-wide window (one column reserved for the caret
/// glyph) so the caret stays in view. Split out because the memo composer's
/// caret is linear over a whole `\n`-separated buffer, but each ROW draws one
/// line and needs the caret placed by its column.
pub fn window_line(line: &str, caret_col: usize, cols: usize) -> (String, String) {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let caret = caret_col.min(len);
    // one column belongs to the caret glyph the caller inserts
    let budget = cols.saturating_sub(1);
    let start = if len <= budget {
        0
    } else {
        // keep the caret off the extreme edges: a half-budget of context to its
        // left where there is room, clamped so the last page stays full
        let left = budget / 2;
        caret.saturating_sub(left).min(len - budget)
    };
    let end = (start + budget).min(len);
    // the caret is guaranteed inside [start, end] by the window above
    let split = caret.clamp(start, end);
    let before: String = chars[start..split].iter().collect();
    let after: String = chars[split..end].iter().collect();
    (before, after)
}

/// So every `&str` method (`trim`, `is_empty`, `chars`, `contains`,
/// `to_lowercase`, `split`, …) reads the value with no wrapper boilerplate.
/// Deliberately no `DerefMut`: mutation must go through the caret-aware methods
/// above, never a raw `&mut String` that could desync the caret.
impl std::ops::Deref for TextInput {
    type Target = str;
    fn deref(&self) -> &str {
        &self.value
    }
}

impl From<&str> for TextInput {
    fn from(s: &str) -> TextInput {
        TextInput { value: s.to_string(), caret: s.chars().count() }
    }
}

impl From<String> for TextInput {
    fn from(value: String) -> TextInput {
        let caret = value.chars().count();
        TextInput { value, caret }
    }
}

impl PartialEq<&str> for TextInput {
    fn eq(&self, other: &&str) -> bool {
        self.value == *other
    }
}

impl PartialEq<str> for TextInput {
    fn eq(&self, other: &str) -> bool {
        self.value == other
    }
}

impl Zeroize for TextInput {
    fn zeroize(&mut self) {
        self.value.zeroize();
        self.caret = 0;
    }
}

impl Drop for TextInput {
    fn drop(&mut self) {
        // the value may be a passphrase or a password field — don't leave it in
        // a freed heap buffer (the caret is not secret and needs no wipe).
        self.value.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Types a whole string, char by char, at the caret — the way a real key
    /// stream reaches [`TextInput::insert`].
    fn typed(s: &str) -> TextInput {
        let mut t = TextInput::new();
        for ch in s.chars() {
            t.insert(ch);
        }
        t
    }

    #[test]
    fn typing_appends_and_the_caret_follows() {
        let t = typed("abc");
        assert_eq!(t.as_str(), "abc");
        assert_eq!(t.caret(), 3, "the caret sits at the end after typing");
    }

    #[test]
    fn insert_lands_in_the_middle() {
        // "abc", Left, Left, insert 'X' -> "aXbc"
        let mut t = typed("abc");
        t.left();
        t.left();
        assert_eq!(t.caret(), 1);
        t.insert('X');
        assert_eq!(t.as_str(), "aXbc");
        assert_eq!(t.caret(), 2, "the caret is just past the inserted char");
    }

    #[test]
    fn backspace_deletes_before_the_caret_and_is_a_noop_at_the_start() {
        let mut t = typed("abc");
        t.left(); // caret before 'c'
        t.backspace(); // removes 'b'
        assert_eq!(t.as_str(), "ac");
        assert_eq!(t.caret(), 1);
        t.home();
        t.backspace(); // nothing before the start
        assert_eq!(t.as_str(), "ac", "backspace at the start is a no-op");
        assert_eq!(t.caret(), 0);
    }

    #[test]
    fn delete_removes_the_char_at_the_caret_and_is_a_noop_at_the_end() {
        let mut t = typed("abc");
        t.home();
        t.delete(); // removes 'a', caret stays at 0
        assert_eq!(t.as_str(), "bc");
        assert_eq!(t.caret(), 0);
        t.end();
        t.delete(); // nothing at the end
        assert_eq!(t.as_str(), "bc", "delete at the end is a no-op");
        assert_eq!(t.caret(), 2);
    }

    #[test]
    fn left_right_home_end_clamp_at_the_edges() {
        let mut t = typed("abc");
        t.right(); // already at the end
        assert_eq!(t.caret(), 3, "right clamps at the end");
        t.home();
        t.left();
        assert_eq!(t.caret(), 0, "left clamps at the start");
        t.end();
        assert_eq!(t.caret(), 3);
        t.left();
        assert_eq!(t.caret(), 2);
    }

    #[test]
    fn multibyte_text_edited_in_the_middle_stays_valid_utf8() {
        // "città" — the à is two bytes; a byte-index edit would corrupt it.
        let mut t = typed("città");
        assert_eq!(t.char_count(), 5);
        // caret between 't' and 't' (index 3), delete forward removes the second 't'
        t.home();
        t.right();
        t.right();
        t.right();
        assert_eq!(t.caret(), 3);
        t.insert('X'); // "citXtà"
        assert_eq!(t.as_str(), "citXtà");
        // walk onto the à and delete it — the multi-byte char goes whole
        t.end();
        t.left();
        assert_eq!(t.caret(), 5);
        t.delete();
        assert_eq!(t.as_str(), "citXt");
        // still valid UTF-8 throughout (as_str would have panicked on a bad cut)
        assert!(t.as_str().chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn backspace_over_a_multibyte_char_removes_the_whole_code_point() {
        let mut t = typed("caffè");
        t.backspace(); // removes the è, not a stray byte
        assert_eq!(t.as_str(), "caff");
        assert_eq!(t.caret(), 4);
    }

    #[test]
    fn set_parks_the_caret_at_the_end_and_wipes_the_old_value() {
        let mut t = typed("old");
        t.set("prefill");
        assert_eq!(t.as_str(), "prefill");
        assert_eq!(t.caret(), 7, "a prefill types from the end");
    }

    #[test]
    fn clear_and_zeroize_empty_it() {
        let mut t = typed("secret");
        t.clear();
        assert_eq!(t.as_str(), "");
        assert_eq!(t.caret(), 0);
        let mut u = typed("secret");
        u.zeroize();
        assert_eq!(u.as_str(), "");
        assert_eq!(u.caret(), 0);
    }

    #[test]
    fn from_str_and_string_start_at_the_end() {
        let t: TextInput = "hello".into();
        assert_eq!(t.caret(), 5);
        let s: TextInput = String::from("world!").into();
        assert_eq!(s.caret(), 6);
    }

    #[test]
    fn view_shows_the_whole_line_when_it_fits() {
        let mut t = typed("abcdef");
        t.home();
        t.right(); // caret at 1
        let (before, after) = t.view(20);
        assert_eq!(before, "a");
        assert_eq!(after, "bcdef", "caller draws its glyph between the two");
    }

    #[test]
    fn view_scrolls_to_keep_the_caret_visible() {
        // a value far wider than the box: the window follows the caret
        let mut t = typed("0123456789abcdef");
        // caret at the very end -> the tail is shown, glyph at the right
        let (before, after) = t.view(6); // 5 chars of text + 1 caret column
        assert!(after.is_empty(), "at the end nothing sits right of the caret");
        assert_eq!(before.chars().count(), 5, "the last 5 chars are shown");
        assert_eq!(before, "bcdef");
        // caret in the middle -> both sides show, caret stays inside the box
        t.home();
        for _ in 0..8 {
            t.right();
        }
        let (before, after) = t.view(6);
        assert!(before.chars().count() + after.chars().count() <= 5, "fits the box minus the caret");
        assert!(!before.is_empty() && !after.is_empty(), "context on both sides mid-line");
    }

    #[test]
    fn a_trailing_caret_glyph_always_fits_the_box() {
        // whatever the caret does, before+after leave a column for the glyph
        let mut t = typed("supercalifragilistic");
        for cols in [1usize, 2, 5, 8, 20, 40] {
            for caret in 0..=t.char_count() {
                while t.caret() > caret {
                    t.left();
                }
                while t.caret() < caret {
                    t.right();
                }
                let (before, after) = t.view(cols);
                let shown = before.chars().count() + after.chars().count();
                assert!(shown + 1 <= cols.max(1), "cols {cols} caret {caret}: {shown}+glyph > box");
            }
        }
    }
}
