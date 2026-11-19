//! Drawing. Only glyphs the Linux console font (Terminus) has: ASCII plus box lines,
//! `│ • …` and accented Latin letters.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_segmentation::UnicodeSegmentation;

use crate::editor::{Editor, Mode, PromptKind};
use crate::text::cell_width;

// Takumi style (see takumi-design): strict monochrome. The Linux console has no
// truecolor, so no RGB at all: the terminal's own default colours plus modifiers.
// Selected / status bar / search hit = REVERSED, quiet text = DIM, emphasis = BOLD.

const HELP: [(&str, &str); 14] = [
    ("Ctrl+S", "guardar"),
    ("Ctrl+Q", "salir"),
    ("Ctrl+Z / Ctrl+Y", "deshacer / rehacer"),
    ("Ctrl+F", "buscar (Enter: siguiente)"),
    ("Ctrl+N / F3", "siguiente coincidencia"),
    ("Ctrl+G", "ir a la línea"),
    ("Ctrl+K", "cortar la línea"),
    ("Ctrl+U", "pegar"),
    ("Ctrl+← / Ctrl+→", "palabra anterior / siguiente"),
    ("Inicio / Fin", "inicio de texto, columna 0 / fin"),
    ("Ctrl+Inicio / Ctrl+Fin", "principio / final del archivo"),
    ("Re Pág / Av Pág", "una pantalla"),
    ("Tab", "sangrar a múltiplo de 4"),
    ("F1", "esta ayuda"),
];

fn base() -> Style {
    Style::new()
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

fn inverse() -> Style {
    Style::new().add_modifier(Modifier::REVERSED)
}

pub fn draw(f: &mut Frame, ed: &mut Editor) {
    let area = f.area();
    f.render_widget(Block::new().style(base()), area);
    if area.width < 20 || area.height < 4 {
        f.render_widget(
            Paragraph::new("Terminal demasiado pequeño").style(base()),
            area,
        );
        return;
    }
    let [text_area, status, hint] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let digits = ed.doc.line_count().to_string().len().max(3);
    let gutter = digits as u16 + 2;
    let body_w = text_area.width.saturating_sub(gutter).max(1);
    ed.set_view(body_w as usize, text_area.height as usize);

    draw_text(f, ed, text_area, digits, gutter);
    draw_status(f, ed, status);
    draw_hint(f, ed, hint);

    if matches!(ed.mode, Mode::Help) {
        draw_help(f, area);
    }
    place_cursor(f, ed, text_area, gutter, status);
}

fn draw_text(f: &mut Frame, ed: &Editor, area: Rect, digits: usize, gutter: u16) {
    let buf = f.buffer_mut();
    let body_w = (area.width - gutter) as usize;
    for i in 0..area.height as usize {
        let row = ed.top + i;
        let y = area.y + i as u16;
        if row >= ed.doc.line_count() {
            buf.set_string(area.x, y, format!("{:>digits$} ", "~"), dim());
            continue;
        }
        let current = row == ed.cursor.row;
        // Current line: bold text and number; the rest of the numbers are dim.
        let line_style = if current { bold() } else { base() };
        let num_style = if current { bold() } else { dim() };
        buf.set_string(area.x, y, format!("{:>digits$} ", row + 1), num_style);
        buf.set_string(area.x + digits as u16 + 1, y, "│", dim());
        // Paint the line background, then the visible graphemes over it.
        let x0 = area.x + gutter;
        buf.set_string(x0, y, " ".repeat(body_w), line_style);
        let line = ed.doc.line(row);
        let mut dc = 0usize;
        for (byte, g) in line.grapheme_indices(true) {
            let w = cell_width(g, dc);
            let at = dc;
            dc += w;
            if at + w <= ed.left {
                continue;
            }
            if at < ed.left || at + w - ed.left > body_w {
                break; // a wide grapheme cut by either edge is left blank
            }
            let hit = ed
                .highlight
                .is_some_and(|(r, s, e)| r == row && byte >= s && byte < e);
            let style = if hit {
                line_style.add_modifier(Modifier::REVERSED)
            } else {
                line_style
            };
            let shown = if g == "\t" {
                " ".repeat(w)
            } else if g.chars().next().is_some_and(char::is_control) {
                "?".to_string()
            } else {
                g.to_string()
            };
            buf.set_string(x0 + (at - ed.left) as u16, y, shown, style);
        }
    }
}

fn draw_status(f: &mut Frame, ed: &Editor, area: Rect) {
    let name = ed.doc.path().map_or("[sin nombre]".to_string(), |p| {
        p.file_name()
            .unwrap_or(p.as_os_str())
            .to_string_lossy()
            .into_owned()
    });
    let dirty = if ed.doc.is_dirty() { " •" } else { "" };
    let pos = format!(
        "Ln {}, Col {}  {}  UTF-8 ",
        ed.cursor.row + 1,
        ed.cursor_display_col() + 1,
        if ed.doc.is_crlf() { "CRLF" } else { "LF" }
    );
    let style = inverse();
    let w = area.width as usize;
    let left = format!(" EDITTUI  {name}{dirty}");
    let pad = w.saturating_sub(left.chars().count() + pos.chars().count());
    let line = Line::from(vec![
        Span::styled(" EDITTUI", inverse().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {name}{dirty}"), style),
        Span::styled(" ".repeat(pad), style),
        Span::styled(pos, style),
    ]);
    f.render_widget(Paragraph::new(line).style(style), area);
}

fn draw_hint(f: &mut Frame, ed: &Editor, area: Rect) {
    let style = dim();
    let line = match &ed.mode {
        Mode::Prompt { kind, input } => {
            let label = match kind {
                PromptKind::Find => "Buscar: ",
                PromptKind::Goto => "Ir a la línea: ",
                PromptKind::SaveAs => "Guardar como: ",
            };
            Line::from(vec![
                Span::styled(label, bold()),
                Span::styled(input.clone(), Style::new()),
                Span::styled("   Enter aceptar · Esc cancelar", style),
            ])
        }
        Mode::ConfirmQuit => Line::from(Span::styled(
            " ! Hay cambios sin guardar: (s) guardar y salir · (n) salir sin guardar · Esc seguir ",
            inverse().add_modifier(Modifier::BOLD),
        )),
        _ => match &ed.message {
            Some(m) => Line::from(Span::styled(m.clone(), bold())),
            None => Line::from(Span::styled(
                "^S guardar  ^Q salir  ^Z deshacer  ^F buscar  ^G ir a  ^K cortar  ^U pegar  F1 ayuda",
                style,
            )),
        },
    };
    f.render_widget(Paragraph::new(line).style(base()), area);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let h = HELP.len() as u16 + 4;
    let w = 64.min(area.width);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h.min(area.height),
    );
    f.render_widget(Clear, r);
    let mut lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<24}"), bold()),
                Span::styled(*d, Style::new()),
            ])
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " Cualquier tecla cierra la ayuda",
        dim(),
    )));
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(dim())
        .title_style(bold())
        .title(" AYUDA // EDITTUI ")
        .title_alignment(Alignment::Left)
        .style(base());
    f.render_widget(Paragraph::new(lines).block(block), r);
}

