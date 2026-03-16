// main.rs — Elm Architecture in a single file
//
//  ┌─────────┐    Msg     ┌────────┐    &Model   ┌──────┐
//  │  Input  │ ─────────► │ update │             │ view │
//  └─────────┘            └────────┘ ──────────► └──────┘
//                          mutates                renders
//                          Model

use arboard::Clipboard;
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem},
};
use ratatui_textarea::{Input, Key, TextArea};

mod autocomplete;
use autocomplete::{Autocomplete, current_word, popup_rect};

// ============================================================================
// MODEL — plain data, zero logic
// ============================================================================

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

struct Model {
    focus: Focus,
    textarea: TextArea<'static>,
    clipboard: Clipboard,
    ac: Autocomplete,
    copied_at: Option<std::time::Instant>,
}

impl Model {
    fn new() -> Self {
        Self {
            focus: Focus::Box1,
            textarea: TextArea::default(),
            clipboard: Clipboard::new().unwrap(),
            ac: Autocomplete::new(vec![
                "SELECT".into(), "FROM".into(),   "WHERE".into(),
                "INSERT".into(),  "UPDATE".into(), "DELETE".into(),
                "CREATE".into(),  "DROP".into(),   "ALTER".into(),
                "INDEX".into(),
            ]),
            copied_at: None,
        }
    }
}

// ============================================================================
// MSG — every possible event, named by what happened (not what to do)
// ============================================================================

enum Msg {
    NextFocus,
    Quit,
    // Autocomplete
    AcNext,
    AcPrev,
    AcDismiss,
    AcAccept,
    // Editor shortcuts
    SelectAll,
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    // Raw input
    Navigate(Input),
    TypeChar(Input),
}

// ============================================================================
// SUBSCRIPTIONS — translate raw key events → Msg
// (Elm calls this `subscriptions`; here it's just a free function)
// ============================================================================

fn key_to_msg(model: &Model, key: event::KeyEvent) -> Option<Msg> {
    let input = Input::from(key);

    // ── Global keys (work in any pane) ───────────────────────────────────────
    if key.code == KeyCode::Tab {
        return Some(Msg::NextFocus);
    }
    if matches!(key.code, KeyCode::Char('q')) && model.focus != Focus::Box2 {
        return Some(Msg::Quit);
    }

    // ── Box2 only below this point ───────────────────────────────────────────
    if model.focus != Focus::Box2 {
        return None;
    }

    // Autocomplete overlay steals Up/Down/Esc/Tab/Enter
    if model.ac.visible {
        return match input {
            Input { key: Key::Down,              .. } => Some(Msg::AcNext),
            Input { key: Key::Up,                .. } => Some(Msg::AcPrev),
            Input { key: Key::Esc,               .. } => Some(Msg::AcDismiss),
            Input { key: Key::Tab | Key::Enter,  .. } => Some(Msg::AcAccept),
        other => Some(Msg::TypeChar(other)),  // ← typing still works
        };
    }

    // Editor shortcuts
    match input {
        Input { key: Key::Char('a'), ctrl: true,                    .. } => Some(Msg::SelectAll),
        Input { key: Key::Char('z'), ctrl: true, shift: false,      .. } => Some(Msg::Undo),
        Input { key: Key::Char('z'), ctrl: true, shift: true,       .. }
        | Input { key: Key::Char('y'), ctrl: true,                  .. } => Some(Msg::Redo),
        Input { key: Key::Char('c'), ctrl: true,                    .. } => Some(Msg::Copy),
        Input { key: Key::Char('x'), ctrl: true,                    .. } => Some(Msg::Cut),
        Input { key: Key::Char('v'), ctrl: true,                    .. } => Some(Msg::Paste),

        // Navigation (use full `input()` so ctrl+arrow, home/end all work)
        Input { key: Key::Up | Key::Down | Key::Left | Key::Right,  .. }
        | Input { key: Key::Home | Key::End,                        .. }
        | Input { key: Key::PageUp | Key::PageDown,                 .. } => Some(Msg::Navigate(input)),

        // Everything else is a typed character
        other => Some(Msg::TypeChar(other)),
    }
}

