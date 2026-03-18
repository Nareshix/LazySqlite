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
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState},
};
use ratatui_textarea::{CursorMove, Input, Key, TextArea};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

mod sqlite;
use sqlite::{DbCommand, DbResponse, TableSchema, db_thread, is_query};

#[derive(PartialEq)]
enum Focus { Sidebar, Editor, Results }

struct Model {
    focus:    Focus,
    textarea: TextArea<'static>,  // input handling only — not rendered directly
    clipboard: Clipboard,

    // Editor scroll (we manage this ourselves so syntect overlay stays in sync)
    editor_scroll: usize,

    // Syntect — loaded once at startup
    syntax_set: SyntaxSet,
    theme_set:  ThemeSet,

    cmd_tx:  mpsc::Sender<DbCommand>,
    resp_rx: mpsc::Receiver<DbResponse>,

    columns:      Vec<String>,
    rows:         Vec<Vec<String>>,
    status:       String,
    loading:      bool,
    result_state: TableState,
    col_offset:   usize,

    schema:        Vec<TableSchema>,
    sidebar_state: ListState,
    sidebar_items: Vec<String>,

    // Stored rects for mouse hit-testing
    sidebar_rect: (u16, u16, u16, u16),
    editor_rect:  (u16, u16, u16, u16),
    results_rect: (u16, u16, u16, u16),

    cursor_visible:   bool,
    last_blink:       std::time::Instant,
    terminal_focused: bool,
}

impl Model {
    fn new(cmd_tx: mpsc::Sender<DbCommand>, resp_rx: mpsc::Receiver<DbResponse>) -> Self {
        let mut textarea = TextArea::default();
        // Invisible — we render our own highlighted view on top
        textarea.set_cursor_style(Style::default());
        textarea.set_cursor_line_style(Style::default());

        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set  = ThemeSet::load_defaults();

        Self {
            focus: Focus::Editor,
            textarea,
            clipboard: Clipboard::new().unwrap(),
            editor_scroll: 0,
            syntax_set,
            theme_set,
            cmd_tx, resp_rx,
            columns: vec![], rows: vec![],
            status: "Ready".into(), loading: false,
            result_state: TableState::default(), col_offset: 0,
            schema: vec![], sidebar_state: ListState::default(), sidebar_items: vec![],
            sidebar_rect: (0,0,0,0), editor_rect: (0,0,0,0), results_rect: (0,0,0,0),
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
                if let Some(fk) = &col.fk_to { tag.push_str(&format!(" FK→{}", fk)); }
                let typ = if col.typ.is_empty() { "?".into() } else { col.typ.clone() };
                self.sidebar_items.push(format!("   {} {}{}", col.name, typ.to_uppercase(), tag));
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

fn col_widths(columns: &[String], rows: &[Vec<String>], offset: usize) -> Vec<Constraint> {
    columns[offset..].iter().enumerate().map(|(i, h)| {
        let ai = i + offset;
        let max = rows.iter().filter_map(|r| r.get(ai)).map(|s| s.len()).max().unwrap_or(0);
        let w = (h.len().max(max).max(4) as u16).min(40);
        Constraint::Min(w)
    }).collect()
}

// Convert syntect color to ratatui Color
fn syn_color(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

// Highlight SQL text with syntect, returning ratatui Lines.
// Falls back to plain text on any error so the editor never crashes.
fn highlight_sql(
    code: &str,
    syntax_set: &SyntaxSet,
    theme_set: &ThemeSet,
) -> Vec<Line<'static>> {
    let syntax = syntax_set
        .find_syntax_by_name("SQL")
        .or_else(|| syntax_set.find_syntax_by_extension("sql"))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());

    let theme = &theme_set.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);
    let mut out = vec![];

    for line in LinesWithEndings::from(code) {
        let spans: Vec<Span<'static>> = match h.highlight_line(line, syntax_set) {
            Ok(ranges) => ranges
                .into_iter()
                .map(|(style, text)| {
                    let fg = syn_color(style.foreground);
                    Span::styled(
                        text.trim_end_matches('\n').to_string(),
                        Style::default().fg(fg),
                    )
                })
                .collect(),
            Err(_) => vec![Span::raw(line.trim_end_matches('\n').to_string())],
        };
        out.push(Line::from(spans));
    }
    out
}

enum Msg {
    FocusNext, FocusPrev,
    Submit,
    Up, Down, Left, Right,
    Copy, Cut, Paste,
    Undo, Redo, SelectAll,
    WordForward, WordBack, WordSelectForward, WordSelectBack,
    Editor(Input),
    MouseClick(u16, u16), MouseScrollUp, MouseScrollDown,
    DbResp(DbResponse),
    Quit,
}

