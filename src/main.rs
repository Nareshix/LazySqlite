use arboard::Clipboard;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders},
};
use ratatui_textarea::{CursorMove, Input, Key, TextArea};

#[derive(PartialEq)]
enum Focus {
    Box1,
    Box2,
    Box3,
}

impl Focus {
    fn next(&self) -> Self {
        match self {
            Focus::Box1 => Focus::Box2,
            Focus::Box2 => Focus::Box3,
            Focus::Box3 => Focus::Box1,
        }
    }
}

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let mut focus = Focus::Box1;

    let mut textarea = TextArea::default();
    textarea.set_block(Block::default().title(" Box 2 ").borders(Borders::ALL));
    let mut clipboard = Clipboard::new().unwrap();

    loop {
        terminal.draw(|frame| {
            let area = frame.area();

            // Split horizontally: left column | right big box
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(65), // big left box
                    Constraint::Percentage(35), // right column
                ])
                .split(area);

            // Split the right column vertically into 2 boxes
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(50), // box 1
                    Constraint::Percentage(50), // box 2
                ])
                .split(cols[1]);

            // highlight border if focused
            let focused = Style::default().fg(Color::Blue);
            let normal = Style::default().fg(Color::Gray);

            let box1 = Block::default()
                .title(" Box 1 ")
                .borders(Borders::ALL)
                .border_style(if focus == Focus::Box1 {
                    focused
                } else {
                    normal
                });

            textarea.set_block(
                Block::default()
                    .title(" Box 2 ")
                    .borders(Borders::ALL)
                    .border_style(if focus == Focus::Box2 {
                        focused
                    } else {
                        normal
                    }),
            );

            let box3 = Block::default()
                .title(" Box 3 ")
                .borders(Borders::ALL)
                .border_style(if focus == Focus::Box3 {
                    focused
                } else {
                    normal
                });
            frame.render_widget(box1, cols[0]); // big box on left
            frame.render_widget(&textarea, rows[0]); // top-right, textarea
            frame.render_widget(box3, rows[1]); // bottom-right
        })?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Tab => {
                    focus = focus.next();
                }
                KeyCode::Char('q') if focus != Focus::Box2 => break,
                _ => {
                    if focus == Focus::Box2 {
                        match Input::from(key) {
                            // Select all
                            Input {
                                key: Key::Char('a'),
                                ctrl: true,
                                ..
                            } => {
                                textarea.select_all();
                            }
                            // Undo
                            Input {
                                key: Key::Char('z'),
                                ctrl: true,
                                shift: false,
                                ..
                            } => {
                                textarea.undo();
                            }
                            // Redo (Ctrl+Shift+Z or Ctrl+Y)
                            Input {
                                key: Key::Char('z'),
                                ctrl: true,
                                shift: true,
                                ..
                            }
                            | Input {
                                key: Key::Char('y'),
                                ctrl: true,
                                ..
                            } => {
                                textarea.redo();
                            }
                            // Copy → system clipboard
                            Input {
                                key: Key::Char('c'),
                                ctrl: true,
                                ..
                            } => {
                                textarea.copy();
                                if !textarea.yank_text().is_empty() {
                                    clipboard.set_text(textarea.yank_text()).ok();
                                }
                            }
                            // Cut → system clipboard
                            Input {
                                key: Key::Char('x'),
                                ctrl: true,
                                ..
                            } => {
                                textarea.cut();
                                if !textarea.yank_text().is_empty() {
                                    clipboard.set_text(textarea.yank_text()).ok();
                                }
                            }
                            // Paste ← system clipboard (override Ctrl+V = PageDown)
                            Input {
                                key: Key::Char('v'),
                                ctrl: true,
                                ..
                            } => {
                                if let Ok(text) = clipboard.get_text() {
                                    textarea.set_yank_text(text);
                                    textarea.paste();
                                }
                            }
                            // Everything else: arrows, backspace, typing, Ctrl+F/B, Home/End, etc.
                            input => {
                                textarea.input_without_shortcuts(input);
                            }
                        }
                    }
                }
            }
        }
    }

    ratatui::restore();
    Ok(())
}
