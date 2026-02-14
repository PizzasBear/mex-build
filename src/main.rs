use std::{fmt, fs, path::PathBuf};

use crate::code::{CodeIter, Source, Span};

mod code;

#[derive(Debug)]
struct ParseError {
    span: Span,
    msg: &'static str,
}

impl ParseError {
    fn new(span: Span, msg: &'static str) -> Self {
        Self { span, msg }
    }
    fn new_at(code: &CodeIter<'_>, msg: &'static str) -> Self {
        Self::new(code.pos().to_span(), msg)
    }
}

type ParseResult<T> = Result<T, ParseError>;

fn eat_whitespace(code: &mut CodeIter<'_>) -> ParseResult<()> {
    while code.take_char_matches(' ')
        || code.speculate(|code| code.next_char() == Some('$') && code.take_newline())
    {}

    if (code.peek()).is_some_and(|ch| !matches!(ch, '\r' | '\n') && ch.is_whitespace()) {
        return Err(ParseError::new_at(
            code,
            "the only supported whitespace is a space, ' '",
        ));
    }

    Ok(())
}

#[derive(Debug)]
struct Varname<'a> {
    name: &'a str,
    span: Span,
}

impl fmt::Display for Varname<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)?;
        Ok(())
    }
}

fn parse_varname<'a>(code: &mut CodeIter<'a>, simple: bool) -> ParseResult<Varname<'a>> {
    let (span, name) = code.take_char_while(|ch| {
        matches!(ch, 'a'..='z' | 'A'..='Z' | '0' ..= '9' | '_' | '-') || !simple && ch == '.'
    });
    match name {
        "" if simple => Err(ParseError::new_at(code, "expected a simple variable name")),
        "" if !simple => Err(ParseError::new_at(code, "expected a variable name")),
        _ => Ok(Varname { name, span }),
    }
}

#[derive(Debug)]
enum EvalStringPiece<'a> {
    Literal(&'a str),
    Var(Varname<'a>),
}

impl fmt::Display for EvalStringPiece<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(lit) => f.write_str(lit),
            Self::Var(var) => {
                f.write_str("${")?;
                fmt::Display::fmt(var, f)?;
                f.write_str("}")?;
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
struct EvalString<'a> {
    span: Span,
    pieces: Vec<EvalStringPiece<'a>>,
}

impl fmt::Display for EvalString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for piece in &self.pieces {
            fmt::Display::fmt(piece, f)?;
        }
        Ok(())
    }
}

fn parse_eval_str<'a>(code: &mut CodeIter<'a>, path: bool) -> ParseResult<EvalString<'a>> {
    let start = code.pos();

    let mut pieces = vec![];
    loop {
        let (_, lit) = code.take_char_while(|ch| {
            !matches!(ch, '\r' | '\n' | '$' | '\0') && !(path && matches!(ch, ' ' | ':' | '|'))
        });
        if !lit.is_empty() {
            pieces.push(EvalStringPiece::Literal(lit))
        }

        if code.take_newline() {
            break;
        } else if code.take_char_matches('$') {
            if code.take_char_matches('$') {
                pieces.push(EvalStringPiece::Literal("$"));
            } else if code.take_char_matches(' ') {
                pieces.push(EvalStringPiece::Literal(" "));
            } else if code.take_newline() {
                code.take_char_while(|ch| ch == ' ');
            } else if code.take_char_matches('{') {
                let name = parse_varname(code, false)?;
                pieces.push(EvalStringPiece::Var(name));
                if !code.take_char_matches('}') {
                    return Err(ParseError::new_at(
                        code,
                        "expected a closing curly brace '}'",
                    ));
                }
            } else if code.take_char_matches(':') {
                pieces.push(EvalStringPiece::Literal(":"));
            } else if code.take_char_matches('^') {
                // Starting with the yet unreleased, ninja 1.14
                pieces.push(EvalStringPiece::Literal("\n"));
            } else if let Ok(name) = parse_varname(code, true) {
                pieces.push(EvalStringPiece::Var(name));
            } else {
                return Err(ParseError::new_at(
                    code,
                    "bad $-escape (literal $ must be written as $$)",
                ));
            }
        } else if code.peek() == Some('\r') {
            return Err(ParseError::new_at(
                code,
                "naked carridge returns aren't supported",
            ));
        } else if code.is_empty() {
            return Err(ParseError::new_at(code, "unexpected EOF"));
        } else {
            return Err(ParseError::new_at(code, "unexpected character"));
        }
    }

    if path {
        eat_whitespace(code)?;
    }

    Ok(EvalString {
        span: start.up_to(code),
        pieces,
    })
}

