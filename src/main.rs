use std::sync::{Arc, mpsc};
use std::thread;

use arboard::Clipboard;
use lazysql::LazyConnection;
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Row, Table, TableState},
};
use ratatui_textarea::{Input, Key, TextArea};

mod sqlite;
use sqlite::{DbCommand, DbResponse, TableSchema, db_thread, is_query};

// ── Focus ────────────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum Focus { Sidebar, Editor, Results }

// ── Model ────────────────────────────────────────────────────────────────────

struct Model {
    focus:    Focus,
    textarea: TextArea<'static>,
    clipboard: Clipboard,

    cmd_tx:  mpsc::Sender<DbCommand>,
    resp_rx: mpsc::Receiver<DbResponse>,

    // Results
    columns:     Vec<String>,
    rows:        Vec<Vec<String>>,
    status:      String,
    loading:     bool,
    result_state: TableState,
    col_offset:  usize,

    // Sidebar
    schema:          Vec<TableSchema>,
    sidebar_state:   ListState,   // selected table index
    sidebar_items:   Vec<String>, // flat rendered lines

    // Cursor blink
    cursor_visible:    bool,
    last_blink:        std::time::Instant,
    terminal_focused:  bool,
}

impl Model {
    fn new(cmd_tx: mpsc::Sender<DbCommand>, resp_rx: mpsc::Receiver<DbResponse>) -> Self {
        let mut textarea = TextArea::default();
        textarea.set_cursor_style(Style::default());
        textarea.set_cursor_line_style(Style::default());
        textarea.set_line_number_style(Style::default().fg(Color::Rgb(80, 80, 100)));

        Self {
            focus: Focus::Editor,
            textarea,
            clipboard: Clipboard::new().unwrap(),
            cmd_tx, resp_rx,
            columns: vec![], rows: vec![],
            status: "Ready".into(), loading: false,
            result_state: TableState::default(), col_offset: 0,
            schema: vec![], sidebar_state: ListState::default(), sidebar_items: vec![],
            cursor_visible: true, last_blink: std::time::Instant::now(),
            terminal_focused: true,
        }
    }

    fn tick_blink(&mut self) {
        if !self.terminal_focused { self.cursor_visible = false; return; }
        if self.last_blink.elapsed().as_millis() >= 500 {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = std::time::Instant::now();
        }
    }

    fn rebuild_sidebar(&mut self) {
        self.sidebar_items.clear();
        for t in &self.schema {
            self.sidebar_items.push(format!(" {}", t.name));
            for col in &t.columns {
                let mut tag = String::new();
                if col.pk { tag.push_str(" PK"); }
                if let Some(fk) = &col.fk_to {
                    tag.push_str(&format!(" FK→{}", fk));
                }
                let typ = if col.typ.is_empty() { "?".into() } else { col.typ.clone() };
                self.sidebar_items.push(format!("   {} {}{}",
                    col.name, typ.to_uppercase(), tag));
            }
        }
    }

    fn run_query(&mut self, sql: String) {
        if self.loading { return; }
        let cmd = if is_query(&sql) { DbCommand::Query(sql) } else { DbCommand::Execute(sql) };
        self.cmd_tx.send(cmd).ok();
        self.loading = true;
        self.status = "Running…".into();
        self.col_offset = 0;
    }
}

// ── Column widths ─────────────────────────────────────────────────────────────

fn col_widths(columns: &[String], rows: &[Vec<String>], offset: usize) -> Vec<Constraint> {
    columns[offset..].iter().enumerate().map(|(i, h)| {
        let ai = i + offset;
        let max = rows.iter().filter_map(|r| r.get(ai)).map(|s| s.len()).max().unwrap_or(0);
        let w = (h.len().max(max).max(4) as u16).min(40);
        Constraint::Min(w)
    }).collect()
}

// ── Messages ──────────────────────────────────────────────────────────────────

enum Msg {
    FocusNext, FocusPrev,
    Submit,
    Up, Down, Left, Right,
    Copy, Cut, Paste,
    Undo, Redo, SelectAll,
    Editor(Input),
    DbResp(DbResponse),
    Quit,
}