fn place_cursor(f: &mut Frame, ed: &Editor, text: Rect, gutter: u16, status: Rect) {
    match &ed.mode {
        Mode::Prompt { kind, input } => {
            let label = match kind {
                PromptKind::Find => "Buscar: ",
                PromptKind::Goto => "Ir a la línea: ",
                PromptKind::SaveAs => "Guardar como: ",
            };
            let x = status.x + (label.chars().count() + input.chars().count()) as u16;
            f.set_cursor_position((x.min(status.right().saturating_sub(1)), status.y + 1));
        }
        Mode::Edit => {
            let x = text.x + gutter + (ed.cursor_display_col() - ed.left) as u16;
            let y = text.y + (ed.cursor.row - ed.top) as u16;
            f.set_cursor_position((x, y));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(ed: &mut Editor, w: u16, h: u16) -> Vec<String> {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, ed)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < w {
                    let sym = buf[(x, y)].symbol();
                    row.push_str(sym);
                    // A wide symbol owns the next cell too.
                    x += unicode_width::UnicodeWidthStr::width(sym).max(1) as u16;
                }
                row
            })
            .collect()
    }

    fn editor_with(text: &str) -> Editor {
        let path =
            std::env::temp_dir().join(format!("edittui-ui-{}-{}", std::process::id(), text.len()));
        std::fs::write(&path, text).unwrap();
        let ed = Editor::new(Document::open(&path).unwrap());
        let _ = std::fs::remove_file(&path);
        ed
    }

    #[test]
    fn draws_gutter_tabs_wide_chars_and_status() {
        let mut ed = editor_with("a\tb\n日本 ñ\n");
        let s = screen(&mut ed, 60, 8);
        assert!(s[0].starts_with("  1 │a   b"), "{:?}", s[0]);
        assert!(s[1].starts_with("  2 │日本 ñ"), "{:?}", s[1]);
        assert!(s[3].starts_with("  ~"));
        assert!(s[6].contains("edittui-ui") && s[6].contains("Ln 1, Col 1"));
    }

    #[test]
    fn scrolls_horizontally_and_vertically_with_the_cursor() {
        let mut ed = editor_with(&format!("{}\n", "x".repeat(100)));
        ed.cursor = crate::document::Pos::new(0, 100);
        let s = screen(&mut ed, 60, 6);
        assert!(ed.left > 0);
        assert!(s[0].starts_with("  1 │xxx"));
        assert!(s[4].contains("Col 101"));
    }

    #[test]
    fn uses_only_default_colours_so_it_works_on_the_linux_console() {
        use ratatui::style::Color;
        let mut ed = editor_with("hola\nmundo\n");
        ed.highlight = Some((0, 0, 2));
        let mut t = Terminal::new(TestBackend::new(60, 8)).unwrap();
        t.draw(|f| draw(f, &mut ed)).unwrap();
        for cell in t.backend().buffer().content() {
            assert_eq!(cell.fg, Color::Reset);
            assert_eq!(cell.bg, Color::Reset);
        }
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        let mut ed = editor_with("x");
        for (w, h) in [(1, 1), (10, 2), (19, 3), (20, 4)] {
            screen(&mut ed, w, h);
        }
    }
}
