#[macro_use]
extern crate anyhow;

use std::{borrow::Cow, fmt, fs, path::PathBuf};

use crate::code::{CodeIter, Source, Span, Spanned};

mod code;

type ParseError<'a> = annotate_snippets::Group<'a>;

fn new_parse_error<'a>(
    code: &CodeIter<'a>,
    primary_title: impl Into<Cow<'a, str>>,
    annotation_span: impl Spanned,
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
                "naked carridge returns aren't supported",
                special_start.up_to(code),
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
                special_start.up_to(code),
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

    fn new_invalid_at(code: &CodeIter<'a>) -> Self {
        Self {
            name: Self::INVALID,
            span: code.pos().span(),
        }
    }

    fn is_valid(&self) -> bool {
        self.name != Self::INVALID
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

fn parse_varname<'a>(code: &mut CodeIter<'a>, simple: bool) -> Option<Varname<'a>> {
    let (span, name) = code.take_char_while(|ch| {
        matches!(ch, 'a'..='z' | 'A'..='Z' | '0' ..= '9' | '_' | '-') || !simple && ch == '.'
    });
    match name {
        "" => None,
        _ => Some(Varname { name, span }),
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

fn parse_eval_str<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
    path: bool,
) -> EvalString<'a> {
    let start = code.pos();

    let mut pieces = vec![];
    loop {
        let (_, lit) = code.take_char_while(|ch| {
            !matches!(ch, '\r' | '\n' | '$') && !(path && matches!(ch, ' ' | ':' | '|'))
        });
        if !lit.is_empty() {
            pieces.push(EvalStringPiece::Literal(lit))
        }

        let piece_start = code.clone();
        if code.take_newline() {
            if path {
                *code = piece_start;
            }
            break;
        } else if path && matches!(code.peek(), Some(' ' | ':' | '|')) {
            break;
        } else if code.take_char_matches('$') {
            if code.take_char_matches('$') {
                pieces.push(EvalStringPiece::Literal("$"));
            } else if code.take_char_matches(' ') {
                pieces.push(EvalStringPiece::Literal(" "));
            } else if code.take_newline() {
                code.take_char_while(|ch| ch == ' ');
            } else if code.take_char_matches('{') {
                let name = parse_varname(code, false).unwrap_or_else(|| {
                    errors.push(new_parse_error(
                        code,
                        "expected a variable name",
                        &*code,
                        "here",
                    ));
                    Varname::new_invalid_at(code)
                });
                pieces.push(EvalStringPiece::Var(name));
                if !code.take_char_matches('}') {
                    errors.push(new_parse_error(
                        code,
                        "expected a closing curly brace '}'",
                        &*code,
                        "here",
                    ));
                }
            } else if code.take_char_matches(':') {
                pieces.push(EvalStringPiece::Literal(":"));
            } else if code.take_char_matches('^') {
                // Starting with the yet unreleased, ninja 1.14
                pieces.push(EvalStringPiece::Literal("\n"));
            } else if let Some(name) = parse_varname(code, true) {
                pieces.push(EvalStringPiece::Var(name));
            } else {
                pieces.push(EvalStringPiece::Literal("$"));
                errors.push(new_parse_error(
                    code,
                    "bad $-escape (literal $ must be written as $$)",
                    piece_start.up_to(code),
                    "here",
                ));
            }
        } else if code.take_char_matches('\r') {
            errors.push(new_parse_error(
                code,
                "naked carridge returns aren't supported",
                &*code,
                "here",
            ));
        } else if code.is_empty() {
            if !path {
                errors.push(new_parse_error(code, "unexpected EOF", &*code, "here"));
            }
            break;
        } else {
            unreachable!();
        }
    }

    if path {
        eat_whitespace(code, Some(errors));
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
        let path = parse_eval_str(code, errors, true);
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
    const EMPTY: Self = Self {
        start_newlines: 0,
        comments: vec![],
    };

    #[inline]
    #[must_use]
    fn is_empty(&self) -> bool {
        self.start_newlines == 0 && self.comments.len() == 0
    }
}

