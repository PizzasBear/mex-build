#[macro_use]
extern crate anyhow;

use std::{borrow::Cow, fmt, fs, path::PathBuf};

use crate::{
    code::{CodeIter, Source, Span, Spanned},
    list::QuoteMode,
};

mod code;
mod list;

type ParseError<'a> = annotate_snippets::Group<'a>;

fn new_parse_error<'a>(
    code: &CodeIter<'a>,
    primary_title: impl Into<Cow<'a, str>>,
    annotation_span: &impl Spanned,
    annotation_label: impl Into<annotate_snippets::OptionCow<'a>>,
) -> annotate_snippets::Group<'a> {
    annotate_snippets::Level::ERROR
        .primary_title(primary_title)
        .element(
            code.source().snippet().annotation(
                annotate_snippets::AnnotationKind::Primary
                    .span(annotation_span.span().to_range())
                    .label(annotation_label),
            ),
        )
}

fn new_parse_error_expected<'a>(
    code: &CodeIter<'a>,
    primary_title: impl Into<Cow<'a, str>>,
    expected: impl Into<Cow<'a, str>>,
    annotation_span: &impl Spanned,
    annotation_label: impl Into<annotate_snippets::OptionCow<'a>>,
) -> annotate_snippets::Group<'a> {
    let annotation_span = annotation_span.span();
    annotate_snippets::Level::ERROR
        .primary_title(primary_title)
        .element(
            code.source().snippet().annotation(
                annotate_snippets::AnnotationKind::Primary
                    .span(annotation_span.to_range())
                    .label(annotation_label),
            ),
        )
        .element(annotate_snippets::Level::HELP.message("try applying the following change"))
        .element(code.source().snippet().patch(annotate_snippets::Patch::new(
            annotation_span.to_range(),
            expected,
        )))
}

fn eat_whitespace<'a>(code: &mut CodeIter<'a>, errors: Option<&mut Vec<ParseError<'a>>>) -> Span {
    let start = code.clone();

    loop {
        while code.take_char_matches(' ')
            || code.speculate(|code| code.next_char() == Some('$') && code.take_newline())
        {}

        let Some(&mut ref mut errors) = errors else {
            break;
        };

        let special_start = code.clone();
        if code.speculate(|code| code.next_char() == Some('\r') && code.peek() != Some('\n')) {
            errors.push(new_parse_error(
                code,
                "naked carriage returns aren't supported",
                &special_start.up_to(code),
                "here",
            ));
            continue;
        } else if code
            .take_char_if(|ch| !matches!(ch, '\r' | '\n') && ch.is_whitespace())
            .is_some()
        {
            errors.push(new_parse_error(
                code,
                "unsupported whitespace (only a plain space, ' ', is allowed)",
                &special_start.up_to(code),
                "here",
            ));
            continue;
        }

        break;
    }
    start.up_to(code)
}

#[derive(Debug)]
struct Varname<'a> {
    name: &'a str,
    span: Span,
}

impl<'a> Varname<'a> {
    const INVALID: &'static str = "<invalid-name>";

    #[must_use]
    fn as_str(&self) -> &'a str {
        self.name
    }

    #[must_use]
    fn new_invalid_at(code: &CodeIter<'a>) -> Self {
        Self {
            name: Self::INVALID,
            span: code.pos().span(),
        }
    }

    #[must_use]
    fn is_valid(&self) -> bool {
        self.name != Self::INVALID
    }

    #[must_use]
    fn valid(self) -> Option<Self> {
        self.is_valid().then_some(self)
    }
}

impl Spanned for Varname<'_> {
    fn span(&self) -> Span {
        self.span
    }
}

impl fmt::Display for Varname<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)?;
        Ok(())
    }
}

fn parse_varname<'a>(code: &mut CodeIter<'a>, simple: bool) -> Varname<'a> {
    let (span, name) = code.take_char_while(|ch| {
        matches!(ch, 'a'..='z' | 'A'..='Z' | '0' ..= '9' | '_' | '-') || !simple && ch == '.'
    });
    match name {
        "" => Varname::new_invalid_at(code),
        _ => Varname { name, span },
    }
}

