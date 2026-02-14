use std::{fmt, ops, path::PathBuf, str};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Pos {
    offset: u32,
}

impl fmt::Debug for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pos({})", self.offset)
    }
}

impl Pos {
    fn advance_by_char(&mut self, ch: char) {
        self.offset += ch.len_utf8() as u32;
    }
    fn advance_by_str(&mut self, s: &str) {
        self.offset += s.len() as u32;
    }

    #[must_use]
    pub fn to_span(self) -> Span {
        self.to(self)
    }
    #[must_use]
    pub fn to(self, end: Self) -> Span {
        Span { start: self, end }
    }
    #[must_use]
    pub fn up_to(self, code: &CodeIter) -> Span {
        self.to(code.pos())
    }
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset as _
    }

    #[must_use]
    pub fn line_col(&self, source: &Source) -> (u32, u32) {
        let code_before = &source.code[..self.offset()];
        let line_start = code_before.rfind('\n').map_or(0, |i| i + 1);

        let line = 1 + code_before[..line_start]
            .bytes()
            .map(|b| (b == b'\n') as u32)
            .sum::<u32>();
        let col = (self.offset() - line_start + 1) as _;
        (line, col)
    }

    #[must_use]
    pub fn display<'a>(&self, source: &'a Source) -> PosDisplay<'a> {
        let (line, col) = self.line_col(source);

        PosDisplay {
            source,
            pos: *self,
            line,
            col,
        }
    }
}

#[derive(Clone, Copy)]
#[repr(align(8))]
pub struct Span {
    start: Pos,
    end: Pos,
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let &Self { start, end } = self;

        write!(f, "Span({:?})", start.offset()..end.offset())
    }
}

impl Span {
    #[must_use]
    pub fn start(&self) -> Pos {
        self.start
    }
    #[must_use]
    pub fn end(&self) -> Pos {
        self.end
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end() <= self.start()
    }
    #[must_use]
    pub fn len(&self) -> usize {
        self.end().offset() - self.start().offset()
    }
    #[must_use]
    pub fn to_range(&self) -> ops::Range<usize> {
        self.start().offset()..self.end().offset()
    }
    #[must_use]
    pub fn display<'a>(&self, source: &'a Source) -> SpanDisplay<'a> {
        let (line, col) = self.start().line_col(source);
        SpanDisplay {
            source,
            span: *self,
            line,
            col,
        }
    }
}

#[must_use]
#[derive(Clone)]
pub struct SpanDisplay<'a> {
    source: &'a Source,
    span: Span,
    line: u32,
    col: u32,
}

impl fmt::Display for SpanDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write;

        let line_start = (self.span.start.offset + 1 - self.col) as usize;
        writeln!(
            f,
            "At {}:{}:{}",
            self.source.path.display(),
            self.line,
            self.col
        )?;
        let mut index = line_start;

        let end = self.span.end();
        for line in self.source.code[line_start..].split('\n') {
            f.write_str("> ")?;
            f.write_str(line.trim_ascii_end())?;
            f.write_char('\n')?;

            index += line.len() + 1;
            if end.offset() <= index {
                break;
            }
        }

        Ok(())
    }
}

pub trait Spanned {
    #[must_use]
    fn span(&self) -> Span;
}

#[derive(Clone)]
pub struct PosDisplay<'a> {
    source: &'a Source,
    pos: Pos,
    line: u32,
    col: u32,
}

impl fmt::Display for PosDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write;

        let line_start = (self.pos.offset + 1 - self.col) as usize;
        write!(
            f,
            "At {}:{}:{}\n> ",
            self.source.path.display(),
            self.line,
            self.col
        )?;
        f.write_str(self.source.code[line_start..].lines().next().unwrap_or(""))?;
        f.write_char('\n')?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct Source {
    pub path: PathBuf,
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct CodeIter<'a> {
    _source: &'a Source,
    pos: Pos,
    chars: str::Chars<'a>,
}

impl<'a> CodeIter<'a> {
    #[must_use]
    pub fn new(source: &'a Source) -> Self {
        // TODO: handle in `Source` constructor
        assert!(source.code.len() <= u32::MAX as usize);

        Self {
            _source: source,
            pos: Pos { offset: 0 },
            chars: source.code.chars(),
        }
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.as_str().is_empty()
    }
    #[must_use]
    pub fn as_str_until(&self, pos: Pos) -> &'a str {
        &self.as_str()[..pos.offset.saturating_sub(self.pos.offset) as usize]
    }
    #[must_use]
    pub fn as_str(&self) -> &'a str {
        self.chars.as_str()
    }
    #[must_use]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.as_str().as_bytes()
    }
    #[must_use]
    pub fn pos(&self) -> Pos {
        self.pos
    }
    #[must_use]
    pub fn up_to(&self, next: &Self) -> Span {
        self.pos.up_to(next)
    }
    pub fn next_char(&mut self) -> Option<char> {
        let ch = self.chars.next()?;
        self.pos.advance_by_char(ch);
        Some(ch)
    }
    #[must_use]
    pub fn peek(&self) -> Option<char> {
        self.chars.clone().next()
    }
    pub fn take_char_if(&mut self, f: impl FnOnce(char) -> bool) -> Option<char> {
        f(self.peek()?).then(|| self.next_char().unwrap())
    }
    pub fn take_char_matches(&mut self, ch: char) -> bool {
        self.take_char_if(|ch1| ch1 == ch).is_some()
    }
    pub fn take_char_while(&mut self, mut f: impl FnMut(char) -> bool) -> (Span, &'a str) {
        let start = self.clone();
        while self.peek().is_some_and(&mut f) {
            self.next_char();
        }
        (start.up_to(self), start.as_str_until(self.pos()))
    }
    pub fn take_str_if_matches(&mut self, s: &str) -> Option<Span> {
        let start = self.clone();
        self.as_str().starts_with(s).then(|| {
            self.skip_n_bytes(s.len());
            start.up_to(self)
        })
    }
    pub fn take_newline(&mut self) -> bool {
        let mut chars = self.chars.clone();
        let ch = chars.next();
        if ch == Some('\n') || ch == Some('\r') && chars.next() == Some('\n') {
            self.pos.offset += (self.chars.as_str().len() - chars.as_str().len()) as u32;
            self.chars = chars;
            true
        } else {
            false
        }
    }
    pub fn skip_n_bytes(&mut self, mut n: usize) {
        let s = self.chars.as_str();
        n = n.min(s.len());
        self.pos.advance_by_str(&s[..n]);
        self.chars = s[n..].chars();
    }
    #[inline]
    pub fn try_speculate<T, E>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut peek = self.clone();
        let should_advance = f(&mut peek);
        if should_advance.is_ok() {
            *self = peek;
        }
        should_advance
    }
    #[inline]
    pub fn speculate_map<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        self.try_speculate(|peek| f(peek).ok_or(())).ok()
    }
    #[inline]
    pub fn speculate(&mut self, f: impl FnOnce(&mut Self) -> bool) -> bool {
        self.speculate_map(|peek| f(peek).then_some(())).is_some()
    }
}