#[derive(Debug)]
struct Comment<'a> {
    text_span: Span,
    text: &'a str,
    post_newlines: u32,
}

impl fmt::Display for Comment<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text)?;
        for _ in 0..self.post_newlines {
            f.write_str("\n")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Filler<'a> {
    start_newlines: u32,
    comments: Vec<Comment<'a>>,
}

impl fmt::Display for Filler<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for _ in 0..self.start_newlines {
            f.write_str("\n")?;
        }
        for comment in &self.comments {
            fmt::Display::fmt(comment, f)?;
        }
        Ok(())
    }
}

impl Filler<'_> {
    #[inline]
    #[must_use]
    fn is_empty(&self) -> bool {
        self.start_newlines == 0 && self.comments.len() == 0
    }
}

#[derive(Debug)]
enum FillerOr<'a, T: 'a> {
    Value(T),
    Filler(Filler<'a>),
}

impl<T: fmt::Display> fmt::Display for FillerOr<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(val) => fmt::Display::fmt(val, f),
            Self::Filler(filler) => fmt::Display::fmt(filler, f),
        }
    }
}

fn parse_filler<'a>(code: &mut CodeIter<'a>) -> ParseResult<Filler<'a>> {
    let mut start_newlines = 0;
    let mut comments = vec![];
    loop {
        let mut peek = code.clone();

        peek.take_char_while(|ch| ch == ' ');

        if peek.take_char_matches('#') {
            peek.take_char_while(|ch| ch != '\n' && ch != '\0');
            let text = code.as_str_until(peek.pos());
            let text_span = peek.up_to(code);
            *code = peek;
            if !code.take_newline() {
                return Err(ParseError::new_at(code, "expected newline"));
            }
            comments.push(Comment {
                text,
                text_span,
                post_newlines: 1,
            });
        } else if peek.take_newline() {
            *code = peek;
            match comments.last_mut() {
                Some(c) => c.post_newlines += 1,
                None => start_newlines += 1,
            }
        } else {
            break Ok(Filler {
                start_newlines,
                comments,
            });
        }
    }
}

#[derive(Debug)]
struct LetStmt<'a> {
    var: Varname<'a>,
    value: EvalString<'a>,
}

impl fmt::Display for LetStmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.var, f)?;
        f.write_str(" = ")?;
        fmt::Display::fmt(&self.value, f)?;
        f.write_str("\n")?;
        Ok(())
    }
}

fn parse_let<'a>(code: &mut CodeIter<'a>) -> ParseResult<LetStmt<'a>> {
    let var = parse_varname(code, false)?;
    eat_whitespace(code)?;
    if !code.take_char_matches('=') {
        return Err(ParseError::new_at(code, "expected assignment operator '='"));
    }
    eat_whitespace(code)?;
    let value = parse_eval_str(code, false)?;

    Ok(LetStmt { var, value })
}

#[derive(Debug)]
struct RuleBinding<'a> {
    indent_span: Span,
    let_stmt: LetStmt<'a>,
}

impl fmt::Display for RuleBinding<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for _ in 0..self.indent_span.len() {
            f.write_str(" ")?;
        }
        fmt::Display::fmt(&self.let_stmt, f)?;
        Ok(())
    }
}

#[derive(Debug)]
struct RuleStmt<'a> {
    rule_span: Span,
    name: Varname<'a>,
    bindings: Vec<FillerOr<'a, RuleBinding<'a>>>,
}

impl fmt::Display for RuleStmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rule ")?;
        fmt::Display::fmt(&self.name, f)?;
        f.write_str("\n")?;
        for binding in &self.bindings {
            fmt::Display::fmt(binding, f)?;
        }
        Ok(())
    }
}