#[derive(Debug)]
enum EvalStringPieceData<'a> {
    Literal(&'a str),
    Var(Varname<'a>),

    // MEX extension
    QuoteVar(Varname<'a>),
    Func(EvalString<'a>),
    QuoteFunc(EvalString<'a>),
}

impl fmt::Display for EvalStringPieceData<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal("$") => f.write_str("$$"),
            Self::Literal("|") => f.write_str("$|"),
            Self::Literal(":") => f.write_str("$:"),
            Self::Literal(lit) => f.write_str(lit),
            Self::Var(var) => {
                f.write_str("${")?;
                fmt::Display::fmt(var, f)?;
                f.write_str("}")?;
                Ok(())
            }
            Self::QuoteVar(var) => {
                f.write_str("$\"")?;
                fmt::Display::fmt(var, f)?;
                f.write_str("\"")?;
                Ok(())
            }
            Self::Func(func) => {
                f.write_str("$(")?;
                fmt::Display::fmt(func, f)?;
                f.write_str(")")?;
                Ok(())
            }
            Self::QuoteFunc(func) => {
                f.write_str("$\"(")?;
                fmt::Display::fmt(func, f)?;
                f.write_str(")\"")?;
                Ok(())
            }
        }
    }
}

#[derive(Debug)]
struct EvalStringPiece<'a> {
    span: Span,
    data: EvalStringPieceData<'a>,
}

impl<'a> EvalStringPiece<'a> {
    #[must_use]
    const fn new(span: Span, data: EvalStringPieceData<'a>) -> Self {
        Self { span, data }
    }
    #[must_use]
    const fn lit(span: Span, lit: &'a str) -> Self {
        Self::new(span, EvalStringPieceData::Literal(lit))
    }

    #[must_use]
    const fn var(span: Span, var: Varname<'a>) -> Self {
        Self::new(span, EvalStringPieceData::Var(var))
    }

    #[must_use]
    const fn func(span: Span, args: EvalString<'a>) -> Self {
        Self::new(span, EvalStringPieceData::Func(args))
    }
}

impl Spanned for EvalStringPiece<'_> {
    fn span(&self) -> Span {
        self.span
    }
}

impl fmt::Display for EvalStringPiece<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.data, f)
    }
}

#[derive(Debug)]
struct EvalString<'a> {
    span: Span,
    pieces: Vec<EvalStringPiece<'a>>,
}

impl Spanned for EvalString<'_> {
    fn span(&self) -> Span {
        self.span
    }
}

