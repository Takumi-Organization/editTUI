//! The text buffer: lines, undo/redo and file I/O. No cursor and no terminal here, so it is
//! testable on its own. Every change goes through [`Document::replace`].

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Refuse files larger than this: the editor keeps the whole text in memory.
pub const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// A position: `col` is a byte offset inside line `row`, always on a char boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: usize,
    pub col: usize,
}

impl Pos {
    pub const fn new(row: usize, col: usize) -> Pos {
        Pos { row, col }
    }
}

/// What an edit is, so runs of typing or deleting undo as one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Typing,
    Backspace,
    Delete,
    Other,
}

struct Edit {
    at: Pos,
    removed: String,
    inserted: String,
    kind: Kind,
    before: Pos,
    after: Pos,
}

pub struct Document {
    lines: Vec<String>,
    path: Option<PathBuf>,
    crlf: bool,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Undo depth that matches the file on disk; `None` if that state is unreachable.
    saved_depth: Option<usize>,
    /// The next edit must not merge into the previous one (after save, undo or redo).
    barrier: bool,
}

/// Where the text ends if `text` is inserted at `start`.
fn end_of(start: Pos, text: &str) -> Pos {
    match text.rsplit_once('\n') {
        None => Pos::new(start.row, start.col + text.len()),
        Some((head, tail)) => Pos::new(start.row + head.matches('\n').count() + 1, tail.len()),
    }
}

impl Document {
    pub fn empty() -> Document {
        Document::from_text(String::new(), None)
    }

    fn from_text(text: String, path: Option<PathBuf>) -> Document {
        let crlf = text.contains("\r\n");
        let text = if crlf {
            text.replace("\r\n", "\n")
        } else {
            text
        };
        Document {
            lines: text.split('\n').map(String::from).collect(),
            path,
            crlf,
            undo: Vec::new(),
            redo: Vec::new(),
            saved_depth: Some(0),
            barrier: false,
        }
    }

    /// Open `path`. A file that does not exist yet gives an empty document that will be
    /// created on save. Non-UTF-8 and oversized files are refused.
    pub fn open(path: &Path) -> io::Result<Document> {
        match fs::metadata(path) {
            Ok(m) if m.is_dir() => {
                return Err(io::Error::other("es un directorio"));
            }
            Ok(m) if m.len() > MAX_BYTES => {
                return Err(io::Error::other("el archivo es demasiado grande"));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Document::from_text(String::new(), Some(path.to_owned())));
            }
            Err(e) => return Err(e),
        }
        let bytes = fs::read(path)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| io::Error::other("el archivo no es UTF-8 (¿es binario?)"))?;
        Ok(Document::from_text(text, Some(path.to_owned())))
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, row: usize) -> &str {
        self.lines.get(row).map_or("", String::as_str)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
        // Saving under a new name makes the on-disk state unrelated to the undo history.
        self.saved_depth = None;
    }

    pub fn is_crlf(&self) -> bool {
        self.crlf
    }

    pub fn is_dirty(&self) -> bool {
        self.saved_depth != Some(self.undo.len())
    }

    /// The whole text with `\n` line ends.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// End of the last line.
    pub fn end(&self) -> Pos {
        let row = self.lines.len() - 1;
        Pos::new(row, self.lines[row].len())
    }

    /// Write to the document's path (via a temp file and rename, keeping the permissions).
    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::other("sin nombre de archivo"))?;
        let mut text = self.text();
        if self.crlf {
            text = text.replace('\n', "\r\n");
        }
        let name = path.file_name().map_or_else(
            || "edittui".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let tmp = path.with_file_name(format!(".{name}.edittui-tmp"));
        fs::write(&tmp, text)?;
        if let Ok(meta) = fs::metadata(&path) {
            let _ = fs::set_permissions(&tmp, meta.permissions());
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.saved_depth = Some(self.undo.len());
        self.barrier = true;
        Ok(())
    }

    /// The text between two positions (`start <= end`).
    pub fn text_in(&self, start: Pos, end: Pos) -> String {
        if start.row == end.row {
            return self.lines[start.row][start.col..end.col].to_string();
        }
        let mut out = self.lines[start.row][start.col..].to_string();
        for row in start.row + 1..end.row {
            out.push('\n');
            out.push_str(&self.lines[row]);
        }
        out.push('\n');
        out.push_str(&self.lines[end.row][..end.col]);
        out
    }

    fn splice(&mut self, start: Pos, end: Pos, text: &str) {
        let joined = format!(
            "{}{}{}",
            &self.lines[start.row][..start.col],
            text,
            &self.lines[end.row][end.col..]
        );
        self.lines
            .splice(start.row..=end.row, joined.split('\n').map(String::from));
    }

    /// Replace `start..end` with `text` and return the position after the inserted text.
    /// `cursor` is where the cursor was, restored by undo.
    pub fn replace(&mut self, start: Pos, end: Pos, text: &str, kind: Kind, cursor: Pos) -> Pos {
        let removed = self.text_in(start, end);
        let after = end_of(start, text);
        self.splice(start, end, text);
        self.redo.clear();
        let barrier = std::mem::take(&mut self.barrier);
        if !barrier
            && let Some(last) = self.undo.last_mut()
            && merge(last, start, end, text, &removed, kind, after)
        {
            return after;
        }
        self.undo.push(Edit {
            at: start,
            removed,
            inserted: text.to_string(),
            kind,
            before: cursor,
            after,
        });
        if self.saved_depth.is_some_and(|d| d >= self.undo.len()) {
            self.saved_depth = None;
        }
        after
    }

    /// Undo one step; returns where the cursor goes.
    pub fn undo(&mut self) -> Option<Pos> {
        let edit = self.undo.pop()?;
        self.splice(edit.at, end_of(edit.at, &edit.inserted), &edit.removed);
        let cursor = edit.before;
        self.redo.push(edit);
        self.barrier = true;
        Some(cursor)
    }

    /// Redo one step; returns where the cursor goes.
    pub fn redo(&mut self) -> Option<Pos> {
        let edit = self.redo.pop()?;
        self.splice(edit.at, end_of(edit.at, &edit.removed), &edit.inserted);
        let cursor = edit.after;
        self.undo.push(edit);
        self.barrier = true;
        Some(cursor)
    }
}