fn is_reserved_binding(var: &str) -> bool {
    var == "command"
        || var == "depfile"
        || var == "dyndep"
        || var == "description"
        || var == "deps"
        || var == "generator"
        || var == "pool"
        || var == "restat"
        || var == "rspfile"
        || var == "rspfile_content"
        || var == "msvc_deps_prefix"
}

fn parse_rule<'a>(code: &mut CodeIter<'a>, rule_span: Span) -> ParseResult<RuleStmt<'a>> {
    eat_whitespace(code)?;
    let name = parse_varname(code, false)?;
    eat_whitespace(code)?;
    if !code.take_newline() {
        return Err(ParseError::new_at(code, "expected newline"));
    }

    let mut bindings = vec![];

    loop {
        let filler = parse_filler(code)?;
        if !filler.is_empty() {
            bindings.push(FillerOr::Filler(filler));
        }

        let (indent_span, indent) = code.take_char_while(|ch| ch == ' ');
        if indent.is_empty() {
            break;
        }

        let let_stmt = parse_let(code)?;
        if !is_reserved_binding(&let_stmt.var.name) {
            return Err(ParseError::new(let_stmt.var.span, "unexpected variable"));
        }
        bindings.push(FillerOr::Value(RuleBinding {
            indent_span,
            let_stmt,
        }));
    }

    Ok(RuleStmt {
        rule_span,
        name,
        bindings,
    })
}

#[derive(Debug)]
enum Item<'a> {
    Let(LetStmt<'a>),
    Rule(RuleStmt<'a>),
    Filler(Filler<'a>),
}

impl fmt::Display for Item<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Let(let_stmt) => fmt::Display::fmt(let_stmt, f),
            Item::Rule(rule_stmt) => fmt::Display::fmt(rule_stmt, f),
            Item::Filler(filler) => fmt::Display::fmt(filler, f),
        }
    }
}

fn parse_item<'a>(code: &mut CodeIter<'a>) -> ParseResult<Item<'a>> {
    loop {
        let filler = parse_filler(code)?;
        if !filler.is_empty() {
            return Ok(Item::Filler(filler));
        }

        let start = code.clone();
        let (indent_span, indent) = code.take_char_while(|ch| ch == ' ');

        if !indent.is_empty() {
            *code = start;
            return Err(ParseError::new(indent_span, "unexpected indent"));
        }

        if let Some(rule_span) = code.take_str_if_matches("rule") {
            break parse_rule(code, rule_span).map(Item::Rule);
        } else {
            break parse_let(code).map(Item::Let);
        }
    }
}

#[derive(Debug)]
struct MexFile<'a> {
    items: Vec<Item<'a>>,
}

impl fmt::Display for MexFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in &self.items {
            fmt::Display::fmt(item, f)?;
        }
        Ok(())
    }
}

fn parse_file<'a>(code: &mut CodeIter<'a>) -> ParseResult<MexFile<'a>> {
    let mut stmts = vec![];
    while !code.is_empty() {
        stmts.push(parse_item(code)?);
    }
    Ok(MexFile { items: stmts })
}

#[derive(clap::Parser, Debug)]
#[command(version, about)]
struct Args {
    /// change to DIR before doing anything else
    #[arg(short = 'C')]
    dir: Option<PathBuf>,

    /// specify input build file [default=build.mex]
    #[arg(short = 'f')]
    file: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = <Args as clap::Parser>::parse();

    if let Some(dir) = args.dir {
        std::env::set_current_dir(dir)?;
    }

    let file = args.file.unwrap_or_else(|| "build.mex".into());

    let source = Source {
        code: fs::read_to_string(&file)?,
        path: file,
    };

    let mut code_iter = code::CodeIter::new(&source);

    match parse_file(&mut code_iter) {
        Ok(val) => print!("{val}"),
        Err(err) => {
            anstream::println!(
                "{}",
                annotate_snippets::Renderer::styled().render(&[annotate_snippets::Level::ERROR
                    .primary_title(err.msg)
                    .element(
                        annotate_snippets::Snippet::source(source.code.as_str())
                            .path(source.path.to_string_lossy())
                            .annotation(
                                annotate_snippets::AnnotationKind::Primary
                                    .span(err.span.to_range())
                                    .label("here")
                            )
                    )])
            );
        }
    }

    Ok(())
}