fn key_to_msg(model: &Model, key: event::KeyEvent) -> Option<Msg> {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    let ctrl  = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    if ctrl {
        match key.code {
            KeyCode::Tab     => return Some(Msg::FocusNext),
            KeyCode::BackTab => return Some(Msg::FocusPrev),
            KeyCode::Right if shift => return Some(Msg::WordSelectForward),
            KeyCode::Left  if shift => return Some(Msg::WordSelectBack),
            KeyCode::Right          => return Some(Msg::WordForward),
            KeyCode::Left           => return Some(Msg::WordBack),
            _ => {}
        }
    }

    let input = Input::from(key);

    match model.focus {
        Focus::Editor => match input {
            Input { key: Key::Enter, ctrl: true, .. } => Some(Msg::Submit),
            Input { key: Key::Char('a'), ctrl: true, .. } => Some(Msg::SelectAll),
            Input { key: Key::Char('z'), ctrl: true, shift: false, .. } => Some(Msg::Undo),
            Input { key: Key::Char('z'), ctrl: true, shift: true, .. } |
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
                if let Some(sel) = model.sidebar_state.selected() {
                    let line = model.sidebar_items.get(sel).cloned().unwrap_or_default();
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
                    let i = model.result_state.selected().map(|i| i.saturating_sub(1)).unwrap_or(0);
                    model.result_state.select(Some(i));
                }
            }
            Focus::Sidebar => {
                let i = model.sidebar_state.selected().map(|i| i.saturating_sub(1)).unwrap_or(0);
                model.sidebar_state.select(Some(i));
            }
            _ => {}
        },
        Msg::Down => match model.focus {
            Focus::Results => {
                if !model.rows.is_empty() {
                    let max = model.rows.len() - 1;
                    let i = model.result_state.selected().map(|i| (i+1).min(max)).unwrap_or(0);
                    model.result_state.select(Some(i));
                }
            }
            Focus::Sidebar => {
                let max = model.sidebar_items.len().saturating_sub(1);
                let i = model.sidebar_state.selected().map(|i| (i+1).min(max)).unwrap_or(0);
                model.sidebar_state.select(Some(i));
            }
            _ => {}
        },
        Msg::Left  => { model.col_offset = model.col_offset.saturating_sub(1); }
        Msg::Right => { if model.col_offset + 1 < model.columns.len() { model.col_offset += 1; } }

        Msg::WordForward => {
            model.textarea.cancel_selection();
            model.textarea.move_cursor(CursorMove::WordForward);
        }
        Msg::WordBack => {
            model.textarea.cancel_selection();
            model.textarea.move_cursor(CursorMove::WordBack);
        }
        Msg::WordSelectForward => {
            model.textarea.start_selection();
            model.textarea.move_cursor(CursorMove::WordForward);
        }
        Msg::WordSelectBack => {
            model.textarea.start_selection();
            model.textarea.move_cursor(CursorMove::WordBack);
        }

        Msg::Copy => {
            model.textarea.copy();
            let _ = model.clipboard.set_text(model.textarea.yank_text());
        }
        Msg::Cut => {
            model.textarea.cut();
            let _ = model.clipboard.set_text(model.textarea.yank_text());
        }
        Msg::Paste => {
            if let Ok(text) = model.clipboard.get_text() {
                model.textarea.set_yank_text(text);
                model.textarea.paste();
            }
        }
        Msg::Undo      => { model.textarea.undo(); }
        Msg::Redo      => { model.textarea.redo(); }
        Msg::SelectAll => model.textarea.select_all(),
        Msg::Editor(i) => { model.textarea.input(i); }

        Msg::MouseClick(x, y) => {
            let (sx, sy, sw, sh) = model.sidebar_rect;
            let (ex, ey, ew, eh) = model.editor_rect;
            let (rx, ry, rw, rh) = model.results_rect;
            if x >= sx && x < sx+sw && y >= sy && y < sy+sh {
                model.focus = Focus::Sidebar;
            } else if x >= ex && x < ex+ew && y >= ey && y < ey+eh {
                model.focus = Focus::Editor;
            } else if x >= rx && x < rx+rw && y >= ry && y < ry+rh {
                model.focus = Focus::Results;
            }
        }
        Msg::MouseScrollUp => match model.focus {
            Focus::Editor  => { model.editor_scroll = model.editor_scroll.saturating_sub(3); }
            Focus::Results => {
                let i = model.result_state.selected().unwrap_or(0).saturating_sub(3);
                model.result_state.select(Some(i));
            }
            Focus::Sidebar => {
                let i = model.sidebar_state.selected().unwrap_or(0).saturating_sub(3);
                model.sidebar_state.select(Some(i));
            }
        },
        Msg::MouseScrollDown => match model.focus {
            Focus::Editor => {
                let max = model.textarea.lines().len().saturating_sub(1);
                model.editor_scroll = (model.editor_scroll + 3).min(max);
            }
            Focus::Results => {
                if !model.rows.is_empty() {
                    let max = model.rows.len() - 1;
                    let i = (model.result_state.selected().unwrap_or(0) + 3).min(max);
                    model.result_state.select(Some(i));
                }
            }
            Focus::Sidebar => {
                let max = model.sidebar_items.len().saturating_sub(1);
                let i = (model.sidebar_state.selected().unwrap_or(0) + 3).min(max);
                model.sidebar_state.select(Some(i));
            }
        },

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

    model.sidebar_rect = (chunks[0].x, chunks[0].y, chunks[0].width, chunks[0].height);
    model.editor_rect  = (right[0].x,  right[0].y,  right[0].width,  right[0].height);
    model.results_rect = (right[1].x,  right[1].y,  right[1].width,  right[1].height);

    let focused   = Style::default().fg(Color::Blue);
    let unfocused = Style::default().fg(Color::DarkGray);

    // ── Sidebar ───────────────────────────────────────────────────────────────
    let sb_style = if model.focus == Focus::Sidebar { focused } else { unfocused };
    let sb_block = Block::default()
        .title(" Schema (Ctrl+Tab) ")
        .borders(Borders::ALL)
        .border_style(sb_style);

    let items: Vec<ListItem> = model.sidebar_items.iter().map(|line| {
        if line.starts_with("   ") {
            let parts: Vec<&str> = line.trim().splitn(2, ' ').collect();
            let (col_name, rest) = if parts.len() == 2 { (parts[0], parts[1]) } else { (line.trim(), "") };
            ListItem::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(col_name, Style::default().fg(Color::White)),
                Span::raw(" "),
                Span::styled(rest, Style::default().fg(Color::Rgb(120, 120, 160))),
            ]))
        } else {
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
    let ed_area  = right[0];
    let ed_style = if model.focus == Focus::Editor { focused } else { unfocused };
    let (crow, ccol) = model.textarea.cursor();
    let total_lines = model.textarea.lines().len().max(1);

    // Auto-scroll to keep cursor visible
    let inner_h = ed_area.height.saturating_sub(2) as usize; // minus borders
    if crow < model.editor_scroll {
        model.editor_scroll = crow;
    } else if crow >= model.editor_scroll + inner_h {
        model.editor_scroll = crow + 1 - inner_h;
    }

    let ed_title = format!(" SQL  Ctrl+Enter  Ln {} Col {} ", crow + 1, ccol + 1);
    let ed_block = Block::default()
        .title(ed_title)
        .borders(Borders::ALL)
        .border_style(ed_style);

    frame.render_widget(ed_block, ed_area);

    // Line number gutter — render manually so we know the exact width
    // Gutter = "<n> " where n is the widest line number
    let gutter_w = total_lines.to_string().len() as u16 + 1; // digits + 1 space
    let gutter_rect = ratatui::layout::Rect {
        x: ed_area.x + 1,
        y: ed_area.y + 1,
        width: gutter_w,
        height: ed_area.height.saturating_sub(2),
    };
    let line_nums: Vec<Line> = (model.editor_scroll..model.editor_scroll + inner_h + 1)
        .take(total_lines)
        .map(|n| Line::from(Span::styled(
            format!("{:<width$} ", n + 1, width = gutter_w as usize - 1),
            Style::default().fg(Color::Rgb(80, 80, 100)),
        )))
        .collect();
    frame.render_widget(Paragraph::new(line_nums), gutter_rect);

    // Syntect-highlighted text — render in the inner text area
    let text_rect = ratatui::layout::Rect {
        x: ed_area.x + 1 + gutter_w,
        y: ed_area.y + 1,
        width: ed_area.width.saturating_sub(2 + gutter_w),
        height: ed_area.height.saturating_sub(2),
    };

    let full_text = model.textarea.lines().join("\n");
    let all_lines = highlight_sql(&full_text, &model.syntax_set, &model.theme_set);
    let visible: Vec<Line> = all_lines
        .into_iter()
        .skip(model.editor_scroll)
        .take(inner_h + 1)
        .collect();

    frame.render_widget(
        Paragraph::new(visible).scroll((0, 0)),
        text_rect,
    );

    // Cursor — bar style via set_cursor_position, only when focused and visible
    if model.focus == Focus::Editor && model.cursor_visible {
        let cx = text_rect.x + ccol as u16;
        let cy = text_rect.y + (crow - model.editor_scroll) as u16;
        if cx < text_rect.right() && cy < text_rect.bottom() {
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
        .borders(Borders::ALL)
        .border_style(rs_style);

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

fn main() -> std::io::Result<()> {
    use ratatui::crossterm::{execute, event::{
        EnableFocusChange, DisableFocusChange, EnableMouseCapture, DisableMouseCapture,
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
    execute!(std::io::stdout(), EnableMouseCapture).ok();

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
                Event::Mouse(me) => {
                    use ratatui::crossterm::event::{MouseEventKind, MouseButton};
                    match me.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            update(&mut model, Msg::MouseClick(me.column, me.row));
                        }
                        MouseEventKind::ScrollUp   => { update(&mut model, Msg::MouseScrollUp); }
                        MouseEventKind::ScrollDown => { update(&mut model, Msg::MouseScrollDown); }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    model.cmd_tx.send(DbCommand::Shutdown).ok();
    execute!(std::io::stdout(), DisableMouseCapture).ok();
    execute!(std::io::stdout(), DisableFocusChange).ok();
    if kitty { execute!(std::io::stdout(), PopKeyboardEnhancementFlags).ok(); }
    execute!(std::io::stdout(),
        ratatui::crossterm::cursor::SetCursorStyle::DefaultUserShape
    ).ok();
    ratatui::restore();
    Ok(())
}