fn key_to_msg(model: &Model, key: event::KeyEvent) -> Option<Msg> {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    // Ctrl+Tab / Ctrl+Shift+Tab — always cycle focus
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Tab     => return Some(Msg::FocusNext),
            KeyCode::BackTab => return Some(Msg::FocusPrev),
            _ => {}
        }
    }

    let input = Input::from(key);

    match model.focus {
        Focus::Editor => match input {
            Input { key: Key::Enter, ctrl: true,  .. } => Some(Msg::Submit),
            Input { key: Key::Char('a'), ctrl: true, .. } => Some(Msg::SelectAll),
            Input { key: Key::Char('z'), ctrl: true, shift: false, .. } => Some(Msg::Undo),
            Input { key: Key::Char('z'), ctrl: true, shift: true,  .. } |
            Input { key: Key::Char('y'), ctrl: true, .. } => Some(Msg::Redo),
            Input { key: Key::Char('c'), ctrl: true, .. } => Some(Msg::Copy),
            Input { key: Key::Char('x'), ctrl: true, .. } => Some(Msg::Cut),
            Input { key: Key::Char('v'), ctrl: true, .. } => Some(Msg::Paste),
            other => Some(Msg::Editor(other)),
        },
        Focus::Results => match input {
            Input { key: Key::Up,    .. } => Some(Msg::Up),
            Input { key: Key::Down,  .. } => Some(Msg::Down),
            Input { key: Key::Left,  .. } => Some(Msg::Left),
            Input { key: Key::Right, .. } => Some(Msg::Right),
            _ => None,
        },
        Focus::Sidebar => match input {
            Input { key: Key::Up,    .. } => Some(Msg::Up),
            Input { key: Key::Down,  .. } => Some(Msg::Down),
            Input { key: Key::Enter, .. } => Some(Msg::Submit),
            Input { key: Key::Char('q'), .. } => Some(Msg::Quit),
            _ => None,
        },
    }
}

