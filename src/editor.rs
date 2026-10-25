//! Editor state and commands: cursor, scrolling, prompts and key handling. It consumes
//! key events and never touches the terminal, so the whole thing is testable.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::document::{Document, Kind, Pos};
use crate::text::{
    self, col_at_display, display_col, indent_of, next_boundary, next_word, prev_boundary,
    prev_word,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromptKind {
    Find,
    Goto,
    SaveAs,
}

pub enum Mode {
    Edit,
    Prompt { kind: PromptKind, input: String },
    ConfirmQuit,
    Help,
}

pub struct Editor {
    pub doc: Document,
    pub cursor: Pos,
    /// Display column the cursor tries to keep while moving up and down.
    want_col: usize,
    pub top: usize,
    pub left: usize,
    pub mode: Mode,
    pub message: Option<String>,
    pub quit: bool,
    clipboard: String,
    last_search: String,
    /// Last search hit: (row, start byte, end byte). Cleared by any key.
    pub highlight: Option<(usize, usize, usize)>,
    /// Size of the text area, set by the UI before each draw.
    view: (usize, usize),
    /// After a successful save-as, quit (the user answered "save" to the quit prompt).
    quit_after_save: bool,
}

impl Editor {
    pub fn new(doc: Document) -> Editor {
        Editor {
            doc,
            cursor: Pos::default(),
            want_col: 0,
            top: 0,
            left: 0,
            mode: Mode::Edit,
            message: None,
            quit: false,
            clipboard: String::new(),
            last_search: String::new(),
            highlight: None,
            view: (80, 24),
            quit_after_save: false,
        }
    }

    // ---- view ------------------------------------------------------------------------

    /// Record the text-area size and scroll so the cursor is visible.
    pub fn set_view(&mut self, width: usize, height: usize) {
        self.view = (width.max(1), height.max(1));
        let (w, h) = self.view;
        if self.cursor.row < self.top {
            self.top = self.cursor.row;
        } else if self.cursor.row >= self.top + h {
            self.top = self.cursor.row + 1 - h;
        }
        let dc = self.cursor_display_col();
        if dc < self.left {
            self.left = dc;
        } else if dc >= self.left + w {
            self.left = dc + 1 - w;
        }
    }

    pub fn cursor_display_col(&self) -> usize {
        display_col(self.doc.line(self.cursor.row), self.cursor.col)
    }

    // ---- cursor movement ---------------------------------------------------------------

    fn line(&self) -> &str {
        self.doc.line(self.cursor.row)
    }

    fn set_cursor(&mut self, pos: Pos) {
        self.cursor = pos;
        self.want_col = self.cursor_display_col();
    }

    fn left(&mut self) {
        let Pos { row, col } = self.cursor;
        if col > 0 {
            self.set_cursor(Pos::new(row, prev_boundary(self.line(), col)));
        } else if row > 0 {
            self.set_cursor(Pos::new(row - 1, self.doc.line(row - 1).len()));
        }
    }

    fn right(&mut self) {
        let Pos { row, col } = self.cursor;
        if col < self.line().len() {
            self.set_cursor(Pos::new(row, next_boundary(self.line(), col)));
        } else if row + 1 < self.doc.line_count() {
            self.set_cursor(Pos::new(row + 1, 0));
        }
    }

    fn word_left(&mut self) {
        let Pos { row, col } = self.cursor;
        if col == 0 {
            self.left();
        } else {
            self.set_cursor(Pos::new(row, prev_word(self.line(), col)));
        }
    }

    fn word_right(&mut self) {
        let Pos { row, col } = self.cursor;
        if col >= self.line().len() {
            self.right();
        } else {
            self.set_cursor(Pos::new(row, next_word(self.line(), col)));
        }
    }

    fn vertical(&mut self, delta: isize) {
        let last = self.doc.line_count() - 1;
        let row = self.cursor.row.saturating_add_signed(delta).min(last);
        let col = col_at_display(self.doc.line(row), self.want_col);
        self.cursor = Pos::new(row, col);
    }

    fn home(&mut self) {
        let line = self.line();
        let first = indent_of(line).len();
        // Smart home: first press goes to the first non-blank, the next to column 0.
        let col = if self.cursor.col == first { 0 } else { first };
        self.set_cursor(Pos::new(self.cursor.row, col));
    }

    fn end(&mut self) {
        self.set_cursor(Pos::new(self.cursor.row, self.line().len()));
    }

    // ---- editing -------------------------------------------------------------------------

    fn replace(&mut self, start: Pos, end: Pos, text: &str, kind: Kind) {
        let after = self.doc.replace(start, end, text, kind, self.cursor);
        self.set_cursor(after);
    }

    fn insert(&mut self, text: &str, kind: Kind) {
        self.replace(self.cursor, self.cursor, text, kind);
    }

    fn insert_char(&mut self, c: char) {
        self.insert(c.encode_utf8(&mut [0; 4]), Kind::Typing);
    }

    fn insert_tab(&mut self) {
        let n = text::TAB - self.cursor_display_col() % text::TAB;
        self.insert(&" ".repeat(n), Kind::Other);
    }

    fn newline(&mut self) {
        let line = self.line();
        let indent = indent_of(&line[..self.cursor.col]).to_string();
        self.insert(&format!("\n{indent}"), Kind::Other);
    }

    fn backspace(&mut self) {
        let Pos { row, col } = self.cursor;
        let start = if col > 0 {
            Pos::new(row, prev_boundary(self.line(), col))
        } else if row > 0 {
            Pos::new(row - 1, self.doc.line(row - 1).len())
        } else {
            return;
        };
        self.replace(start, self.cursor, "", Kind::Backspace);
    }

    fn delete(&mut self) {
        let Pos { row, col } = self.cursor;
        let end = if col < self.line().len() {
            Pos::new(row, next_boundary(self.line(), col))
        } else if row + 1 < self.doc.line_count() {
            Pos::new(row + 1, 0)
        } else {
            return;
        };
        let cursor = self.cursor;
        self.replace(cursor, end, "", Kind::Delete);
        self.set_cursor(cursor);
    }

    /// Cut the current line into the clipboard (nano's Ctrl+K).
    fn cut_line(&mut self) {
        let row = self.cursor.row;
        let last = row + 1 == self.doc.line_count();
        let (start, end) = if !last {
            (Pos::new(row, 0), Pos::new(row + 1, 0))
        } else if row > 0 {
            (
                Pos::new(row - 1, self.doc.line(row - 1).len()),
                self.doc.end(),
            )
        } else {
            (Pos::new(0, 0), self.doc.end())
        };
        self.clipboard = format!("{}\n", self.doc.line(row));
        self.doc.replace(start, end, "", Kind::Other, self.cursor);
        self.set_cursor(Pos::new(row.min(self.doc.line_count() - 1), 0));
    }

    /// Paste the clipboard: whole lines go above the current one, anything else at the cursor.
    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            self.message = Some("El portapapeles está vacío".into());
            return;
        }
        let text = self.clipboard.clone();
        if text.ends_with('\n') {
            let at = Pos::new(self.cursor.row, 0);
            let cursor = self.cursor;
            let after = self.doc.replace(at, at, &text, Kind::Other, cursor);
            self.set_cursor(after);
        } else {
            self.insert(&text, Kind::Other);
        }
    }

    fn undo(&mut self) {
        match self.doc.undo() {
            Some(p) => self.set_cursor(p),
            None => self.message = Some("Nada que deshacer".into()),
        }
    }

    fn redo(&mut self) {
        match self.doc.redo() {
            Some(p) => self.set_cursor(p),
            None => self.message = Some("Nada que rehacer".into()),
        }
    }

    // ---- file ----------------------------------------------------------------------------

    fn save(&mut self) {
        if self.doc.path().is_none() {
            self.mode = Mode::Prompt {
                kind: PromptKind::SaveAs,
                input: String::new(),
            };
            return;
        }
        match self.doc.save() {
            Ok(()) => {
                self.message = Some(format!("Guardado ({} líneas)", self.doc.line_count()));
                if std::mem::take(&mut self.quit_after_save) {
                    self.quit = true;
                }
            }
            Err(e) => {
                self.quit_after_save = false;
                self.message = Some(format!("No se pudo guardar: {e}"));
            }
        }
    }

    fn request_quit(&mut self) {
        if self.doc.is_dirty() {
            self.mode = Mode::ConfirmQuit;
        } else {
            self.quit = true;
        }
    }

    // ---- search --------------------------------------------------------------------------

    /// First case-insensitive match of `query` in `line` starting at or after byte `from`.
    fn find_in_line(line: &str, query: &str, from: usize) -> Option<(usize, usize)> {
        let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
        let n = query.chars().count();
        line.char_indices()
            .filter(|&(i, _)| i >= from)
            .find_map(|(i, _)| {
                let mut matched = 0;
                let mut end = i;
                for (c, q) in line[i..].chars().zip(query.chars()) {
                    if fold(c) != fold(q) {
                        return None;
                    }
                    matched += 1;
                    end += c.len_utf8();
                }
                (matched == n).then_some((i, end))
            })
    }

    /// Search `query` after the cursor, wrapping around the end of the document.
    fn find_next(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }
        let rows = self.doc.line_count();
        let start_row = self.cursor.row;
        let from_col = next_boundary(self.line(), self.cursor.col);
        for step in 0..=rows {
            let row = (self.cursor.row + step) % rows;
            let from = if step == 0 { from_col } else { 0 };
            let line = self.doc.line(row);
            if let Some((s, e)) = Self::find_in_line(line, query, from) {
                self.set_cursor(Pos::new(row, s));
                self.highlight = Some((row, s, e));
                if step == rows || row < start_row {
                    self.message = Some("Búsqueda reiniciada desde el principio".into());
                }
                return;
            }
        }
        self.message = Some(format!("No encontrado: {query}"));
    }

    // ---- keys ----------------------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.message = None;
        self.highlight = None;
        match std::mem::replace(&mut self.mode, Mode::Edit) {
            Mode::Edit => self.key_edit(key),
            Mode::Prompt { kind, input } => self.key_prompt(key, kind, input),
            Mode::ConfirmQuit => self.key_confirm_quit(key),
            Mode::Help => {} // any key closes the help
        }
    }

    fn key_edit(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let page = self.view.1.saturating_sub(1).max(1) as isize;
        match key.code {
            KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
                's' => self.save(),
                'q' => self.request_quit(),
                'z' => self.undo(),
                'y' => self.redo(),
                'k' => self.cut_line(),
                'u' => self.paste(),
                'f' => {
                    self.mode = Mode::Prompt {
                        kind: PromptKind::Find,
                        input: String::new(),
                    }
                }
                'g' => {
                    self.mode = Mode::Prompt {
                        kind: PromptKind::Goto,
                        input: String::new(),
                    }
                }
                'n' => {
                    let q = self.last_search.clone();
                    self.find_next(&q);
                }
                _ => {}
            },
            KeyCode::Char(c) if !alt => self.insert_char(c),
            KeyCode::Enter => self.newline(),
            KeyCode::Tab => self.insert_tab(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left if ctrl => self.word_left(),
            KeyCode::Right if ctrl => self.word_right(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Up => self.vertical(-1),
            KeyCode::Down => self.vertical(1),
            KeyCode::PageUp => self.vertical(-page),
            KeyCode::PageDown => self.vertical(page),
            KeyCode::Home if ctrl => self.set_cursor(Pos::new(0, 0)),
            KeyCode::End if ctrl => self.set_cursor(self.doc.end()),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            KeyCode::F(1) => self.mode = Mode::Help,
            KeyCode::F(3) => {
                let q = self.last_search.clone();
                self.find_next(&q);
            }
            _ => {}
        }
    }

    fn key_prompt(&mut self, key: KeyEvent, kind: PromptKind, mut input: String) {
        match key.code {
            KeyCode::Esc => {
                self.quit_after_save = false;
            }
            KeyCode::Enter => self.submit(kind, input),
            KeyCode::Backspace => {
                input.pop();
                self.mode = Mode::Prompt { kind, input };
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.push(c);
                self.mode = Mode::Prompt { kind, input };
            }
            _ => self.mode = Mode::Prompt { kind, input },
        }
    }

    fn submit(&mut self, kind: PromptKind, input: String) {
        match kind {
            PromptKind::Find => {
                let q = if input.is_empty() {
                    self.last_search.clone()
                } else {
                    input
                };
                self.last_search = q.clone();
                self.find_next(&q);
            }
            PromptKind::Goto => match input.trim().parse::<usize>() {
                Ok(n) if n >= 1 => {
                    let row = (n - 1).min(self.doc.line_count() - 1);
                    self.set_cursor(Pos::new(row, 0));
                }
                _ => self.message = Some(format!("Número de línea no válido: {input}")),
            },
            PromptKind::SaveAs => {
                let name = input.trim();
                if name.is_empty() {
                    self.quit_after_save = false;
                    return;
                }
                self.doc.set_path(PathBuf::from(name));
                self.save();
            }
        }
    }

    fn key_confirm_quit(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('s' | 'S' | 'y' | 'Y') => {
                self.quit_after_save = true;
                self.save();
            }
            KeyCode::Char('n' | 'N') => self.quit = true,
            _ => {} // Esc, c or anything else: back to editing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEvent;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ed(text: &str) -> Editor {
        static N: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "edittui-ed-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, text).unwrap();
        let e = Editor::new(Document::open(&path).unwrap());
        let _ = fs::remove_file(&path);
        e
    }

    fn press(e: &mut Editor, code: KeyCode) {
        e.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn ctrl(e: &mut Editor, c: char) {
        e.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    fn type_str(e: &mut Editor, s: &str) {
        for c in s.chars() {
            press(
                e,
                if c == '\n' {
                    KeyCode::Enter
                } else {
                    KeyCode::Char(c)
                },
            );
        }
    }

    #[test]
    fn typing_enter_keeps_the_indent_and_tab_aligns() {
        let mut e = ed("");
        type_str(&mut e, "  fn a() {\n");
        assert_eq!(e.doc.text(), "  fn a() {\n  ");
        assert_eq!(e.cursor, Pos::new(1, 2));
        press(&mut e, KeyCode::Tab);
        assert_eq!(e.cursor, Pos::new(1, 4));
        type_str(&mut e, "x");
        press(&mut e, KeyCode::Tab);
        assert_eq!(e.cursor_display_col(), 8);
    }

    #[test]
    fn backspace_and_delete_join_lines_and_respect_graphemes() {
        let mut e = ed("ab\ncd");
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::Backspace);
        assert_eq!(e.doc.text(), "abcd");
        assert_eq!(e.cursor, Pos::new(0, 2));
        press(&mut e, KeyCode::Delete);
        assert_eq!(e.doc.text(), "abd");
        let mut e = ed("e\u{301}x");
        press(&mut e, KeyCode::Delete);
        assert_eq!(e.doc.text(), "x", "a combining mark goes with its base");
    }

    #[test]
    fn up_down_keep_the_display_column() {
        let mut e = ed("abcdef\nab\nabcdef");
        for _ in 0..5 {
            press(&mut e, KeyCode::Right);
        }
        press(&mut e, KeyCode::Down);
        assert_eq!(e.cursor, Pos::new(1, 2));
        press(&mut e, KeyCode::Down);
        assert_eq!(e.cursor, Pos::new(2, 5));
    }

    #[test]
    fn home_toggles_between_first_nonblank_and_column_zero() {
        let mut e = ed("   x");
        press(&mut e, KeyCode::End);
        press(&mut e, KeyCode::Home);
        assert_eq!(e.cursor.col, 3);
        press(&mut e, KeyCode::Home);
        assert_eq!(e.cursor.col, 0);
    }

    #[test]
    fn undo_redo_restore_text_and_cursor() {
        let mut e = ed("x");
        press(&mut e, KeyCode::End);
        type_str(&mut e, "abc");
        ctrl(&mut e, 'z');
        assert_eq!(e.doc.text(), "x");
        assert_eq!(e.cursor, Pos::new(0, 1));
        ctrl(&mut e, 'y');
        assert_eq!(e.doc.text(), "xabc");
        assert_eq!(e.cursor, Pos::new(0, 4));
        ctrl(&mut e, 'z');
        ctrl(&mut e, 'z');
        assert_eq!(e.message.as_deref(), Some("Nada que deshacer"));
    }

    #[test]
    fn cut_line_and_paste_move_whole_lines() {
        let mut e = ed("a\nb\nc");
        press(&mut e, KeyCode::Down);
        ctrl(&mut e, 'k');
        assert_eq!(e.doc.text(), "a\nc");
        assert_eq!(e.cursor, Pos::new(1, 0));
        press(&mut e, KeyCode::Down); // already on the last line: stays
        ctrl(&mut e, 'u');
        assert_eq!(e.doc.text(), "a\nb\nc");
        // Cutting the last line takes the preceding newline with it.
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::Down);
        ctrl(&mut e, 'k');
        assert_eq!(e.doc.text(), "a\nb");
        ctrl(&mut e, 'z');
        assert_eq!(e.doc.text(), "a\nb\nc");
    }

    #[test]
    fn cutting_the_only_line_leaves_an_empty_document() {
        let mut e = ed("solo");
        ctrl(&mut e, 'k');
        assert_eq!(e.doc.text(), "");
        assert_eq!(e.cursor, Pos::new(0, 0));
        ctrl(&mut e, 'u');
        assert_eq!(e.doc.text(), "solo\n");
    }

    #[test]
    fn find_is_case_insensitive_wraps_and_reports_misses() {
        let mut e = ed("uno Foo\ndos\nfoo tres");
        ctrl(&mut e, 'f');
        type_str(&mut e, "FOO");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.cursor, Pos::new(0, 4));
        assert_eq!(e.highlight, Some((0, 4, 7)));
        ctrl(&mut e, 'n');
        assert_eq!(e.cursor, Pos::new(2, 0));
        ctrl(&mut e, 'n'); // wraps to the first one
        assert_eq!(e.cursor, Pos::new(0, 4));
        ctrl(&mut e, 'f');
        type_str(&mut e, "zzz");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.message.as_deref(), Some("No encontrado: zzz"));
        assert_eq!(e.cursor, Pos::new(0, 4));
    }

    #[test]
    fn goto_clamps_and_rejects_garbage() {
        let mut e = ed("a\nb\nc");
        ctrl(&mut e, 'g');
        type_str(&mut e, "2");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.cursor.row, 1);
        ctrl(&mut e, 'g');
        type_str(&mut e, "99");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.cursor.row, 2);
        ctrl(&mut e, 'g');
        type_str(&mut e, "x");
        press(&mut e, KeyCode::Enter);
        assert!(e.message.as_deref().unwrap().starts_with("Número de línea"));
    }

    #[test]
    fn quit_asks_when_dirty_and_saves_on_request() {
        let dir = std::env::temp_dir().join(format!("edittui-q-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("q.txt");
        fs::write(&path, "a").unwrap();
        let mut e = Editor::new(Document::open(&path).unwrap());
        ctrl(&mut e, 'q');
        assert!(e.quit, "clean documents quit at once");

        let mut e = Editor::new(Document::open(&path).unwrap());
        type_str(&mut e, "b");
        ctrl(&mut e, 'q');
        assert!(!e.quit && matches!(e.mode, Mode::ConfirmQuit));
        press(&mut e, KeyCode::Esc);
        assert!(!e.quit && matches!(e.mode, Mode::Edit));
        ctrl(&mut e, 'q');
        press(&mut e, KeyCode::Char('s'));
        assert!(e.quit);
        assert_eq!(fs::read_to_string(&path).unwrap(), "ba");

        let mut e = Editor::new(Document::open(&path).unwrap());
        type_str(&mut e, "z");
        ctrl(&mut e, 'q');
        press(&mut e, KeyCode::Char('n'));
        assert!(e.quit);
        assert_eq!(fs::read_to_string(&path).unwrap(), "ba", "'n' discards");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_an_unnamed_document_asks_for_a_name() {
        let dir = std::env::temp_dir().join(format!("edittui-sa-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut e = Editor::new(Document::empty());
        type_str(&mut e, "hola");
        ctrl(&mut e, 'q');
        press(&mut e, KeyCode::Char('s'));
        assert!(matches!(
            e.mode,
            Mode::Prompt {
                kind: PromptKind::SaveAs,
                ..
            }
        ));
        let target = dir.join("n.txt");
        type_str(&mut e, target.to_str().unwrap());
        press(&mut e, KeyCode::Enter);
        assert!(e.quit, "save-as answered from the quit prompt also quits");
        assert_eq!(fs::read_to_string(&target).unwrap(), "hola");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scrolling_keeps_the_cursor_in_view() {
        let mut e = ed(&"x\n".repeat(50));
        e.set_view(10, 5);
        for _ in 0..20 {
            press(&mut e, KeyCode::Down);
        }
        e.set_view(10, 5);
        assert_eq!(e.top, 16);
        ctrl(&mut e, 'g');
        type_str(&mut e, "1");
        press(&mut e, KeyCode::Enter);
        e.set_view(10, 5);
        assert_eq!(e.top, 0);
        let mut e = ed(&"y".repeat(30));
        e.set_view(10, 5);
        press(&mut e, KeyCode::End);
        e.set_view(10, 5);
        assert_eq!(e.left, 21);
    }
}