impl fmt::Display for EvalString<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for piece in &self.pieces {
            fmt::Display::fmt(piece, f)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvalStrNewline {
    Ignore,
    Stop,
    Consume,
}

fn parse_eval_str<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    stop_chars: &[char],
    newline_handling: EvalStrNewline,
    quote_aware: bool,
) -> EvalString<'a> {
    let start = code.pos();

    let mut pieces = vec![];
    let mut quote_mode = QuoteMode::Unquoted;
    let mut backslash_escape = false;
    let mut ws = true;

    loop {
        let (lit_span, lit) = code.take_char_while(|ch| {
            if matches!(ch, '\r' | '\n' | '$') {
                return false;
            }
            if !quote_aware {
                return !stop_chars.contains(&ch);
            }

            let prev_ws = ws;
            ws = false;

            if backslash_escape {
                backslash_escape = false;
                return true;
            }

            match (quote_mode, ch) {
                (QuoteMode::Unquoted, ch) if stop_chars.contains(&ch) => return false,
                (QuoteMode::Unquoted, ' ' | '\t' | '\n') => {
                    ws = true;
                }
                (QuoteMode::Unquoted, '\\') => {
                    backslash_escape = true;
                    ws = prev_ws;
                }
                (QuoteMode::Unquoted, '\'') => quote_mode = QuoteMode::SingleQuoted,
                (QuoteMode::Unquoted, '"') => quote_mode = QuoteMode::DoubleQuoted,
                (QuoteMode::SingleQuoted, '\'') => quote_mode = QuoteMode::Unquoted,
                (QuoteMode::DoubleQuoted, '\\') => {
                    backslash_escape = true;
                }
                (QuoteMode::DoubleQuoted, '"') => quote_mode = QuoteMode::Unquoted,
                _ => {}
            }
            true
        });

        let prev_backslash_escape = backslash_escape;
        backslash_escape = false;
        let prev_ws = ws;
        ws = false;

        if !lit.is_empty() {
            pieces.push(EvalStringPiece::lit(lit_span, lit))
        }

        let mut dollar_escape = false;

        let piece_start = code.clone();
        if code.take_newline() {
            ws = prev_ws || !prev_backslash_escape;
            match newline_handling {
                EvalStrNewline::Ignore => {
                    pieces.push(EvalStringPiece::lit(piece_start.up_to(code), "\n"))
                }
                EvalStrNewline::Stop => {
                    *code = piece_start;
                    break;
                }
                EvalStrNewline::Consume => break,
            }
        } else if code.peek().is_some_and(|ch| stop_chars.contains(&ch)) {
            break;
        } else if code.take_char_matches('\r') {
            errors.push(new_parse_error(
                code,
                "naked carriage returns aren't supported",
                &*code,
                "here",
            ));
        } else if code.is_empty() {
            if matches!(newline_handling, EvalStrNewline::Consume) {
                errors.push(new_parse_error(code, "unexpected EOF", &*code, "here"));
            }
            break;
        } else if code.take_char_matches('$') {
            dollar_escape = true
        } else {
            unreachable!();
        }

        if !dollar_escape {
            continue;
        }

        let mut expansion_escape = false;

        let escapee_start = code.pos();
        if code.take_char_matches('$') {
            pieces.push(EvalStringPiece::lit(piece_start.up_to(code), "$"));
        } else if code.take_char_matches(':') {
            pieces.push(EvalStringPiece::lit(piece_start.up_to(code), ":"));
        } else if code.take_char_matches(' ') {
            ws = !prev_backslash_escape;
            pieces.push(EvalStringPiece::lit(piece_start.up_to(code), " "));
        } else if code.take_char_matches('^') {
            // Starting with the yet unreleased, ninja 1.14
            ws |= !prev_backslash_escape;
            pieces.push(EvalStringPiece::lit(piece_start.up_to(code), "\n"));
        } else if code.take_newline() {
            backslash_escape = prev_backslash_escape;
            code.take_char_while(|ch| ch == ' ');
        } else if let Some(quote_ch) = code.take_char_if(|ch| matches!(ch, '"' | '\'')) {
            // MEX extension
            let opening_quote_span = escapee_start.up_to(code);

            if prev_backslash_escape {
                errors.push(new_parse_error_expected(
                    code,
                    "quoted values cannot be preceded by backslashes",
                    "\\",
                    &piece_start.span().start(),
                    "here",
                ));
            }

            let mut eat_closing_quote = true;
            let data = if code.take_char_matches('(') {
                let func_str = parse_eval_str(code, errors, &[')'], EvalStrNewline::Ignore, true);
                if !code.take_char_matches(')') {
                    eat_closing_quote = false;
                    errors.push(new_parse_error_expected(
                        code,
                        "expected closing parenthesis ')'",
                        ")",
                        &*code,
                        "here",
                    ));
                }
                EvalStringPieceData::QuoteFunc(func_str)
            } else {
                let name = parse_varname(code, false);
                if !name.is_valid() {
                    errors.push(new_parse_error_expected(
                        code,
                        "expected a variable name",
                        "<variable_name>",
                        &*code,
                        "here",
                    ));
                }
                EvalStringPieceData::QuoteVar(name)
            };

            let closing_quote_start = code.pos();
            if eat_closing_quote && !code.take_char_matches(quote_ch) {
                errors.push(new_parse_error_expected(
                    code,
                    "expected a closing double quote (\")",
                    "\"",
                    &*code,
                    "here",
                ));
            }
            let closing_quote_span = closing_quote_start.up_to(code);

            pieces.push(EvalStringPiece::new(piece_start.up_to(code), data));

            if eat_closing_quote && quote_ch != '\"' {
                errors.push(
                    annotate_snippets::Level::ERROR
                        .primary_title("quoted variables and functions must use double quotes (\")")
                        .element(
                            code.source()
                                .snippet()
                                .annotation(
                                    annotate_snippets::AnnotationKind::Primary
                                        .span(opening_quote_span.to_range())
                                        .label("here"),
                                )
                                .annotation(
                                    annotate_snippets::AnnotationKind::Primary
                                        .span(closing_quote_span.to_range())
                                        .label("and here"),
                                ),
                        )
                        .element(
                            annotate_snippets::Level::HELP
                                .message("try applying the following change"),
                        )
                        .element(code.source().snippet().patches([
                            annotate_snippets::Patch::new(opening_quote_span.to_range(), "\""),
                            annotate_snippets::Patch::new(closing_quote_span.to_range(), "\""),
                        ])),
                );
            }

            if quote_mode != QuoteMode::Unquoted {
                errors.push(new_parse_error(
                    code,
                    "cannot nest quoted expressions inside other quotes",
                    &piece_start.up_to(code),
                    "this expression is quoted",
                ));
            }
        } else {
            expansion_escape = true;
        }

        if !expansion_escape {
            continue;
        }

        if quote_aware && !prev_ws {
            errors.push(new_parse_error_expected(
                code,
                "an expansion must be preceded by whitespace",
                match prev_backslash_escape {
                    true => "\\ ",
                    false => " ",
                },
                &piece_start,
                "here",
            ));
        }

        if code.take_char_matches('{') {
            let name = parse_varname(code, false);
            if !name.is_valid() {
                errors.push(new_parse_error_expected(
                    code,
                    "expected a variable name",
                    "<variable_name>",
                    &*code,
                    "here",
                ));
            }
            pieces.push(EvalStringPiece::var(piece_start.up_to(code), name));
            if !code.take_char_matches('}') {
                errors.push(new_parse_error_expected(
                    code,
                    "expected a closing curly brace '}'",
                    "}",
                    &*code,
                    "here",
                ));
            }
        } else if code.take_char_matches('(') {
            // MEX extension
            let func_str = parse_eval_str(code, errors, &[')'], EvalStrNewline::Ignore, true);
            pieces.push(EvalStringPiece::func(piece_start.up_to(code), func_str));
            if !code.take_char_matches(')') {
                errors.push(new_parse_error_expected(
                    code,
                    "expected closing parenthesis ')'",
                    ")",
                    &*code,
                    "here",
                ));
            }
        } else if let Some(name) = parse_varname(code, true).valid() {
            pieces.push(EvalStringPiece::var(piece_start.up_to(code), name));
        } else {
            pieces.push(EvalStringPiece::lit(piece_start.up_to(code), "$"));
            errors.push(new_parse_error(
                code,
                "bad $-escape (literal $ must be written as $$)",
                &piece_start.up_to(code),
                "here",
            ));
            continue;
        }

        if quote_aware && quote_mode != QuoteMode::Unquoted {
            errors.push(
                new_parse_error(
                    code,
                    "cannot expand inside quotes",
                    pieces.last().unwrap(),
                    "here",
                )
                .element(
                    annotate_snippets::Level::HELP
                        .message("the variable may contain quotes which will corrupt quoting"),
                ),
            );
        }
    }

    // Check for quote & whitespace integrity
    for [piece, next_piece] in pieces.array_windows() {
        use EvalStringPieceData as Data;

        if !quote_aware || !matches!(piece.data, Data::Var(_) | Data::Func(_)) {
            continue;
        }

        let forward_err = match next_piece.data {
            Data::Literal(lit) => !lit.starts_with(&[' ', '\t', '\n']),
            Data::Func(_) | Data::Var(_) => false,
            _ => true,
        };

        if forward_err {
            errors.push(new_parse_error_expected(
                code,
                "an expansion must be followed by whitespace",
                " ",
                &piece.span().end(),
                "after this",
            ));
        }
    }

    EvalString {
        span: start.up_to(code),
        pieces,
    }
}