// ============================================================================
// UPDATE — the only place Model is mutated
//          returns None to signal "quit", Some(()) to keep running
// ============================================================================

fn update(model: &mut Model, msg: Msg) -> Option<()> {
    match msg {
        // ── Global ────────────────────────────────────────────────────────────
        Msg::Quit      => return None,
        Msg::NextFocus => model.focus = model.focus.next(),

        // ── Autocomplete ──────────────────────────────────────────────────────
        Msg::AcNext    => model.ac.next(),
        Msg::AcPrev    => model.ac.prev(),
        Msg::AcDismiss => model.ac.dismiss(),
        Msg::AcAccept  => {
            if let Some(word) = model.ac.selected() {
                let word = word.to_string();
                let partial = current_word(&model.textarea);
                for _ in 0..partial.chars().count() {
                    model.textarea.delete_char();
                }
                model.textarea.insert_str(&word);
                model.ac.dismiss();
            }
        }

        // ── Editor shortcuts ──────────────────────────────────────────────────
        Msg::SelectAll => model.textarea.select_all(),
        Msg::Undo      => { model.textarea.undo(); }
        Msg::Redo      => { model.textarea.redo(); }

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
            if !yanked.is_empty() {
                model.clipboard.set_text(yanked).ok();
            }
        }
        Msg::Paste => {
            if let Ok(text) = model.clipboard.get_text() {
                model.textarea.set_yank_text(text);
                model.textarea.paste();
            }
        }

        // ── Raw input ─────────────────────────────────────────────────────────
        Msg::Navigate(input) => {
            model.textarea.input(input);
        }
        Msg::TypeChar(input) => {
            model.textarea.input_without_shortcuts(input);
            let word = current_word(&model.textarea);
            model.ac.update(&word);
        }
    }

    Some(())
}

// ============================================================================
// VIEW — read-only; describes what to draw, never touches Model
// ============================================================================

fn view(model: &mut Model, frame: &mut Frame) {
    let area = frame.area();

    // ── Layout ────────────────────────────────────────────────────────────────
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(cols[1]);

    // ── Helpers ───────────────────────────────────────────────────────────────
    let focused = Style::default().fg(Color::Blue);
    let normal  = Style::default().fg(Color::Gray);
    let border  = |pane: Focus| if model.focus == pane { focused } else { normal };

    // ── Box 1 — left pane ─────────────────────────────────────────────────────
    frame.render_widget(
        Block::default().borders(Borders::ALL).border_style(border(Focus::Box1)),
        cols[0],
    );

    // ── Box 2 — textarea (top-right) ──────────────────────────────────────────
    let title = match model.copied_at {
        Some(t) if t.elapsed().as_secs() < 1 => " Copied! ✓ ",
        _ => " Box 2 ",
    };
    model.textarea.set_block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border(Focus::Box2)),
    );
    frame.render_widget(&model.textarea, rows[0]);

    // ── Box 3 — bottom-right pane ─────────────────────────────────────────────
    frame.render_widget(
        Block::default().borders(Borders::ALL).border_style(border(Focus::Box3)),
        rows[1],
    );

    // ── Autocomplete popup (drawn last so it floats above everything) ─────────
    if model.ac.visible {
        let (row, col) = model.textarea.cursor();
        let popup = popup_rect(rows[0], row, col);

        let items: Vec<ListItem> = model.ac.matches
            .iter()
            .map(|m| ListItem::new(m.as_str()))
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Autocomplete "))
            .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");

        frame.render_widget(ratatui::widgets::Clear, popup);
        frame.render_stateful_widget(list, popup, &mut model.ac.state);
    }
}

// ============================================================================
// MAIN — the runtime loop: draw → read event → msg → update → repeat
// ============================================================================

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let mut model = Model::new();

    loop {
        // 1. DRAW: hand model to view, view describes the frame
        terminal.draw(|frame| view(&mut model, frame))?;

        // 2. READ: block until the next key press
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            // 3. TRANSLATE: raw event → typed Msg (or ignore)
            if let Some(msg) = key_to_msg(&model, key) {
                // 4. UPDATE: msg + old model → new model (or quit)
                if update(&mut model, msg).is_none() {
                    break;
                }
            }
        }
    }

    ratatui::restore();
    Ok(())
}

