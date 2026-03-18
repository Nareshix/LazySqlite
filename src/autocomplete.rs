use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use ratatui::{
    layout::Rect,
    widgets::ListState,
};
use ratatui_textarea::TextArea;

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

    /// Add extra words (e.g. table names fetched from the DB) without duplicates.
    pub fn add_words(&mut self, extra: Vec<String>) {
        for w in extra {
            if !self.words.contains(&w) {
                self.words.push(w);
            }
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
            self.state.select(Some(0));
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

// Returns the word currently being typed at the cursor position.
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

// Returns a Rect for the autocomplete popup near the cursor.
//
// textarea_area : the editor pane rect  (cursor-relative positioning)
// frame_area    : the full terminal rect (hard-clamp so we NEVER exceed
//                 buffer bounds, which would cause a ratatui panic on resize)
pub fn popup_rect(textarea_area: Rect, frame_area: Rect, row: usize, col: usize) -> Rect {
    const W: u16 = 30;
    const H: u16 = 8;
    const GAP: u16 = 1; // rows of breathing room between cursor and popup top

    // Terminal too tiny to show anything useful.
    if frame_area.width == 0 || frame_area.height == 0 {
        return Rect::default();
    }

    let cursor_screen_y = textarea_area.y.saturating_add(1).saturating_add(row as u16);
    let mut x = textarea_area.x.saturating_add(1).saturating_add(col as u16);
    let mut y = cursor_screen_y.saturating_add(1).saturating_add(GAP);

    // Prefer showing below; flip above when there is not enough room in
    // the FULL terminal (not just the editor pane — that was the old bug).
    if y.saturating_add(H) > frame_area.bottom() {
        y = cursor_screen_y.saturating_sub(H.saturating_add(GAP));
    }

    // Hard-clamp: popup must ALWAYS be fully inside the terminal buffer.
    // Without this, shrinking the window triggers an out-of-bounds panic.
    let width  = W.min(frame_area.width);
    let height = H.min(frame_area.height);
    x = x.min(frame_area.right().saturating_sub(width));
    y = y.min(frame_area.bottom().saturating_sub(height));

    Rect { x, y, width, height }
}