fn parse_paths<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
) -> Vec<EvalString<'a>> {
    let mut paths = vec![];
    eat_whitespace(code, Some(errors));
    let start = code.clone();
    loop {
        let path = parse_eval_str(code, errors, &[' ', ':', '|'], EvalStrNewline::Stop, false);
        eat_whitespace(code, Some(errors));
        if path.pieces.is_empty() {
            break;
        }
        paths.push(path);
    }
    if paths.is_empty() {
        assert_eq!(start.pos(), code.pos());
    }
    paths
}

#[derive(Debug)]
struct Comment<'a> {
    text_span: Span,
    text: &'a str,
    post_newlines: u32,
}

impl Spanned for Comment<'_> {
    fn span(&self) -> Span {
        self.text_span
    }
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
    span: Span,
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

fn parse_filler<'a>(code: &mut CodeIter<'a>, errors: &mut Vec<ParseError<'a>>) -> Filler<'a> {
    let start = code.pos();

    let mut start_newlines = 0;
    let mut comments = vec![];
    loop {
        let mut peek = code.clone();

        peek.take_char_while(|ch| ch == ' ');

        if peek.take_char_matches('#') {
            peek.take_char_while(|ch| ch != '\n');
            let text = code.as_str_until(peek.pos());
            let text_span = peek.up_to(code);
            *code = peek;
            if !code.take_newline() {
                assert!(code.is_empty());
                errors.push(new_parse_error_expected(
                    code,
                    "expected newline",
                    "\n",
                    &*code,
                    "here",
                ));
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
            break Filler {
                span: start.up_to(code),
                start_newlines,
                comments,
            };
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
        if !matches!(self.var.as_str(), "@" | "!") {
            f.write_str(" = ")?;
        }
        fmt::Display::fmt(&self.value, f)?;
        f.write_str("\n")?;
        Ok(())
    }
}

fn parse_let<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    var: Varname<'a>,
) -> LetStmt<'a> {
    if var.is_valid() {
        eat_whitespace(code, Some(errors));
        if !code.take_char_matches('=') {
            errors.push(new_parse_error_expected(
                code,
                "expected assignment operator '='",
                " = ",
                &*code,
                "here",
            ));
        }
    } else {
        errors.push(new_parse_error_expected(
            code,
            "expected a variable name",
            "<variable_name> = ",
            &*code,
            "here",
        ));
    }
    eat_whitespace(code, None);
    let value = parse_eval_str(code, errors, &[], EvalStrNewline::Consume, false);

    LetStmt { var, value }
}