fn update(model: &mut Model, msg: Msg) -> bool {
    match msg {
        Msg::Quit => return false,

        Msg::FocusNext => {
            model.focus = match model.focus {
                Focus::Sidebar => Focus::Editor,
                Focus::Editor  => Focus::Results,
                Focus::Results => Focus::Sidebar,
            };
            model.cursor_visible = true;
            model.last_blink = std::time::Instant::now();
        }
        Msg::FocusPrev => {
            model.focus = match model.focus {
                Focus::Sidebar => Focus::Results,
                Focus::Editor  => Focus::Sidebar,
                Focus::Results => Focus::Editor,
            };
            model.cursor_visible = true;
            model.last_blink = std::time::Instant::now();
        }

        Msg::Submit => match model.focus {
            Focus::Editor => {
                let sql = model.textarea.lines().join("\n").trim().to_string();
                if !sql.is_empty() { model.run_query(sql); }
            }
            Focus::Sidebar => {
                // Find which table the selected sidebar line belongs to
                if let Some(sel) = model.sidebar_state.selected() {
                    let line = model.sidebar_items.get(sel).cloned().unwrap_or_default();
                    // Table header lines start with " <name>" (single leading space)
                    if line.starts_with(" ") && !line.starts_with("   ") {
                        let name = line.trim().to_string();
                        model.run_query(format!("SELECT * FROM {} LIMIT 200", name));
                    }
                }
            }
            _ => {}
        },

        Msg::Up => match model.focus {
            Focus::Results => {
                if !model.rows.is_empty() {
                    let i = model.result_state.selected()
                        .map(|i| i.saturating_sub(1)).unwrap_or(0);
                    model.result_state.select(Some(i));
                }
            }
            Focus::Sidebar => {
                let i = model.sidebar_state.selected()
                    .map(|i| i.saturating_sub(1)).unwrap_or(0);
                model.sidebar_state.select(Some(i));
            }
            _ => {}
        },
        Msg::Down => match model.focus {
            Focus::Results => {
                if !model.rows.is_empty() {
                    let max = model.rows.len() - 1;
                    let i = model.result_state.selected()
                        .map(|i| (i + 1).min(max)).unwrap_or(0);
                    model.result_state.select(Some(i));
                }
            }
            Focus::Sidebar => {
                let max = model.sidebar_items.len().saturating_sub(1);
                let i = model.sidebar_state.selected()
                    .map(|i| (i + 1).min(max)).unwrap_or(0);
                model.sidebar_state.select(Some(i));
            }
            _ => {}
        },
        Msg::Left  => { model.col_offset = model.col_offset.saturating_sub(1); }
        Msg::Right => { if model.col_offset + 1 < model.columns.len() { model.col_offset += 1; } }

        Msg::Copy => {
            model.textarea.copy();
            if let Ok(()) = model.clipboard.set_text(model.textarea.yank_text()) {}
        }
        Msg::Cut => {
            model.textarea.cut();
            if let Ok(()) = model.clipboard.set_text(model.textarea.yank_text()) {}
        }
        Msg::Paste => {
            if let Ok(text) = model.clipboard.get_text() {
                model.textarea.set_yank_text(text);
                model.textarea.paste();
            }
        }
        Msg::Undo     => { model.textarea.undo(); }
        Msg::Redo     => { model.textarea.redo(); }
        Msg::SelectAll => model.textarea.select_all(),
        Msg::Editor(i) => { model.textarea.input(i); }

        Msg::DbResp(resp) => {
            model.loading = false;
            match resp {
                DbResponse::Rows { columns, rows, elapsed } => {
                    model.columns = columns;
                    model.rows = rows;
                    model.status = format!("{} rows  {:.2?}", model.rows.len(), elapsed);
                    model.result_state = TableState::default();
                    model.col_offset = 0;
                }
                DbResponse::RowsAffected(n, elapsed) => {
                    model.columns = vec![]; model.rows = vec![];
                    model.status = format!("{} rows affected  {:.2?}", n, elapsed);
                }
                DbResponse::Schema(schema) => {
                    model.schema = schema;
                    model.rebuild_sidebar();
                    if !model.sidebar_items.is_empty() {
                        model.sidebar_state.select(Some(0));
                    }
                }
                DbResponse::Error(e) => {
                    model.columns = vec![]; model.rows = vec![];
                    model.status = format!("Error: {}", e);
                }
            }
        }
    }
    true
}

// ── View ──────────────────────────────────────────────────────────────────────

