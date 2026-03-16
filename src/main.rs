use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders},
};

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

            let box2 = Block::default()
                .title(" Box 2 ")
                .borders(Borders::ALL)
                .border_style(if focus == Focus::Box2 {
                    focused
                } else {
                    normal
                });

            let box3 = Block::default()
                .title(" Box 3 ")
                .borders(Borders::ALL)
                .border_style(if focus == Focus::Box3 {
                    focused
                } else {
                    normal
                });
            frame.render_widget(box1, cols[0]); // big box on left
            frame.render_widget(box2, rows[0]); // small top-right
            frame.render_widget(box3, rows[1]); // small bottom-right
        })?;

        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Tab => focus = focus.next(),
                    KeyCode::Char('q') => break,
                    _ => {}
                }
            }
    }

    ratatui::restore();
    Ok(())
}