fn parse_filler<'a>(code: &mut CodeIter<'a>, errors: &mut Vec<ParseError<'a>>) -> Filler<'a> {
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
                errors.push(new_parse_error(code, "expected newline", &*code, "here"));
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
        f.write_str(" = ")?;
        fmt::Display::fmt(&self.value, f)?;
        f.write_str("\n")?;
        Ok(())
    }
}

fn parse_let<'a>(code: &mut CodeIter<'a>, errors: &mut Vec<ParseError<'a>>) -> LetStmt<'a> {
    let var = parse_varname(code, false).unwrap_or_else(|| {
        errors.push(new_parse_error(
            code,
            "expected a variable name",
            &*code,
            "here",
        ));
        Varname::new_invalid_at(code)
    });
    eat_whitespace(code, Some(errors));
    if !code.take_char_matches('=') {
        errors.push(new_parse_error(
            code,
            "expected assignment operator '='",
            &*code,
            "here",
        ));
    }
    eat_whitespace(code, None);
    let value = parse_eval_str(code, errors, false);

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
    rule_span: Span,
) -> (RuleStmt<'a>, Filler<'a>) {
    eat_whitespace(code, Some(errors));
    let name = parse_varname(code, false).unwrap_or_else(|| {
        errors.push(new_parse_error(
            code,
            "expected a rule name",
            &*code,
            "here",
        ));
        Varname::new_invalid_at(code)
    });
    eat_whitespace(code, Some(errors));
    if !code.take_newline() {
        errors.push(new_parse_error(code, "expected newline", &*code, "here"));
        code.take_char_while(|ch| ch != '\n');
        code.take_newline();
    }

    let mut bindings = vec![];
    let filler = loop {
        let filler = parse_filler(code, errors);

        let indent_span = eat_whitespace(code, Some(errors));
        if indent_span.is_empty() {
            break filler;
        }

        let let_stmt = parse_let(code, errors);
        if !is_reserved_binding(&let_stmt.var.name) && let_stmt.var.is_valid() {
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
    };

    let rule_stmt = RuleStmt {
        rule_span,
        name,
        bindings,
    };

    (rule_stmt, filler)
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
    build_span: Span,
) -> (BuildStmt<'a>, Filler<'a>) {
    let outs = parse_paths(code, errors);
    let implicit_outs = match code.take_char_matches('|') {
        true => parse_paths(code, errors),
        false => vec![],
    };
    if outs.is_empty() && implicit_outs.is_empty() {
        errors.push(new_parse_error(
            code,
            "expected output path",
            &*code,
            "here",
        ));
    }
    if !code.take_char_matches(':') {
        errors.push(new_parse_error(code, "expected colon", &*code, "here"));
    }
    eat_whitespace(code, Some(errors));
    let rule = parse_varname(code, false).unwrap_or_else(|| {
        errors.push(new_parse_error(
            code,
            "expected a rule name",
            &*code,
            "here",
        ));
        Varname::new_invalid_at(code)
    });
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
            Some(':') => new_parse_error(code, "unexpected colon", code.up_to(&peek), "here"),
            _ => new_parse_error(code, "expected newline", &*code, "here"),
        });

        code.take_char_while(|ch| ch != '\n');
        code.take_newline();
    }

    let mut bindings = vec![];

    let filler = loop {
        let filler = parse_filler(code, errors);

        let indent_span = eat_whitespace(code, Some(errors));
        if indent_span.is_empty() {
            break filler;
        }

        let let_stmt = parse_let(code, errors);
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

    (build_stmt, filler)
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
            Some(':') => new_parse_error(code, "unexpected colon", code.up_to(&peek), "here"),
            Some('|') => new_parse_error(code, "unexpected pipe", code.up_to(&peek), "here"),
            _ => new_parse_error(code, "expected newline", &*code, "here"),
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
    pool_span: Span,
) -> (PoolStmt<'a>, Filler<'a>) {
    eat_whitespace(code, Some(errors));
    let name = parse_varname(code, false).unwrap_or_else(|| {
        errors.push(new_parse_error(
            code,
            "expected a pool name",
            &*code,
            "here",
        ));
        Varname::new_invalid_at(code)
    });
    eat_whitespace(code, Some(errors));
    if !code.take_newline() {
        errors.push(new_parse_error(code, "expected newline", &*code, "here"));

        code.take_char_while(|ch| ch != '\n');
        code.take_newline();
    }

    let mut bindings = vec![];

    let mut depth_defined = false;
    let filler = loop {
        let filler = parse_filler(code, errors);

        let indent_span = eat_whitespace(code, Some(errors));
        if indent_span.is_empty() {
            break filler;
        }

        let let_stmt = parse_let(code, errors);
        if let_stmt.var.name != "depth" && let_stmt.var.is_valid() {
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
                ),
        );
    }

    let pool_stmt = PoolStmt {
        pool_span,
        name,
        bindings,
    };

    (pool_stmt, filler)
}

