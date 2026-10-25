//! editTUI — a small text editor for the terminal. See the README for the keys.

mod document;
mod editor;
mod text;
mod ui;

use std::io;
use std::path::Path;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyEventKind};

use document::Document;
use editor::Editor;

const USAGE: &str = "Uso: edittui [ARCHIVO]\n\nEditor de texto para el terminal. F1 dentro del editor muestra las teclas.";

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("edittui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let doc = match args.first() {
        Some(path) => match Document::open(Path::new(path)) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("edittui: {path}: {e}");
                std::process::exit(1);
            }
        },
        None => Document::empty(),
    };
    let mut editor = Editor::new(doc);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut editor);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, editor: &mut Editor) -> io::Result<()> {
    while !editor.quit {
        terminal.draw(|f| ui::draw(f, editor))?;
        if event::poll(Duration::from_millis(500))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => editor.handle_key(k),
                _ => {} // resize and the rest: the next draw adapts
            }
        }
    }
    Ok(())
}
