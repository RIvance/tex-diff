use super::SourceLocation;
use anyhow::{Context, Result, ensure};
use pest::{Parser as _, iterators::Pair};
use pest_derive::Parser;
use std::path::Path;

#[derive(Parser)]
#[grammar = "latex/latex.pest"]
struct LatexParser;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Command(String),
    Char(char),
    Space(bool),
    Open,
    Close,
    Math(bool),
    Verbatim,
}

#[derive(Clone, Debug)]
pub(super) struct Token {
    pub kind: Kind,
    pub raw: String,
    pub source: SourceLocation,
    pub base: Option<std::path::PathBuf>,
}

impl Token {
    pub fn command(&self, name: &str) -> bool {
        matches!(&self.kind, Kind::Command(n) if n == name)
    }

    pub fn is_word_command(&self) -> bool {
        matches!(
            &self.kind,
            Kind::Command(name) if name.chars().all(|c| c.is_ascii_alphabetic() || c == '@')
        )
    }

    pub fn is_space(&self) -> bool {
        matches!(self.kind, Kind::Space(_))
    }
}

pub(super) fn tokens(source: &str, path: &Path) -> Result<Vec<Token>> {
    parse(source, path, Rule::source, 1)
}

pub(super) fn package_tokens(source: &str, path: &Path) -> Result<Vec<Token>> {
    parse(source, path, Rule::package_source, 1)
}

pub(super) fn fragment_tokens(source: &str, path: &Path) -> Result<Vec<Token>> {
    // A fragment begins after a delimiter or a macro call, rather than at the
    // start of an otherwise empty physical line.
    parse(source, path, Rule::source, 2)
}

fn parse(source: &str, path: &Path, rule: Rule, initial_column: usize) -> Result<Vec<Token>> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let guard = LatexParser::parse(Rule::guard, source)?;
    let mut depth = 0usize;
    for pair in guard.flat_map(Pair::into_inner) {
        match pair.as_rule() {
            Rule::guard_open => {
                depth += 1;
                ensure!(
                    depth <= 128,
                    "{}: brace groups exceed 128 levels",
                    path.display()
                );
            }
            Rule::guard_close => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    let parsed = LatexParser::parse(rule, source)
        .with_context(|| format!("parsing LaTeX source {}", path.display()))?;

    struct Cursor<'a> {
        source: &'a str,
        offset: usize,
        line: usize,
        column: usize,
    }

    impl Cursor<'_> {
        fn location(&mut self, offset: usize, path: &Path) -> SourceLocation {
            for c in self.source[self.offset..offset].chars() {
                if c == '\n' {
                    self.line += 1;
                    self.column = 1;
                } else {
                    self.column += 1;
                }
            }
            self.offset = offset;
            SourceLocation {
                file: path.to_owned(),
                line: self.line,
                column: self.column,
            }
        }
    }

    fn emit(
        pair: Pair<'_, Rule>,
        path: &Path,
        depth: usize,
        cursor: &mut Cursor<'_>,
        out: &mut Vec<Token>,
    ) -> Result<()> {
        ensure!(
            depth <= 128,
            "{}: brace groups exceed 128 levels",
            path.display()
        );
        let span = pair.as_span();
        let source = cursor.location(span.start(), path);
        let rule = pair.as_rule();
        let raw = pair.as_str().to_owned();
        let kind = match rule {
            Rule::source | Rule::package_source | Rule::declaration => {
                for child in pair.into_inner() {
                    emit(child, path, depth, cursor, out)?;
                }
                return Ok(());
            }
            Rule::group | Rule::raw_group | Rule::raw_optional => {
                let end = span.end() - 1;
                let optional = rule == Rule::raw_optional;
                out.push(Token {
                    kind: if optional {
                        Kind::Char('[')
                    } else {
                        Kind::Open
                    },
                    raw: if optional { "[" } else { "{" }.into(),
                    source,
                    base: None,
                });
                for child in pair.into_inner() {
                    emit(child, path, depth + 1, cursor, out)?;
                }
                let closing = cursor.location(end, path);
                out.push(Token {
                    kind: if optional {
                        Kind::Char(']')
                    } else {
                        Kind::Close
                    },
                    raw: if optional { "]" } else { "}" }.into(),
                    source: closing,
                    base: None,
                });
                return Ok(());
            }
            Rule::comment | Rule::EOI => return Ok(()),
            // TeX ignores indentation at the start of a physical line. In
            // particular, a comment consumes its newline; retaining the next
            // line's indentation would add spaces to stored macro bodies.
            Rule::space if source.column == 1 && !raw.contains('\n') => return Ok(()),
            Rule::command
            | Rule::new_command_head
            | Rule::definition_head
            | Rule::environment_head => Kind::Command(raw[1..].to_owned()),
            Rule::space => Kind::Space(
                raw.chars().filter(|&c| c == '\n').count() >= 2
                    || source.column == 1 && raw.contains('\n'),
            ),
            Rule::character | Rule::raw_character | Rule::raw_star => {
                Kind::Char(raw.chars().next().expect("nonempty character pair"))
            }
            Rule::raw_open => Kind::Open,
            Rule::raw_close => Kind::Close,
            Rule::display_dollars | Rule::display_brackets => Kind::Math(true),
            Rule::inline_dollars | Rule::inline_parentheses => Kind::Math(false),
            Rule::inline_verbatim | Rule::verbatim_environment => Kind::Verbatim,
            _ => unreachable!("silent grammar rules do not emit pairs"),
        };
        out.push(Token {
            kind,
            raw,
            source,
            base: None,
        });
        Ok(())
    }
    let mut cursor = Cursor {
        source,
        offset: 0,
        line: 1,
        column: initial_column,
    };
    let mut out = Vec::new();
    for pair in parsed {
        emit(pair, path, 0, &mut cursor, &mut out)?;
    }
    Ok(out)
}