#[derive(Debug)]
struct Binding<'a> {
    indent_span: Span,
    let_stmt: LetStmt<'a>,
}

impl fmt::Display for Binding<'_> {
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
    bindings: Vec<(Filler<'a>, Binding<'a>)>,
}

impl fmt::Display for RuleStmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rule ")?;
        fmt::Display::fmt(&self.name, f)?;
        f.write_str("\n")?;
        for (filler, binding) in &self.bindings {
            fmt::Display::fmt(filler, f)?;
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

fn parse_rule<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    base_indent: Span,
    rule_span: Span,
) -> (RuleStmt<'a>, Filler<'a>, Span) {
    eat_whitespace(code, Some(errors));
    let name = parse_varname(code, false);
    if !name.is_valid() {
        errors.push(new_parse_error_expected(
            code,
            "expected a rule name",
            " <rule_name>",
            &*code,
            "here",
        ));
    }
    eat_whitespace(code, Some(errors));
    if !code.take_newline() {
        errors.push(new_parse_error_expected(
            code,
            "expected newline",
            "\n",
            &*code,
            "here",
        ));
        code.take_char_while(|ch| ch != '\n');
        code.take_newline();
    }

    let mut bindings = vec![];
    let (filler, indent_span) = loop {
        let filler = parse_filler(code, errors);

        let indent_span = eat_whitespace(code, Some(errors));
        if indent_span.len() <= base_indent.len() {
            break (filler, indent_span);
        }

        let name_start = code.clone();
        if code.take_char_if(|ch| matches!(ch, '@' | '!')).is_some() {
            // MEX extension
            let var = Varname {
                span: name_start.up_to(code),
                name: name_start.as_str_until(code.pos()),
            };
            let value = parse_eval_str(code, errors, &[], EvalStrNewline::Consume, false);
            let binding = Binding {
                indent_span,
                let_stmt: LetStmt { var, value },
            };
            bindings.push((filler, binding))
        } else {
            let var = parse_varname(code, false);
            let let_stmt = parse_let(code, errors, var);
            if !is_reserved_binding(&let_stmt.var.as_str()) && let_stmt.var.is_valid() {
                errors.push(
                    annotate_snippets::Level::ERROR
                        .primary_title(format!("unexpected variable '{}'", let_stmt.var))
                        .element(
                            code.source().snippet().annotations([
                                annotate_snippets::AnnotationKind::Context
                                    .span(rule_span.join(name.span()).to_range())
                                    .label("while parsing"),
                                annotate_snippets::AnnotationKind::Primary
                                    .span(let_stmt.var.span().to_range())
                                    .label("here"),
                            ]),
                        )
                        .element(
                            annotate_snippets::Level::INFO
                                .message("rules may only bind certain variables"),
                        ),
                );
            }
            let binding = Binding {
                indent_span,
                let_stmt,
            };
            bindings.push((filler, binding));
        }
    };

    let rule_stmt = RuleStmt {
        rule_span,
        name,
        bindings,
    };

    (rule_stmt, filler, indent_span)
}

#[derive(Debug)]
struct BuildStmt<'a> {
    build_span: Span,
    outs: Vec<EvalString<'a>>,
    implicit_outs: Vec<EvalString<'a>>,
    rule: Varname<'a>,
    deps: Vec<EvalString<'a>>,
    implicit_deps: Vec<EvalString<'a>>,
    order_only_deps: Vec<EvalString<'a>>,
    validations: Vec<EvalString<'a>>,
    bindings: Vec<(Filler<'a>, Binding<'a>)>,
}

impl fmt::Display for BuildStmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("build")?;
        for out in &self.outs {
            f.write_str(" ")?;
            fmt::Display::fmt(out, f)?;
        }
        if !self.implicit_outs.is_empty() {
            f.write_str(" |")?;
        }
        for out in &self.implicit_outs {
            f.write_str(" ")?;
            fmt::Display::fmt(out, f)?;
        }
        f.write_str(": ")?;
        fmt::Display::fmt(&self.rule, f)?;
        for deps in &self.deps {
            f.write_str(" ")?;
            fmt::Display::fmt(deps, f)?;
        }
        if !self.implicit_deps.is_empty() {
            f.write_str(" |")?;
        }
        for dep in &self.implicit_deps {
            f.write_str(" ")?;
            fmt::Display::fmt(dep, f)?;
        }
        if !self.order_only_deps.is_empty() {
            f.write_str(" ||")?;
        }
        for dep in &self.order_only_deps {
            f.write_str(" ")?;
            fmt::Display::fmt(dep, f)?;
        }
        if !self.validations.is_empty() {
            f.write_str(" |@")?;
        }
        for val in &self.validations {
            f.write_str(" ")?;
            fmt::Display::fmt(val, f)?;
        }
        f.write_str("\n")?;
        for (filler, binding) in &self.bindings {
            fmt::Display::fmt(filler, f)?;
            fmt::Display::fmt(binding, f)?;
        }
        Ok(())
    }
}

