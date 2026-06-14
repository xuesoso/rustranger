// The `:` / `/` command-line editor state. Ported in spirit from
// ranger/gui/widgets/console.py, reduced to a single-line editor.

pub struct ConsoleState {
    pub prompt: char,
    pub input: String,
    /// Cursor position as a character index into `input`.
    pub cursor: usize,
}

impl ConsoleState {
    pub fn new(prompt: char, initial: &str) -> ConsoleState {
        ConsoleState {
            prompt,
            input: initial.to_string(),
            cursor: initial.chars().count(),
        }
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    pub fn insert(&mut self, c: char) {
        let b = self.byte_at(self.cursor);
        self.input.insert(b, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let start = self.byte_at(self.cursor - 1);
            let end = self.byte_at(self.cursor);
            self.input.replace_range(start..end, "");
            self.cursor -= 1;
        }
    }

    pub fn delete(&mut self) {
        let len = self.input.chars().count();
        if self.cursor < len {
            let start = self.byte_at(self.cursor);
            let end = self.byte_at(self.cursor + 1);
            self.input.replace_range(start..end, "");
        }
    }

    pub fn left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn right(&mut self) {
        if self.cursor < self.input.chars().count() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.input.chars().count();
    }

    /// Clear from cursor to the beginning of the line (Ctrl-U).
    pub fn clear_to_start(&mut self) {
        let end = self.byte_at(self.cursor);
        self.input.replace_range(0..end, "");
        self.cursor = 0;
    }
}
