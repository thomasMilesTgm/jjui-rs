#[derive(Debug)]
pub struct Input {
    pub action: String,
    pub label: String,
    pub text: String,
    pub cursor: usize,
    pub sources: Vec<String>,
    pub files: Vec<String>,
    pub confirm: bool,
}
impl Input {
    pub fn new(
        action: &str,
        label: &str,
        text: String,
        sources: Vec<String>,
        files: Vec<String>,
    ) -> Self {
        let cursor = text.len();
        Self {
            action: action.into(),
            label: label.into(),
            text,
            cursor,
            sources,
            files,
            confirm: false,
        }
    }
    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }
    pub fn edit(&mut self, action: &str) {
        match action {
            "cursor_left" => {
                self.cursor = self.text[..self.cursor]
                    .char_indices()
                    .last()
                    .map_or(0, |(i, _)| i)
            }
            "cursor_right" => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            "backspace" => {
                let end = self.cursor;
                self.edit("cursor_left");
                self.text.replace_range(self.cursor..end, "");
            }
            "delete" => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.text
                        .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
                }
            }
            "input_home" => self.cursor = 0,
            "delete_word" => {
                let prefix = self.text[..self.cursor].trim_end();
                let start = prefix
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_whitespace())
                    .map_or(0, |(i, c)| i + c.len_utf8());
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            "delete_to_start" => {
                self.text.replace_range(..self.cursor, "");
                self.cursor = 0;
            }
            "input_end" => self.cursor = self.text.len(),
            "newline" => self.insert('\n'),
            _ => {}
        }
    }
    pub fn visible(&self) -> String {
        format!(
            "{}▏{}",
            &self.text[..self.cursor],
            &self.text[self.cursor..]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_cursor_edits_stay_on_boundaries() {
        let mut input = Input::new("describe", "", "a😀z".into(), vec![], vec![]);
        input.edit("cursor_left");
        input.edit("backspace");
        assert_eq!(input.text, "az");
        input.insert('é');
        assert_eq!(input.text, "aéz");
        input.edit("input_home");
        input.edit("delete");
        assert_eq!(input.text, "éz");
        input.text = "α 😀two tail".into();
        input.cursor = "α 😀two ".len();
        input.edit("delete_word");
        assert_eq!(input.text, "α tail");
        input.edit("delete_to_start");
        assert_eq!(input.text, "tail");
        assert_eq!(input.cursor, 0);
    }
}
