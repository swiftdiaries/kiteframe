use super::{Diagnostic, Span};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TokenKind {
    Identifier(String),
    String(String),
    TripleString(String),
    Number(String),
    Arrow,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Equals,
    Question,
    Eof,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

pub(crate) fn lex(
    source: &str,
    max_tokens: usize,
    max_identifier_bytes: usize,
    max_diagnostics: usize,
) -> Result<Vec<Token>, Vec<Diagnostic>> {
    let mut lexer = Lexer {
        source,
        offset: 0,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
        max_tokens,
        max_identifier_bytes,
        max_diagnostics,
    };
    lexer.run();
    if lexer.diagnostics.is_empty() {
        Ok(lexer.tokens)
    } else {
        Err(lexer.diagnostics)
    }
}

struct Lexer<'a> {
    source: &'a str,
    offset: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
    max_tokens: usize,
    max_identifier_bytes: usize,
    max_diagnostics: usize,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.offset < self.source.len() && self.diagnostics.len() < self.max_diagnostics {
            self.skip_whitespace();
            if self.offset == self.source.len() {
                break;
            }
            let start = self.offset;
            let rest = &self.source[self.offset..];
            let token = if rest.starts_with("->") {
                self.offset += 2;
                Some(TokenKind::Arrow)
            } else if rest.starts_with("\"\"\"") {
                self.lex_triple_string(start)
            } else {
                let ch = rest.chars().next().expect("offset is in source");
                match ch {
                    '{' => {
                        self.offset += 1;
                        Some(TokenKind::LBrace)
                    }
                    '}' => {
                        self.offset += 1;
                        Some(TokenKind::RBrace)
                    }
                    '(' => {
                        self.offset += 1;
                        Some(TokenKind::LParen)
                    }
                    ')' => {
                        self.offset += 1;
                        Some(TokenKind::RParen)
                    }
                    '[' => {
                        self.offset += 1;
                        Some(TokenKind::LBracket)
                    }
                    ']' => {
                        self.offset += 1;
                        Some(TokenKind::RBracket)
                    }
                    ':' => {
                        self.offset += 1;
                        Some(TokenKind::Colon)
                    }
                    ',' => {
                        self.offset += 1;
                        Some(TokenKind::Comma)
                    }
                    '=' => {
                        self.offset += 1;
                        Some(TokenKind::Equals)
                    }
                    '?' => {
                        self.offset += 1;
                        Some(TokenKind::Question)
                    }
                    '"' => self.lex_quoted_string(start),
                    '-' if rest[1..]
                        .chars()
                        .next()
                        .is_some_and(|next| next.is_ascii_digit()) =>
                    {
                        Some(self.lex_number())
                    }
                    '0'..='9' => Some(self.lex_number()),
                    '_' | 'a'..='z' | 'A'..='Z' => Some(self.lex_identifier(start)),
                    _ => {
                        self.offset += ch.len_utf8();
                        self.error(
                            "unexpected character",
                            Span {
                                start,
                                end: self.offset,
                            },
                        );
                        None
                    }
                }
            };
            if let Some(kind) = token {
                self.push(
                    kind,
                    Span {
                        start,
                        end: self.offset,
                    },
                );
            }
        }
        if self.diagnostics.is_empty() {
            self.push(
                TokenKind::Eof,
                Span {
                    start: self.offset,
                    end: self.offset,
                },
            );
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.source[self.offset..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            self.offset += ch.len_utf8();
            if self.offset == self.source.len() {
                break;
            }
        }
    }

    fn lex_identifier(&mut self, start: usize) -> TokenKind {
        while let Some(ch) = self.source[self.offset..].chars().next() {
            if !(ch == '_' || ch.is_ascii_alphanumeric()) {
                break;
            }
            self.offset += ch.len_utf8();
            if self.offset == self.source.len() {
                break;
            }
        }
        let value = &self.source[start..self.offset];
        if value.len() > self.max_identifier_bytes {
            self.error(
                "identifier exceeds configured byte limit",
                Span {
                    start,
                    end: self.offset,
                },
            );
        }
        TokenKind::Identifier(value.to_owned())
    }

    fn lex_number(&mut self) -> TokenKind {
        let start = self.offset;
        if self.source[self.offset..].starts_with('-') {
            self.offset += 1;
        }
        while let Some(ch) = self.source[self.offset..].chars().next() {
            if !ch.is_ascii_digit() {
                break;
            }
            self.offset += 1;
            if self.offset == self.source.len() {
                break;
            }
        }
        if self.source[self.offset..].starts_with('.') {
            self.offset += 1;
            let fractional_start = self.offset;
            while let Some(ch) = self.source[self.offset..].chars().next() {
                if !ch.is_ascii_digit() {
                    break;
                }
                self.offset += 1;
                if self.offset == self.source.len() {
                    break;
                }
            }
            if self.offset == fractional_start {
                self.error(
                    "decimal literal requires digits after the decimal point",
                    Span {
                        start,
                        end: self.offset,
                    },
                );
            }
        }
        TokenKind::Number(self.source[start..self.offset].to_owned())
    }

    fn lex_triple_string(&mut self, start: usize) -> Option<TokenKind> {
        self.offset += 3;
        let content_start = self.offset;
        while self.offset < self.source.len() && !self.source[self.offset..].starts_with("\"\"\"") {
            let ch = self.source[self.offset..]
                .chars()
                .next()
                .expect("offset is in source");
            self.offset += ch.len_utf8();
        }
        if self.offset == self.source.len() {
            self.error(
                "unterminated triple-quoted prompt",
                Span {
                    start,
                    end: self.offset,
                },
            );
            return None;
        }
        let value = self.source[content_start..self.offset].to_owned();
        self.offset += 3;
        Some(TokenKind::TripleString(value))
    }

    fn lex_quoted_string(&mut self, start: usize) -> Option<TokenKind> {
        self.offset += 1;
        let mut value = String::new();
        while self.offset < self.source.len() {
            let ch = self.source[self.offset..]
                .chars()
                .next()
                .expect("offset is in source");
            self.offset += ch.len_utf8();
            match ch {
                '"' => return Some(TokenKind::String(value)),
                '\\' => {
                    let Some(escaped) = self.source[self.offset..].chars().next() else {
                        self.error(
                            "unterminated string escape",
                            Span {
                                start,
                                end: self.offset,
                            },
                        );
                        return None;
                    };
                    self.offset += escaped.len_utf8();
                    match escaped {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        _ => {
                            self.error(
                                "invalid string escape",
                                Span {
                                    start,
                                    end: self.offset,
                                },
                            );
                            return None;
                        }
                    }
                }
                '\n' | '\r' => {
                    self.error(
                        "unterminated string",
                        Span {
                            start,
                            end: self.offset,
                        },
                    );
                    return None;
                }
                _ => value.push(ch),
            }
        }
        self.error(
            "unterminated string",
            Span {
                start,
                end: self.offset,
            },
        );
        None
    }

    fn push(&mut self, kind: TokenKind, span: Span) {
        if self.tokens.len() >= self.max_tokens {
            self.error("token count exceeds configured limit", span);
        } else {
            self.tokens.push(Token { kind, span });
        }
    }
    fn error(&mut self, message: &str, span: Span) {
        if self.diagnostics.len() < self.max_diagnostics {
            self.diagnostics.push(Diagnostic::new(message, span));
        }
    }
}
