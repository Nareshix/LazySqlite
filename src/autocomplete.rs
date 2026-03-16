use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
};
use ratatui_textarea::TextArea;

// ── Autocomplete state ──────────────────────────────────────────────────────

pub struct Autocomplete {
    pub words:   Vec<String>,
    pub matches: Vec<String>,
    pub state:   ListState,
    pub visible: bool,
    pub matcher: Matcher,
}

impl Autocomplete {
    pub fn new(words: Vec<String>) -> Self {
        Self {
            words,
            matches: vec![],
            state:   ListState::default(),
            visible: false,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn update(&mut self, query: &str) {
        if query.is_empty() {
            self.dismiss();
            return;
        }

        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut buf = Vec::new();

        let mut scored: Vec<(u32, String)> = self
            .words
            .iter()
            .filter_map(|word| {
                let score = pattern.score(Utf32Str::new(word, &mut buf), &mut self.matcher)?;
                Some((score, word.clone()))
            })
            .collect();

        scored.sort_by(|a, b| b.0.cmp(&a.0));
        self.matches = scored.into_iter().map(|(_, w)| w).collect();

        if self.matches.is_empty() {
            self.dismiss();
        } else {
            self.visible = true;
            self.state.select(Some(0)); // always reset to top on new query
        }
    }

    pub fn next(&mut self) {
        if let Some(i) = self.state.selected() {
            self.state.select(Some((i + 1).min(self.matches.len().saturating_sub(1))));
        }
    }

    pub fn prev(&mut self) {
        if let Some(i) = self.state.selected() {
            self.state.select(Some(i.saturating_sub(1)));
        }
    }

    pub fn selected(&self) -> Option<&str> {
        self.state.selected()
            .and_then(|i| self.matches.get(i).map(String::as_str))
    }

    pub fn dismiss(&mut self) {
        self.visible = false;
        self.matches.clear();
        self.state.select(None);
    }
}

// ── Helper: get the word currently being typed ──────────────────────────────

pub fn current_word(textarea: &TextArea) -> String {
    let (row, col) = textarea.cursor();
    let line = &textarea.lines()[row];
    let chars: Vec<char> = line.chars().collect();
    let mut start = col;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
        start -= 1;
    }
    chars[start..col].iter().collect()
}

// ── Helper: popup Rect positioned near the cursor ───────────────────────────

pub fn popup_rect(textarea_area: Rect, row: usize, col: usize) -> Rect {
    const W: u16 = 30;
    const H: u16 = 8;

    // +1 for top border of textarea
    let mut x = textarea_area.x + 1 + col as u16;
    let mut y = textarea_area.y + 1 + row as u16 + 1; // appear below cursor line

    // clamp so popup doesn't go off screen
    if x + W > textarea_area.right() {
        x = textarea_area.right().saturating_sub(W);
    }
    if y + H > textarea_area.bottom() {
        // show above cursor instead
        y = (textarea_area.y + 1 + row as u16).saturating_sub(H);
    }

    Rect { x, y, width: W, height: H }
}