fn parse_build<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    base_indent: Span,
    build_span: Span,
) -> (BuildStmt<'a>, Filler<'a>, Span) {
    let outs = parse_paths(code, errors);
    let implicit_outs = match code.take_char_matches('|') {
        true => parse_paths(code, errors),
        false => vec![],
    };
    if outs.is_empty() && implicit_outs.is_empty() {
        errors.push(new_parse_error_expected(
            code,
            "expected output path",
            "<output-path>",
            &*code,
            "here",
        ));
    }
    if !code.take_char_matches(':') {
        errors.push(new_parse_error_expected(
            code,
            "expected colon",
            ":",
            &*code,
            "here",
        ));
    }
    eat_whitespace(code, Some(errors));
    let rule = parse_varname(code, false);
    if !rule.is_valid() {
        errors.push(new_parse_error_expected(
            code,
            "expected a rule name",
            "<rule_name>",
            &*code,
            "here",
        ));
    }
    let deps = parse_paths(code, errors);
    let implicit_deps = match code.take_char_matches('|') {
        true => parse_paths(code, errors),
        false => vec![],
    };
    let order_only_deps = match code.take_str_matches("||") {
        Some(_) => parse_paths(code, errors),
        None => vec![],
    };
    let validations = match code.take_str_matches("|@") {
        Some(_) => parse_paths(code, errors),
        None => vec![],
    };
    if !code.take_newline() {
        let mut peek = code.clone();
        errors.push(match peek.next_char() {
            Some(':') => new_parse_error(code, "unexpected colon", &code.up_to(&peek), "here"),
            _ => new_parse_error_expected(code, "expected newline", "\n", &*code, "here"),
        });

        code.take_char_while(|ch| ch != '\n');
        code.take_newline();
    }

    let mut bindings = vec![];

    let (filler, indent_span) = loop {
        let filler = parse_filler(code, errors);

        let indent_span = eat_whitespace(code, Some(errors));
        if indent_span.len() <= base_indent.len() {
            break (filler, indent_span);
        }

        let var = parse_varname(code, false);
        let let_stmt = parse_let(code, errors, var);
        let binding = Binding {
            indent_span,
            let_stmt,
        };
        bindings.push((filler, binding));
    };

    let build_stmt = BuildStmt {
        build_span,
        outs,
        implicit_outs,
        rule,
        deps,
        implicit_deps,
        order_only_deps,
        validations,
        bindings,
    };

    (build_stmt, filler, indent_span)
}

#[derive(Debug)]
struct DefaultStmt<'a> {
    default_span: Span,
    outs: Vec<EvalString<'a>>,
}

impl fmt::Display for DefaultStmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("default")?;
        for out in &self.outs {
            f.write_str(" ")?;
            fmt::Display::fmt(out, f)?;
        }
        f.write_str("\n")?;
        Ok(())
    }
}

fn parse_default<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    default_span: Span,
) -> DefaultStmt<'a> {
    let outs = parse_paths(code, errors);
    if !code.take_newline() {
        let mut peek = code.clone();
        errors.push(match peek.next_char() {
            Some(':') => new_parse_error(code, "unexpected colon", &code.up_to(&peek), "here"),
            Some('|') => new_parse_error(code, "unexpected pipe", &code.up_to(&peek), "here"),
            _ => new_parse_error_expected(code, "expected newline", "\n", &*code, "here"),
        });

        code.take_char_while(|ch| ch != '\n');
        code.take_newline();
    }

    DefaultStmt { default_span, outs }
}

#[derive(Debug)]
struct PoolStmt<'a> {
    pool_span: Span,
    name: Varname<'a>,
    bindings: Vec<(Filler<'a>, Binding<'a>)>,
}

impl fmt::Display for PoolStmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("pool ")?;
        fmt::Display::fmt(&self.name, f)?;
        f.write_str("\n")?;
        for (filler, binding) in &self.bindings {
            fmt::Display::fmt(filler, f)?;
            fmt::Display::fmt(binding, f)?;
        }
        Ok(())
    }
}

