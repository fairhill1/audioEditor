/// A reusable popup modal that collects a single numeric input from the user.
/// Renders as a centered box with a title, text field, and Enter to confirm / Escape to cancel.

pub struct Modal {
    pub title: String,
    pub input: String,
    pub visible: bool,
    /// Called with the parsed value when the user confirms
    pub kind: ModalKind,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ModalKind {
    ClickTrackBpm,
}

#[derive(Clone, Copy)]
pub enum ModalResult {
    ClickTrackBpm(f64),
}

impl Modal {
    pub fn new(title: &str, kind: ModalKind) -> Self {
        Self {
            title: title.to_string(),
            input: String::new(),
            visible: true,
            kind,
        }
    }

    /// Handle a character input. Returns None if modal is still open,
    /// Some(result) if confirmed, or the modal sets visible=false on cancel.
    pub fn handle_char(&mut self, c: char) -> Option<ModalResult> {
        match c {
            '\r' | '\n' => {
                // Try to parse and confirm
                if let Ok(val) = self.input.parse::<f64>() {
                    if val > 0.0 {
                        self.visible = false;
                        return Some(self.make_result(val));
                    }
                }
                None
            }
            _ if c.is_ascii_digit() || c == '.' => {
                self.input.push(c);
                None
            }
            _ => None,
        }
    }

    pub fn handle_backspace(&mut self) {
        self.input.pop();
    }

    pub fn cancel(&mut self) {
        self.visible = false;
    }

    fn make_result(&self, val: f64) -> ModalResult {
        match self.kind {
            ModalKind::ClickTrackBpm => ModalResult::ClickTrackBpm(val),
        }
    }
}