pub(super) fn math_parts(raw: &str) -> (&str, &str, &str) {
    let (open, close) = if raw.starts_with("$$") {
        ("$$", "$$")
    } else if raw.starts_with('$') {
        ("$", "$")
    } else if raw.starts_with("\\(") {
        ("\\(", "\\)")
    } else {
        ("\\[", "\\]")
    };
    (open, &raw[open.len()..raw.len() - close.len()], close)
}

pub(super) fn skip_space(tokens: &[Token], mut i: usize) -> usize {
    while tokens.get(i).is_some_and(Token::is_space) {
        i += 1;
    }
    i
}

pub(super) fn group(tokens: &[Token], i: usize) -> Option<(&[Token], usize)> {
    let start = skip_space(tokens, i);
    if tokens.get(start)?.kind != Kind::Open {
        return None;
    }
    let mut depth = 1;
    for (offset, token) in tokens[start + 1..].iter().enumerate() {
        match token.kind {
            Kind::Open => depth += 1,
            Kind::Close => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            let end = start + offset + 1;
            return Some((&tokens[start + 1..end], end + 1));
        }
    }
    None
}

pub(super) fn optional(tokens: &[Token], i: usize) -> Option<(&[Token], usize)> {
    let start = skip_space(tokens, i);
    if tokens.get(start)?.kind != Kind::Char('[') {
        return None;
    }
    let mut bracket = 1;
    let mut braces = 0;
    for (offset, token) in tokens[start + 1..].iter().enumerate() {
        match token.kind {
            Kind::Open => braces += 1,
            Kind::Close => braces -= 1,
            Kind::Char('[') if braces == 0 => bracket += 1,
            Kind::Char(']') if braces == 0 => bracket -= 1,
            _ => {}
        }
        if bracket == 0 {
            let end = start + offset + 1;
            return Some((&tokens[start + 1..end], end + 1));
        }
    }
    None
}

pub(super) fn source(tokens: &[Token]) -> String {
    let mut result = String::new();
    let mut word_command = false;
    let mut ordinary_space = false;
    for token in tokens {
        if token.kind == Kind::Space(false) && ordinary_space {
            // Adjacent end-of-line spaces from separate input files are still
            // spaces. Serializing both as newlines would manufacture a paragraph.
            continue;
        }
        if word_command
            && matches!(token.kind, Kind::Char(c) if c.is_ascii_alphabetic() || c == '@')
        {
            result.push(' ');
        }
        if token.kind == Kind::Space(true) && token.raw.matches('\n').count() < 2 {
            // A comment can consume the previous newline. Retain the following
            // physical blank line's paragraph meaning when removing comments.
            result.push_str("\n\n");
        } else {
            result.push_str(&token.raw);
        }
        word_command = token.is_word_command();
        ordinary_space = token.kind == Kind::Space(false);
    }
    result
}
