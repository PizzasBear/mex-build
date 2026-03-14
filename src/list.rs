use std::str::Chars;

pub struct SplitList<'a> {
    chars: Chars<'a>,
    error: bool,
}

impl<'a> Iterator for SplitList<'a> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        #[derive(Debug, Clone, Copy)]
        enum Mode {
            Unquoted { skip_ws: bool },
            SingleQuoted,
            DoubleQuoted,
        }

        let mut result = String::new();

        let mut mode = Mode::Unquoted { skip_ws: true };
        while let Some(ch) = self.chars.next() {
            match (mode, ch) {
                (Mode::Unquoted { skip_ws }, ' ' | '\t' | '\n') => {
                    if !skip_ws {
                        break;
                    }
                }
                (Mode::Unquoted { ref mut skip_ws }, '\\') => match self.chars.next() {
                    Some('\n') => {}
                    Some(ch) => {
                        *skip_ws = false;
                        result.push(ch);
                    }
                    None => {
                        *skip_ws = false;
                        result.push('\\');
                        break;
                    }
                },
                (Mode::Unquoted { .. }, '\'') => mode = Mode::SingleQuoted,
                (Mode::Unquoted { .. }, '"') => mode = Mode::DoubleQuoted,
                (Mode::Unquoted { ref mut skip_ws }, ch) => {
                    *skip_ws = false;
                    result.push(ch);
                }
                (Mode::SingleQuoted, '\'') => mode = Mode::Unquoted { skip_ws: false },
                (Mode::SingleQuoted, ch) => result.push(ch),
                (Mode::DoubleQuoted, '\\') => match self.chars.next() {
                    Some('\n') => {}
                    Some('$' | '`' | '"' | '\\') => result.push(ch),
                    Some(ch) => {
                        result.push('\\');
                        result.push(ch);
                    }
                    None => result.push('\\'),
                },
                (Mode::DoubleQuoted, '"') => mode = Mode::Unquoted { skip_ws: false },
                (Mode::DoubleQuoted, ch) => result.push(ch),
            }
        }

        if matches!(mode, Mode::Unquoted { skip_ws: true }) {
            return None;
        }

        self.error = !matches!(mode, Mode::Unquoted { skip_ws: false });

        Some(result)
    }
}