fn view(model: &mut Model, frame: &mut Frame) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(22), Constraint::Percentage(78)])
        .split(area);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(chunks[1]);

    let focused  = Style::default().fg(Color::Blue);
    let unfocused = Style::default().fg(Color::DarkGray);

    // ── Sidebar ───────────────────────────────────────────────────────────────
    let sb_style = if model.focus == Focus::Sidebar { focused } else { unfocused };
    let sb_block = Block::default().title(" Schema (Ctrl+Tab) ").borders(Borders::ALL).border_style(sb_style);

    let items: Vec<ListItem> = model.sidebar_items.iter().map(|line| {
        // Table header = single leading space, column = triple space indent
        if line.starts_with("   ") {
            // column line — dim the type/tag portion
            let parts: Vec<&str> = line.trim().splitn(2, ' ').collect();
            let (col_name, rest) = if parts.len() == 2 { (parts[0], parts[1]) } else { (line.trim(), "") };
            ListItem::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(col_name, Style::default().fg(Color::White)),
                Span::raw(" "),
                Span::styled(rest, Style::default().fg(Color::Rgb(120, 120, 160))),
            ]))
        } else {
            // Table name — bold
            ListItem::new(Line::from(vec![
                Span::styled(line.as_str(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            ]))
        }
    }).collect();

    let list = List::new(items)
        .block(sb_block)
        .highlight_style(Style::default().bg(Color::Rgb(30, 30, 60)));
    frame.render_stateful_widget(list, chunks[0], &mut model.sidebar_state);

    // ── Editor ─────────────────────────────────────────────────────────────────
    let ed_style = if model.focus == Focus::Editor { focused } else { unfocused };
    let (crow, ccol) = model.textarea.cursor();
    let ed_title = format!(" SQL Editor  Ctrl+Enter to Run  Ln {} Col {} ", crow + 1, ccol + 1);
    model.textarea.set_block(
        Block::default().title(ed_title).borders(Borders::ALL).border_style(ed_style)
    );
    frame.render_widget(&model.textarea, right[0]);

    // Hardware cursor
    if model.focus == Focus::Editor && model.cursor_visible {
        let total_lines = model.textarea.lines().len().max(1);
        let gutter_w = (total_lines as f64).log10().floor() as u16 + 2;
        let cx = right[0].x + 1 + gutter_w + ccol as u16 + 1;
        let cy = right[0].y + 1 + crow as u16;
        if cx < right[0].right().saturating_sub(1) && cy < right[0].bottom().saturating_sub(1) {
            frame.set_cursor_position((cx, cy));
        }
    }

    // ── Results ────────────────────────────────────────────────────────────────
    let rs_style = if model.focus == Focus::Results { focused } else { unfocused };
    let col_hint = if !model.columns.is_empty() {
        format!("  col {}/{}", model.col_offset + 1, model.columns.len())
    } else { String::new() };
    let rs_block = Block::default()
        .title(format!(" Results{}  {} ", col_hint, model.status))
        .borders(Borders::ALL).border_style(rs_style);

    if model.columns.is_empty() {
        frame.render_widget(rs_block, right[1]);
    } else {
        let widths = col_widths(&model.columns, &model.rows, model.col_offset);
        let header = Row::new(model.columns[model.col_offset..].iter().map(|c| {
            Cell::from(c.as_str()).style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        }));
        let data: Vec<Row> = model.rows.iter().map(|r| {
            Row::new(r[model.col_offset..].iter().map(|c| Cell::from(c.as_str())))
        }).collect();
        let table = Table::new(data, widths)
            .header(header).block(rs_block)
            .row_highlight_style(Style::default().bg(Color::Rgb(30, 30, 60)));
        frame.render_stateful_widget(table, right[1], &mut model.result_state);
    }
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> std::io::Result<()> {
    use ratatui::crossterm::{execute, event::{
        EnableFocusChange, DisableFocusChange,
        KeyboardEnhancementFlags, PushKeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    }};

    let conn = LazyConnection::open("viewer.db").expect("failed to open database");
    let (cmd_tx, cmd_rx) = mpsc::channel::<DbCommand>();
    let (resp_tx, resp_rx) = mpsc::channel::<DbResponse>();
    let conn_bg = Arc::clone(&conn);
    thread::spawn(move || db_thread(conn_bg, cmd_rx, resp_tx));

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(),
        ratatui::crossterm::cursor::SetCursorStyle::BlinkingBar
    ).ok();
    let kitty = execute!(std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    ).is_ok();
    execute!(std::io::stdout(), EnableFocusChange).ok();

    let mut model = Model::new(cmd_tx, resp_rx);
    model.cmd_tx.send(DbCommand::LoadSchema).ok();

    loop {
        while let Ok(r) = model.resp_rx.try_recv() {
            if !update(&mut model, Msg::DbResp(r)) { break; }
        }
        model.tick_blink();
        terminal.draw(|f| view(&mut model, f))?;

        if event::poll(std::time::Duration::from_millis(16))? {
            match event::read()? {
                Event::FocusGained => {
                    model.terminal_focused = true;
                    model.cursor_visible   = true;
                    model.last_blink       = std::time::Instant::now();
                }
                Event::FocusLost => { model.terminal_focused = false; }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(msg) = key_to_msg(&model, key) {
                        if !update(&mut model, msg) { break; }
                    }
                }
                _ => {}
            }
        }
    }

    model.cmd_tx.send(DbCommand::Shutdown).ok();
    execute!(std::io::stdout(), DisableFocusChange).ok();
    if kitty { execute!(std::io::stdout(), PopKeyboardEnhancementFlags).ok(); }
    execute!(std::io::stdout(),
        ratatui::crossterm::cursor::SetCursorStyle::DefaultUserShape
    ).ok();
    ratatui::restore();
    Ok(())
}