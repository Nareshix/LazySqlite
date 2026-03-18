use std::sync::{Arc, mpsc};
use std::thread;

use arboard::Clipboard;
use lazysql::LazyConnection;
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, List, ListItem, Row, Table, TableState},
};
use ratatui_textarea::{Input, Key, TextArea};

mod autocomplete;
mod sqlite;

use autocomplete::{Autocomplete, current_word, popup_rect};
use sqlite::{DbCommand, DbResponse, db_thread, is_query};

#[derive(PartialEq, Debug)]
enum Focus {
    Box1,
    Box2,
    Box3,
}

struct Model {
    focus: Focus,
    textarea: TextArea<'static>,
    clipboard: Clipboard,
    ac: Autocomplete,
    copied_at: Option<std::time::Instant>,

    cmd_tx: mpsc::Sender<DbCommand>,
    resp_rx: mpsc::Receiver<DbResponse>,
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    db_status: String,
    db_loading: bool,

    table_state: TableState,

    cursor_visible: bool,
    last_blink: std::time::Instant,
    terminal_focused: bool,  // false when VSCode steals focus
}

impl Model {
    fn new(cmd_tx: mpsc::Sender<DbCommand>, resp_rx: mpsc::Receiver<DbResponse>) -> Self {
        let mut textarea = TextArea::default();
        textarea.set_cursor_style(Style::default());
        textarea.set_cursor_line_style(Style::default());

        Self {
            focus: Focus::Box2,
            textarea,
            clipboard: Clipboard::new().unwrap(),
            ac: Autocomplete::new(vec![
                "SELECT".into(), "FROM".into(), "WHERE".into(), "INSERT".into(),
                "UPDATE".into(), "DELETE".into(), "CREATE".into(), "DROP".into(),
                "ALTER".into(), "INDEX".into(), "PRAGMA".into(), "JOIN".into(),
                "VALUES".into(), "LIMIT".into(), "DISTINCT".into(),
            ]),
            copied_at: None,
            cmd_tx,
            resp_rx,
            columns: vec![],
            rows: vec![],
            db_status: String::from("Ready"),
            db_loading: false,
            table_state: TableState::default(),
            cursor_visible: true,
            last_blink: std::time::Instant::now(),
            terminal_focused: true,
        }
    }