#[derive(Debug)]
enum Item<'a> {
    Filler(Filler<'a>),
    Let(LetStmt<'a>),
    Rule(RuleStmt<'a>),
    Build(BuildStmt<'a>),
    Default(DefaultStmt<'a>),
    Pool(PoolStmt<'a>),
}

impl fmt::Display for Item<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Item::Filler(filler) => fmt::Display::fmt(filler, f),
            Item::Let(let_stmt) => fmt::Display::fmt(let_stmt, f),
            Item::Rule(rule_stmt) => fmt::Display::fmt(rule_stmt, f),
            Item::Build(build_stmt) => fmt::Display::fmt(build_stmt, f),
            Item::Default(default_stmt) => fmt::Display::fmt(default_stmt, f),
            Item::Pool(default_stmt) => fmt::Display::fmt(default_stmt, f),
        }
    }
}

fn parse_item<'a>(
    code: &mut CodeIter<'a>,
    errors: &mut Vec<ParseError<'a>>,
) -> (Item<'a>, Filler<'a>) {
    loop {
        let indent_span = eat_whitespace(code, Some(errors));

        if !indent_span.is_empty() {
            errors.push(new_parse_error(
                code,
                "unexpected indent",
                indent_span,
                "here",
            ));
        }

        if let Some(rule_span) = code.take_str_matches("rule") {
            let (rule_stmt, filler) = parse_rule(code, errors, rule_span);
            break (Item::Rule(rule_stmt), filler);
        } else if let Some(build_span) = code.take_str_matches("build") {
            let (build_stmt, filler) = parse_build(code, errors, build_span);
            break (Item::Build(build_stmt), filler);
        } else if let Some(default_stmt) = code.take_str_matches("default") {
            let default_stmt = parse_default(code, errors, default_stmt);
            break (Item::Default(default_stmt), parse_filler(code, errors));
        } else if let Some(pool_span) = code.take_str_matches("pool") {
            let (pool_stmt, filler) = parse_pool(code, errors, pool_span);
            break (Item::Pool(pool_stmt), filler);
        } else {
            let let_stmt = parse_let(code, errors);
            break (Item::Let(let_stmt), parse_filler(code, errors));
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

fn parse_file<'a>(code: &mut CodeIter<'a>, errors: &mut Vec<ParseError<'a>>) -> MexFile<'a> {
    let mut items = vec![];

    let filler = parse_filler(code, errors);
    if !filler.is_empty() {
        items.push(Item::Filler(filler));
    }

    while !code.is_empty() {
        let (item, filler) = parse_item(code, errors);
        items.push(item);
        if !filler.is_empty() {
            items.push(Item::Filler(filler));
        }
    }
    MexFile { items }
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

    // TODO: improve UTF-8 & \0 error handling
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