/// Try to fold a new edit into the last one; true if it was merged.
fn merge(
    last: &mut Edit,
    start: Pos,
    end: Pos,
    text: &str,
    removed: &str,
    kind: Kind,
    after: Pos,
) -> bool {
    if last.kind != kind {
        return false;
    }
    match kind {
        Kind::Typing if removed.is_empty() && !text.contains('\n') && start == last.after => {
            last.inserted.push_str(text);
            last.after = after;
            true
        }
        Kind::Backspace if text.is_empty() && last.inserted.is_empty() && end == last.at => {
            last.at = start;
            last.removed.insert_str(0, removed);
            last.after = start;
            true
        }
        Kind::Delete if text.is_empty() && last.inserted.is_empty() && start == last.at => {
            last.removed.push_str(removed);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Document {
        Document::from_text(text.to_string(), None)
    }

    fn typ(d: &mut Document, at: Pos, s: &str) -> Pos {
        d.replace(at, at, s, Kind::Typing, at)
    }

    #[test]
    fn insert_and_newline_split_lines() {
        let mut d = doc("hello");
        let p = typ(&mut d, Pos::new(0, 2), "\n");
        assert_eq!(p, Pos::new(1, 0));
        assert_eq!(d.text(), "he\nllo");
        let p = typ(&mut d, Pos::new(1, 3), "!\nx\ny");
        assert_eq!(p, Pos::new(3, 1));
        assert_eq!(d.text(), "he\nllo!\nx\ny");
        assert_eq!(d.line_count(), 4);
    }

    #[test]
    fn delete_across_lines_joins_them() {
        let mut d = doc("ab\ncd\nef");
        d.replace(
            Pos::new(0, 1),
            Pos::new(2, 1),
            "",
            Kind::Other,
            Pos::new(2, 1),
        );
        assert_eq!(d.text(), "af");
        assert_eq!(d.text_in(Pos::new(0, 0), Pos::new(0, 2)), "af");
    }

    #[test]
    fn typing_run_undoes_as_one_step_and_redoes() {
        let mut d = doc("");
        let mut p = Pos::new(0, 0);
        for c in ["a", "b", "c"] {
            p = d.replace(p, p, c, Kind::Typing, p);
        }
        assert_eq!(d.text(), "abc");
        assert_eq!(d.undo(), Some(Pos::new(0, 0)));
        assert_eq!(d.text(), "");
        assert_eq!(d.redo(), Some(Pos::new(0, 3)));
        assert_eq!(d.text(), "abc");
        assert_eq!(d.undo(), Some(Pos::new(0, 0)));
        assert_eq!(d.undo(), None);
    }

    #[test]
    fn backspace_run_merges_and_restores_the_text() {
        let mut d = doc("abcd");
        d.replace(
            Pos::new(0, 3),
            Pos::new(0, 4),
            "",
            Kind::Backspace,
            Pos::new(0, 4),
        );
        d.replace(
            Pos::new(0, 2),
            Pos::new(0, 3),
            "",
            Kind::Backspace,
            Pos::new(0, 3),
        );
        assert_eq!(d.text(), "ab");
        assert_eq!(d.undo(), Some(Pos::new(0, 4)));
        assert_eq!(d.text(), "abcd");
    }

    #[test]
    fn forward_delete_run_merges() {
        let mut d = doc("abcd");
        d.replace(
            Pos::new(0, 1),
            Pos::new(0, 2),
            "",
            Kind::Delete,
            Pos::new(0, 1),
        );
        d.replace(
            Pos::new(0, 1),
            Pos::new(0, 2),
            "",
            Kind::Delete,
            Pos::new(0, 1),
        );
        assert_eq!(d.text(), "ad");
        d.undo();
        assert_eq!(d.text(), "abcd");
    }

    #[test]
    fn newline_is_its_own_undo_step() {
        let mut d = doc("");
        let p = typ(&mut d, Pos::new(0, 0), "a");
        let p = d.replace(p, p, "\n", Kind::Other, p);
        typ(&mut d, p, "b");
        assert_eq!(d.text(), "a\nb");
        d.undo();
        assert_eq!(d.text(), "a\n");
        d.undo();
        assert_eq!(d.text(), "a");
    }

    #[test]
    fn dirty_tracks_the_saved_state() {
        let dir = std::env::temp_dir().join(format!("edittui-dirty-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        let mut d = Document::open(&path).unwrap(); // does not exist yet
        assert!(!d.is_dirty());
        let p = typ(&mut d, Pos::new(0, 0), "x");
        assert!(d.is_dirty());
        d.save().unwrap();
        assert!(!d.is_dirty());
        // Typing right after a save must not merge into the saved edit.
        typ(&mut d, p, "y");
        assert!(d.is_dirty());
        d.undo();
        assert!(!d.is_dirty(), "undo returns to the saved text");
        d.redo();
        assert!(d.is_dirty());
        d.undo();
        d.undo();
        typ(&mut d, Pos::new(0, 0), "z");
        d.undo();
        d.redo();
        assert!(
            d.is_dirty(),
            "a diverged history never matches the saved state"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_and_open_keep_crlf_and_the_final_newline() {
        let dir = std::env::temp_dir().join(format!("edittui-io-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.txt");
        fs::write(&path, "uno\r\ndos\r\n").unwrap();
        let mut d = Document::open(&path).unwrap();
        assert!(d.is_crlf());
        assert_eq!(d.line_count(), 3); // the last, empty line stands for the final newline
        typ(&mut d, Pos::new(1, 3), "!");
        d.save().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"uno\r\ndos!\r\n");
        let lf = dir.join("lf.txt");
        fs::write(&lf, "a\nb").unwrap();
        let mut d = Document::open(&lf).unwrap();
        d.save().unwrap();
        assert_eq!(fs::read(&lf).unwrap(), b"a\nb");
        assert!(!dir.join(".lf.txt.edittui-tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_binary_and_directories() {
        let dir = std::env::temp_dir().join(format!("edittui-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("b.bin");
        fs::write(&bin, [0xff, 0xfe, 0x00]).unwrap();
        assert!(Document::open(&bin).is_err());
        assert!(Document::open(&dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