    fn table_next(&mut self) {
        if self.rows.is_empty() { return; }
        let i = match self.table_state.selected() {
            Some(i) => (i + 1).min(self.rows.len() - 1),
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn table_prev(&mut self) {
        if self.rows.is_empty() { return; }
        let i = match self.table_state.selected() {
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.table_state.select(Some(i));
    }

    fn tick_blink(&mut self) {
        if !self.terminal_focused {
            // Terminal is not focused — freeze cursor off so it's clear
            // to the user that this pane is not active.
            self.cursor_visible = false;
            return;
        }
        if self.last_blink.elapsed() >= std::time::Duration::from_millis(500) {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = std::time::Instant::now();
        }
    }
}

enum Msg {
    FocusBox(Focus),
    Quit,
    SubmitQuery,
    DbResponse(DbResponse),
    TableNext,
    TablePrev,
    AcNext,
    AcPrev,
    AcDismiss,
    AcAccept,
    SelectAll,
    Undo, Redo, Copy, Cut, Paste,
    EditorAction(Input),
}

fn key_to_msg(model: &Model, key: event::KeyEvent) -> Option<Msg> {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    // Global navigation: Ctrl+Tab (forward) / Ctrl+Shift+Tab (backward)
    // Cycle order: Box1 -> Box2 -> Box3 -> Box1
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::BackTab {
        // Ctrl+Shift+Tab — go backward
        let prev = match &model.focus {
            Focus::Box1 => Focus::Box3,
            Focus::Box2 => Focus::Box1,
            Focus::Box3 => Focus::Box2,
        };
        return Some(Msg::FocusBox(prev));
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Tab {
        // Ctrl+Tab — go forward
        let next = match &model.focus {
            Focus::Box1 => Focus::Box2,
            Focus::Box2 => Focus::Box3,
            Focus::Box3 => Focus::Box1,
        };
        return Some(Msg::FocusBox(next));
    }

    let input = Input::from(key);

    // Autocomplete overlay — handled before focus logic
    if model.ac.visible {
        match input {
            Input { key: Key::Esc, .. }               => return Some(Msg::AcDismiss),
            Input { key: Key::Down, alt: false, .. }  => return Some(Msg::AcNext),
            Input { key: Key::Up,   alt: false, .. }  => return Some(Msg::AcPrev),
            Input { key: Key::Tab | Key::Enter, .. }  => return Some(Msg::AcAccept),
            _ => {}
        }
    }

    match model.focus {
        Focus::Box2 => {
            match input {
                // Ctrl+Enter to run
                Input { key: Key::Enter, ctrl: true, .. } => Some(Msg::SubmitQuery),
                Input { key: Key::Char('a'), ctrl: true, .. } => Some(Msg::SelectAll),
                Input { key: Key::Char('z'), ctrl: true, shift: false, .. } => Some(Msg::Undo),
                Input { key: Key::Char('z'), ctrl: true, shift: true, .. } |
                Input { key: Key::Char('y'), ctrl: true, .. } => Some(Msg::Redo),
                Input { key: Key::Char('c'), ctrl: true, .. } => Some(Msg::Copy),
                Input { key: Key::Char('x'), ctrl: true, .. } => Some(Msg::Cut),
                Input { key: Key::Char('v'), ctrl: true, .. } => Some(Msg::Paste),
                other => Some(Msg::EditorAction(other)),
            }
        }
        Focus::Box3 => {
            match input {
                Input { key: Key::Down, .. } => Some(Msg::TableNext),
                Input { key: Key::Up, .. }   => Some(Msg::TablePrev),
                _ => None,
            }
        }
        Focus::Box1 => {
            match input {
                Input { key: Key::Char('q'), .. } => Some(Msg::Quit),
                _ => None,
            }
        }
    }
}

fn update(model: &mut Model, msg: Msg) -> Option<()> {
    match msg {
        Msg::Quit => return None,
        Msg::FocusBox(f) => {
            model.focus = f;
            model.ac.dismiss();
            model.cursor_visible = true;
            model.last_blink = std::time::Instant::now();
        }
        Msg::SubmitQuery => {
            if model.db_loading { return Some(()); }
            let sql: String = model.textarea.lines().join("\n").trim().to_string();
            if sql.is_empty() { return Some(()); }
            let cmd = if is_query(&sql) { DbCommand::Query(sql) } else { DbCommand::Execute(sql) };
            model.cmd_tx.send(cmd).ok();
            model.db_loading = true;
            model.db_status = String::from("Running...");
        }
        Msg::DbResponse(resp) => {
            model.db_loading = false;
            match resp {
                DbResponse::Rows { columns, rows, elapsed } => {
                    model.columns = columns;
                    model.rows = rows;
                    model.db_status = format!("{} rows  {:.2?}", model.rows.len(), elapsed);
                    model.table_state = TableState::default();
                }
                DbResponse::RowsAffected(n, elapsed) => {
                    model.columns = vec![];
                    model.rows = vec![];
                    model.db_status = format!("{} rows affected  {:.2?}", n, elapsed);
                }
                DbResponse::Tables(names) => {
                    // Inject table names into autocomplete word list
                    model.ac.add_words(names);
                }
                DbResponse::Error(e) => {
                    model.columns = vec![];
                    model.rows = vec![];
                    model.db_status = format!("Error: {}", e);
                }
            }
        }
        Msg::TableNext => model.table_next(),
        Msg::TablePrev => model.table_prev(),
        Msg::AcNext    => model.ac.next(),
        Msg::AcPrev    => model.ac.prev(),
        Msg::AcDismiss => model.ac.dismiss(),
        Msg::AcAccept  => {
            if let Some(word) = model.ac.selected() {
                let partial = current_word(&model.textarea);
                for _ in 0..partial.chars().count() { model.textarea.delete_char(); }
                model.textarea.insert_str(word);
                model.ac.dismiss();
            }
        }
        Msg::SelectAll => model.textarea.select_all(),
        Msg::Undo => { model.textarea.undo(); }
        Msg::Redo => { model.textarea.redo(); }
        Msg::Copy => {
            model.textarea.copy();
            let yanked = model.textarea.yank_text();
            if !yanked.is_empty() {
                model.clipboard.set_text(yanked).ok();
                model.copied_at = Some(std::time::Instant::now());
            }
        }
        Msg::Cut => {
            model.textarea.cut();
            let yanked = model.textarea.yank_text();
            if !yanked.is_empty() { model.clipboard.set_text(yanked).ok(); }
        }
        Msg::Paste => {
            if let Ok(text) = model.clipboard.get_text() {
                model.textarea.set_yank_text(text);
                model.textarea.paste();
            }
        }
        Msg::EditorAction(input) => {
            model.textarea.input(input);
            let word = current_word(&model.textarea);
            if !word.is_empty() { model.ac.update(&word); } else { model.ac.dismiss(); }
        }
    }
    Some(())
}

fn view(model: &mut Model, frame: &mut Frame) {
    let area = frame.area();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
        .split(area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(cols[1]);

    let focused_style = Style::default().fg(Color::Blue);
    let normal_style  = Style::default().fg(Color::DarkGray);

    // Box 1 - Tables
    frame.render_widget(
        Block::default().title(" Tables ").borders(Borders::ALL)
            .border_style(if model.focus == Focus::Box1 { focused_style } else { normal_style }),
        cols[0],
    );

    // Box 2 - Editor
    let editor_area = rows[0];
    let editor_title = if model.copied_at.map_or(false, |t| t.elapsed().as_secs() < 1) {
        " SQL Editor  Copied! "
    } else {
        " SQL Editor  Ctrl+Enter to Run  Ctrl+Tab to switch "
    };
    model.textarea.set_block(
        Block::default().title(editor_title).borders(Borders::ALL)
            .border_style(if model.focus == Focus::Box2 { focused_style } else { normal_style }),
    );

    if model.focus == Focus::Box2 {
        if model.cursor_visible {
            model.textarea.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        } else {
            model.textarea.set_cursor_style(Style::default());
        }
    } else {
        model.textarea.set_cursor_style(Style::default());
    }

    frame.render_widget(&model.textarea, editor_area);

    if model.focus == Focus::Box2 && model.cursor_visible {
        let (cursor_row, cursor_col) = model.textarea.cursor();
        let cur_x = editor_area.x + 1 + cursor_col as u16;
        let cur_y = editor_area.y + 1 + cursor_row as u16;
        // Clamp to the inner area of the editor box so the hardware cursor
        // never escapes into another pane when the text scrolls out of view.
        let max_x = editor_area.right().saturating_sub(2);
        let max_y = editor_area.bottom().saturating_sub(2);
        if cur_x <= max_x && cur_y <= max_y {
            frame.set_cursor_position((cur_x, cur_y));
        }
    }

    // Box 3 - Results
    let results_block = Block::default()
        .title(format!(" Results  {} ", model.db_status))
        .borders(Borders::ALL)
        .border_style(if model.focus == Focus::Box3 { focused_style } else { normal_style });

    if model.columns.is_empty() {
        frame.render_widget(results_block, rows[1]);
    } else {
        let data_rows: Vec<Row> = model.rows.iter()
            .map(|r| Row::new(r.iter().map(|c| Cell::from(c.as_str()))))
            .collect();
        let widths = vec![Constraint::Percentage(100 / model.columns.len() as u16); model.columns.len()];
        let table = Table::new(data_rows, widths)
            .header(Row::new(model.columns.iter().map(|c| {
                Cell::from(c.as_str()).style(Style::default().fg(Color::Yellow))
            })))
            .block(results_block)
            .row_highlight_style(Style::default().bg(Color::Rgb(30, 30, 60)));
        frame.render_stateful_widget(table, rows[1], &mut model.table_state);
    }

    // Autocomplete popup
    if model.ac.visible && model.focus == Focus::Box2 {
        let (r, c) = model.textarea.cursor();
        let popup = popup_rect(editor_area, area, r, c);
        // Only draw if the popup has a usable size
        if popup.width > 2 && popup.height > 2 {
            let items: Vec<ListItem> = model.ac.matches.iter()
                .map(|m| ListItem::new(m.as_str()))
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" Suggest "))
                .highlight_style(Style::default().bg(Color::Blue));
            frame.render_widget(ratatui::widgets::Clear, popup);
            frame.render_stateful_widget(list, popup, &mut model.ac.state);
        }
    }
}

fn main() -> std::io::Result<()> {
    use ratatui::crossterm::{
        execute,
        event::{
            KeyboardEnhancementFlags,
            PushKeyboardEnhancementFlags,
            PopKeyboardEnhancementFlags,
        },
    };

    let conn = LazyConnection::open("viewer.db").expect("failed to open database");
    let (cmd_tx, cmd_rx) = mpsc::channel::<DbCommand>();
    let (resp_tx, resp_rx) = mpsc::channel::<DbResponse>();
    let conn_bg = Arc::clone(&conn);
    thread::spawn(move || db_thread(conn_bg, cmd_rx, resp_tx));

    let mut terminal = ratatui::init();

    let kitty_supported = execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    ).is_ok();

    // Enable focus-change events so we know when VSCode steals the terminal.
    execute!(std::io::stdout(), ratatui::crossterm::event::EnableFocusChange).ok();

    let mut model = Model::new(cmd_tx, resp_rx);

    // Fetch table names on startup so they appear in autocomplete immediately
    model.cmd_tx.send(DbCommand::GetTables).ok();

    loop {
        while let Ok(resp) = model.resp_rx.try_recv() {
            update(&mut model, Msg::DbResponse(resp));
        }

        model.tick_blink();

        terminal.draw(|frame| view(&mut model, frame))?;

        if event::poll(std::time::Duration::from_millis(16))? {
            match event::read()? {
                Event::FocusGained => {
                    model.terminal_focused = true;
                    model.cursor_visible = true;
                    model.last_blink = std::time::Instant::now();
                }
                Event::FocusLost => {
                    model.terminal_focused = false;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(msg) = key_to_msg(&model, key) {
                        if update(&mut model, msg).is_none() { break; }
                    }
                }
                _ => {}
            }
        }
    }

    model.cmd_tx.send(DbCommand::Shutdown).ok();
    execute!(std::io::stdout(), ratatui::crossterm::event::DisableFocusChange).ok();
    if kitty_supported {
        execute!(std::io::stdout(), PopKeyboardEnhancementFlags).ok();
    }
    ratatui::restore();
    Ok(())
}