fn parse_pool<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    base_indent: Span,
    pool_span: Span,
) -> (PoolStmt<'a>, Filler<'a>, Span) {
    eat_whitespace(code, Some(errors));
    let name = parse_varname(code, false);
    if !name.is_valid() {
        errors.push(new_parse_error_expected(
            code,
            "expected a pool name",
            "<pool_name>",
            &*code,
            "here",
        ));
    }
    eat_whitespace(code, Some(errors));
    if !code.take_newline() {
        errors.push(new_parse_error_expected(
            code,
            "expected newline",
            "\n",
            &*code,
            "here",
        ));

        code.take_char_while(|ch| ch != '\n');
        code.take_newline();
    }

    let mut bindings = vec![];

    let mut depth_defined = false;
    let (filler, indent_span) = loop {
        let filler = parse_filler(code, errors);

        let indent_span = eat_whitespace(code, Some(errors));
        if indent_span.len() <= base_indent.len() {
            break (filler, indent_span);
        }

        let var = parse_varname(code, false);
        let let_stmt = parse_let(code, errors, var);
        if let_stmt.var.as_str() != "depth" && let_stmt.var.is_valid() {
            errors.push(
                annotate_snippets::Level::ERROR
                    .primary_title(format!("unexpected variable '{}'", let_stmt.var))
                    .element(
                        code.source().snippet().annotations([
                            annotate_snippets::AnnotationKind::Context
                                .span(pool_span.join(name.span()).to_range())
                                .label("while parsing"),
                            annotate_snippets::AnnotationKind::Primary
                                .span(let_stmt.var.span().to_range())
                                .label("here"),
                        ]),
                    )
                    .element(
                        annotate_snippets::Level::INFO
                            .message("pools must have only a 'depth' variable binding"),
                    ),
            );
        } else {
            depth_defined = true;
            let binding = Binding {
                indent_span,
                let_stmt,
            };
            bindings.push((filler, binding));
        }
    };

    if !depth_defined {
        errors.push(
            annotate_snippets::Level::ERROR
                .primary_title("expected 'depth =' line")
                .element(
                    code.source().snippet().annotations([
                        annotate_snippets::AnnotationKind::Context
                            .span(pool_span.join(name.span()).to_range())
                            .label("while parsing"),
                        annotate_snippets::AnnotationKind::Primary
                            .span(code.span().to_range())
                            .label("here"),
                    ]),
                )
                .element(
                    annotate_snippets::Level::INFO
                        .message("pools require a 'depth' variable binding"),
                )
                .element(annotate_snippets::Level::HELP.message("try the following change"))
                .element(code.source().snippet().patch(annotate_snippets::Patch::new(
                    filler.span.start().span().to_range(),
                    "\n    depth = <number>",
                ))),
        );
    }

    let pool_stmt = PoolStmt {
        pool_span,
        name,
        bindings,
    };

    (pool_stmt, filler, indent_span)
}

#[derive(Debug)]
struct ForStmt<'a> {
    for_span: Span,
    var: Varname<'a>,
    values: EvalString<'a>,
    items: Vec<ForBodyItem<'a>>,
}

#[derive(Debug)]
struct ForBodyItem<'a> {
    filler: Filler<'a>,
    indent_span: Span,
    item: Item<'a>,
}

impl fmt::Display for ForStmt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("for ")?;
        fmt::Display::fmt(&self.var, f)?;
        f.write_str(" in ")?;
        fmt::Display::fmt(&self.values, f)?;
        f.write_str("\n")?;
        for item in &self.items {
            fmt::Display::fmt(item, f)?;
        }
        Ok(())
    }
}

impl fmt::Display for ForBodyItem<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.filler, f)?;
        for _ in 0..self.indent_span.len() {
            f.write_str(" ")?;
        }
        fmt::Display::fmt(&self.item, f)?;

        Ok(())
    }
}

