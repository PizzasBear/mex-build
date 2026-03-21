use std::str::Chars;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteMode {
    Unquoted,
    SingleQuoted,
    DoubleQuoted,
}

pub struct SplitList<'a> {
    chars: Chars<'a>,
    error: bool,
}

impl<'a> Iterator for SplitList<'a> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        let mut result = String::new();

        let mut ws = true;
        let mut mode = QuoteMode::Unquoted;

        while let Some(ch) = self.chars.next() {
            let prev_ws = ws;
            ws = false;
            match (mode, ch) {
                (QuoteMode::Unquoted, ' ' | '\t' | '\n') => {
                    ws = true;
                    if !prev_ws {
                        break;
                    }
                }
                (QuoteMode::Unquoted, '\\') => match self.chars.next() {
                    Some('\n') => {}
                    Some(ch) => {
                        result.push(ch);
                    }
                    None => {
                        result.push('\\');
                        break;
                    }
                },
                (QuoteMode::Unquoted, '\'') => {
                    mode = QuoteMode::SingleQuoted;
                }
                (QuoteMode::Unquoted, '"') => {
                    mode = QuoteMode::DoubleQuoted;
                }
                (QuoteMode::Unquoted, ch) => {
                    result.push(ch);
                }
                (QuoteMode::SingleQuoted, '\'') => mode = QuoteMode::Unquoted,
                (QuoteMode::SingleQuoted, ch) => result.push(ch),
                (QuoteMode::DoubleQuoted, '\\') => match self.chars.next() {
                    Some('\n') => {}
                    Some('$' | '`' | '"' | '\\') => result.push(ch),
                    Some(ch) => {
                        result.push('\\');
                        result.push(ch);
                    }
                    None => result.push('\\'),
                },
                (QuoteMode::DoubleQuoted, '"') => mode = QuoteMode::Unquoted,
                (QuoteMode::DoubleQuoted, ch) => result.push(ch),
            }
        }

        self.error = mode != QuoteMode::Unquoted;

        ws.then_some(result)
    }
}
