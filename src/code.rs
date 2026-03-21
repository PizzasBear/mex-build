use std::{fmt, ops, path::PathBuf, str};

pub trait Spanned {
    #[must_use]
    fn span(&self) -> Span;
}

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
    pub fn to(self, end: Self) -> Span {
        Span { start: self, end }
    }
    #[must_use]
    pub fn up_to(self, end: &impl Spanned) -> Span {
        self.to(end.span().start())
    }
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset as _
    }
}

impl Spanned for Pos {
    fn span(&self) -> Span {
        self.to(*self)
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
    pub fn join(&self, other: impl Spanned) -> Self {
        let other = other.span();
        (self.start().min(other.start())).to(self.end().max(other.end()))
    }
}

impl Spanned for Span {
    fn span(&self) -> Span {
        *self
    }
}

#[derive(Debug)]
pub struct Source {
    pub path: PathBuf,
    pub code: String,
}

impl Source {
    #[must_use]
    pub fn snippet<T: Clone>(&self) -> annotate_snippets::Snippet<'_, T> {
        annotate_snippets::Snippet::source(self.code.as_str()).path(self.path.to_string_lossy())
    }
}

#[derive(Debug, Clone)]
pub struct CodeIter<'a> {
    source: &'a Source,
    pos: Pos,
    chars: str::Chars<'a>,
}

impl<'a> CodeIter<'a> {
    #[must_use]
    pub fn new(source: &'a Source) -> Self {
        // TODO: handle in `Source` constructor
        assert!(source.code.len() <= u32::MAX as usize);

        Self {
            source,
            pos: Pos { offset: 0 },
            chars: source.code.chars(),
        }
    }
    #[must_use]
    pub fn source(&self) -> &'a Source {
        &self.source
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
    pub fn pos(&self) -> Pos {
        self.pos
    }
    #[must_use]
    pub fn up_to(&self, end: &impl Spanned) -> Span {
        self.pos.up_to(end)
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
    pub fn take_str_matches(&mut self, s: &str) -> Option<Span> {
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

impl Spanned for CodeIter<'_> {
    fn span(&self) -> Span {
        self.pos().span()
    }
}