#[derive(Debug)]
enum Item<'a> {
    Let(LetStmt<'a>),
    Rule(RuleStmt<'a>),
    Build(BuildStmt<'a>),
    Default(DefaultStmt<'a>),
    Pool(PoolStmt<'a>),

    // MEX extension
    For(ForStmt<'a>),
}

impl fmt::Display for Item<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Let(let_stmt) => fmt::Display::fmt(let_stmt, f),
            Item::Rule(rule_stmt) => fmt::Display::fmt(rule_stmt, f),
            Item::Build(build_stmt) => fmt::Display::fmt(build_stmt, f),
            Item::Default(default_stmt) => fmt::Display::fmt(default_stmt, f),
            Item::Pool(default_stmt) => fmt::Display::fmt(default_stmt, f),
            Item::For(for_stmt) => fmt::Display::fmt(for_stmt, f),
        }
    }
}

fn parse_for<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    base_indent: Span,
    for_span: Span,
) -> (ForStmt<'a>, Filler<'a>, Span) {
    eat_whitespace(code, Some(errors));
    let var = parse_varname(code, false);
    if !var.is_valid() {
        errors.push(new_parse_error_expected(
            code,
            "expected a variable name",
            " <var_name>",
            &*code,
            "here",
        ));
    };
    eat_whitespace(code, Some(errors));

    let _in_span = code.take_str_matches("in").unwrap_or_else(|| {
        errors.push(new_parse_error_expected(
            code,
            "expected 'in'",
            " in",
            &*code,
            "here",
        ));
        code.span()
    });
    eat_whitespace(code, Some(errors));

    if !matches!(
        code.peek(),
        Some('a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '-' | '.'),
    ) {
        errors.push(new_parse_error_expected(
            code,
            "expected a space ' '",
            " ",
            code,
            "here",
        ));
    }

    let values = parse_eval_str(code, errors, &[], EvalStrNewline::Consume, false);

    let mut items = vec![];
    let mut filler = parse_filler(code, errors);
    let mut indent_span = eat_whitespace(code, Some(errors));

    loop {
        if indent_span.len() <= base_indent.len() {
            break;
        }

        let (item, post_filler, post_indent) = parse_item(code, errors, indent_span);
        items.push(ForBodyItem {
            filler,
            indent_span,
            item,
        });

        filler = post_filler;
        indent_span = post_indent;
    }

    let stmt = ForStmt {
        for_span,
        var,
        values,
        items,
    };
    (stmt, filler, indent_span)
}

fn parse_item<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    indent: Span,
) -> (Item<'a>, Filler<'a>, Span) {
    let name = parse_varname(code, false);
    if name.as_str() == "rule" {
        let (rule_stmt, filler, indent_span) = parse_rule(code, errors, indent, name.span());
        (Item::Rule(rule_stmt), filler, indent_span)
    } else if name.as_str() == "build" {
        let (build_stmt, filler, indent_span) = parse_build(code, errors, indent, name.span());
        (Item::Build(build_stmt), filler, indent_span)
    } else if name.as_str() == "default" {
        let default_stmt = parse_default(code, errors, name.span());
        (
            Item::Default(default_stmt),
            parse_filler(code, errors),
            eat_whitespace(code, Some(errors)),
        )
    } else if name.as_str() == "pool" {
        let (pool_stmt, filler, indent_span) = parse_pool(code, errors, indent, name.span());
        (Item::Pool(pool_stmt), filler, indent_span)
    } else if name.as_str() == "for" {
        // MEX extension
        let (for_stmt, filler, indent_span) = parse_for(code, errors, indent, name.span());
        (Item::For(for_stmt), filler, indent_span)
    } else {
        let let_stmt = parse_let(code, errors, name);
        (
            Item::Let(let_stmt),
            parse_filler(code, errors),
            eat_whitespace(code, Some(errors)),
        )
    }
}

#[derive(Debug)]
struct MexFile<'a> {
    opening_filler: Filler<'a>,
    items: Vec<(Item<'a>, Filler<'a>)>,
}

impl fmt::Display for MexFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.opening_filler, f)?;
        for (item, filler) in &self.items {
            fmt::Display::fmt(item, f)?;
            fmt::Display::fmt(filler, f)?;
        }
        Ok(())
    }
}

fn parse_file<'a>(code: &mut CodeIter<'a>, errors: &mut Vec<ParseError<'a>>) -> MexFile<'a> {
    let mut items = vec![];

    let opening_filler = parse_filler(code, errors);

    let mut indent_span = eat_whitespace(code, Some(errors));

    while !code.is_empty() {
        if !indent_span.is_empty() {
            errors.push(new_parse_error(
                code,
                "unexpected indent",
                &indent_span,
                "here",
            ));
        }

        let (item, filler);
        (item, filler, indent_span) = parse_item(code, errors, indent_span);

        items.push((item, filler));
    }

    MexFile {
        opening_filler,
        items,
    }
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

    // TODO: improve UTF-8 & \0 error handling
    let source = Source {
        code: fs::read_to_string(&file)?,
        path: file,
    };

    if source.code.contains('\0') {
        bail!(
            "Null bytes are not supported (file: {})",
            source.path.display()
        );
    }

    let mut errors = vec![];
    let mut code_iter = code::CodeIter::new(&source);
    let file_ast = parse_file(&mut code_iter, &mut errors);

    println!("=== File AST ===");
    println!("{file_ast}");

    if !errors.is_empty() {
        println!("=== ERRORS ===");
        anstream::println!(
            "{}",
            annotate_snippets::Renderer::styled()
                .decor_style(annotate_snippets::renderer::DecorStyle::Ascii)
                .render(&errors)
        );
    }

    Ok(())
}
