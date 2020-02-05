// MIT License
//
// Copyright (c) 2019 Alasdair Armstrong
//
// Permission is hereby granted, free of charge, to any person
// obtaining a copy of this software and associated documentation
// files (the "Software"), to deal in the Software without
// restriction, including without limitation the rights to use, copy,
// modify, merge, publish, distribute, sublicense, and/or sell copies
// of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be
// included in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
// NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS
// BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
// ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
// CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use regex::Regex;
use std::fmt;

pub struct LexError {
    pub pos: usize,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Lex error at position: {}", self.pos)
    }
}

pub struct Keyword {
    word: &'static str,
    token: Tok<'static>,
    len: usize,
}

impl Keyword {
    pub fn new(kw: &'static str, tok: Tok<'static>) -> Self {
        Keyword { word: kw, token: tok, len: kw.len() }
    }
}

pub struct Lexer<'input> {
    pub buf: &'input str,
    pub pos: usize,
}

impl<'input> Lexer<'input> {
    pub fn new(input: &'input str) -> Self {
        Lexer { buf: input, pos: 0 }
    }
}

impl<'input> Lexer<'input> {
    pub fn consume_whitespace(&mut self) -> Option<()> {
        loop {
            if self.buf.chars().next()?.is_whitespace() {
                self.pos += 1;
                self.buf = &self.buf[1..]
            } else {
                break Some(());
            }
        }
    }

    pub fn consume_regex(&mut self, r: &Regex) -> Option<(usize, &'input str, usize)> {
        match r.find(&self.buf) {
            None => None,
            Some(mat) => {
                let start_pos = self.pos;
                self.pos += mat.end();
                self.buf = &self.buf[mat.end()..];
                Some((start_pos, mat.as_str(), self.pos))
            }
        }
    }

    pub fn consume_string_literal(&mut self) -> Option<(usize, &'input str, usize)> {
        if self.buf.chars().next()? == '\"' {
            let mut string_end = 1;
            loop {
                if let '\"' = self.buf.chars().nth(string_end)? {
                    let contents = &self.buf[1..string_end];
                    let start_pos = self.pos;
                    self.pos += string_end + 1;
                    self.buf = &self.buf[(string_end + 1)..];
                    break Some((start_pos, &contents, self.pos));
                }
                string_end += 1
            }
        } else {
            None
        }
    }

    pub fn consume_comment(&mut self) -> bool {
        loop {
            if self.buf.is_empty() {
                break false;
            } else if self.buf.starts_with("*)") {
                self.pos += 2;
                self.buf = &self.buf[2..];
                break true;
            } else {
                self.pos += 1;
                self.buf = &self.buf[1..];
            }
        }
    }

    pub fn consume_line_comment(&mut self) {
        loop {
            match self.buf.chars().next() {
                Some(c) if c == '\n' => {
                    self.pos += 1;
                    self.buf = &self.buf[1..];
                    break;
                }
                Some(_) => {
                    self.pos += 1;
                    self.buf = &self.buf[1..];
                }
                None => (),
            }
        }
    }
}

macro_rules! lex_regex {
    ($lexer: ident, $token: path, $regex: expr) => {
        match $lexer.consume_regex(&$regex) {
            None => (),
            Some((from, id, to)) => return Some(Ok((from, $token(id), to))),
        }
    };
}

macro_rules! lex_keyword {
    ($lexer: ident, $keyword: expr) => {
        if $lexer.buf.starts_with($keyword.word) {
            match $lexer.buf.chars().nth($keyword.len) {
                // A keyword cannot be immediately followed by any valid identifier characters
                Some(c) if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' => (),
                _ => {
                    let start_pos = $lexer.pos;
                    $lexer.pos += $keyword.len;
                    $lexer.buf = &$lexer.buf[$keyword.len..];
                    return Some(Ok((start_pos, $keyword.token.clone(), $lexer.pos)));
                }
            }
        }
    };
}

macro_rules! lex_char {
    ($lexer: ident, $next: ident, $token: path, $char: expr) => {
        if $next == $char {
            let start_pos = $lexer.pos;
            $lexer.pos += 1;
            $lexer.buf = &$lexer.buf[1..];
            return Some(Ok((start_pos, $token.clone(), $lexer.pos)));
        }
    };
}

#[derive(Clone, Debug)]
pub enum Tok<'input> {
    Id(&'input str),
    String(&'input str),
    IslaSpecialCos,
    // Keywords
    Acyclic,
    As,
    Empty,
    Flag,
    In,
    Include,
    Inverse,
    HatPlus,
    HatStar,
    Irreflexive,
    Let,
    And,
    Rec,
    Try,
    With,
    Show,
    Unshow,
    PlusPlus,
    // Symbols
    Zero,
    Eq,
    Plus,
    Bar,
    Tilde,
    Amp,
    Star,
    SemiColon,
    Backslash,
    Comma,
    Question,
    // Brackets
    Lparen,
    Rparen,
    Lbrace,
    Rbrace,
    Lsquare,
    Rsquare,
}

impl<'input> fmt::Display for Tok<'input> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

lazy_static! {
    pub static ref CAT_ID_REGEX: Regex = Regex::new(r"^[a-zA-Z_][0-9a-zA-Z_.-]*").unwrap();
    pub static ref KW_ACYCLIC: Keyword = Keyword::new("acyclic", Tok::Acyclic);
    pub static ref KW_AS: Keyword = Keyword::new("as", Tok::As);
    pub static ref KW_EMPTY: Keyword = Keyword::new("empty", Tok::Empty);
    pub static ref KW_FLAG: Keyword = Keyword::new("flag", Tok::Flag);
    pub static ref KW_IN: Keyword = Keyword::new("in", Tok::In);
    pub static ref KW_INCLUDE: Keyword = Keyword::new("include", Tok::Include);
    pub static ref KW_IRREFLEXIVE: Keyword = Keyword::new("irreflexive", Tok::Irreflexive);
    pub static ref KW_ISLA_SPECIAL_COS: Keyword = Keyword::new("isla_special_cos", Tok::IslaSpecialCos);
    pub static ref KW_LET: Keyword = Keyword::new("let", Tok::Let);
    pub static ref KW_REC: Keyword = Keyword::new("rec", Tok::Rec);
    pub static ref KW_AND: Keyword = Keyword::new("and", Tok::And);
    pub static ref KW_TRY: Keyword = Keyword::new("try", Tok::Try);
    pub static ref KW_WITH: Keyword = Keyword::new("with", Tok::With);
    pub static ref KW_SHOW: Keyword = Keyword::new("show", Tok::Show);
    pub static ref KW_UNSHOW: Keyword = Keyword::new("unshow", Tok::Unshow);
    pub static ref KW_INVERSE: Keyword = Keyword::new("^-1", Tok::Inverse);
    pub static ref KW_HATPLUS: Keyword = Keyword::new("^+", Tok::HatPlus);
    pub static ref KW_HATSTAR: Keyword = Keyword::new("^*", Tok::HatStar);
    pub static ref KW_PLUS_PLUS: Keyword = Keyword::new("++", Tok::PlusPlus);
}

pub type Span<'input> = Result<(usize, Tok<'input>, usize), LexError>;

impl<'input> Iterator for Lexer<'input> {
    type Item = Span<'input>;

    #[allow(clippy::cognitive_complexity)]
    fn next(&mut self) -> Option<Self::Item> {
        use Tok::*;
        self.consume_whitespace()?;

        let next = self.buf.chars().next()?;

        if next == 'a' {
            lex_keyword!(self, KW_ACYCLIC);
            lex_keyword!(self, KW_AS);
            lex_keyword!(self, KW_AND);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 'e' {
            lex_keyword!(self, KW_EMPTY);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 'f' {
            lex_keyword!(self, KW_FLAG);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 'i' {
            lex_keyword!(self, KW_INCLUDE);
            lex_keyword!(self, KW_IN);
            lex_keyword!(self, KW_IRREFLEXIVE);
            lex_keyword!(self, KW_ISLA_SPECIAL_COS);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 'l' {
            lex_keyword!(self, KW_LET);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 'r' {
            lex_keyword!(self, KW_REC);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 's' {
            lex_keyword!(self, KW_SHOW);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 't' {
            lex_keyword!(self, KW_TRY);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 'u' {
            lex_keyword!(self, KW_UNSHOW);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == 'w' {
            lex_keyword!(self, KW_WITH);
            lex_regex!(self, Id, CAT_ID_REGEX)
        } else if next == '+' {
            lex_keyword!(self, KW_PLUS_PLUS);
            lex_char!(self, next, Tok::Plus, '+');
        } else if next == '^' {
            lex_keyword!(self, KW_INVERSE);
            lex_keyword!(self, KW_HATPLUS);
            lex_keyword!(self, KW_HATSTAR);
        } else if next == '"' {
            match self.consume_string_literal() {
                None => (),
                Some((from, s, to)) => {
                    return Some(Ok((from, String(s), to)));
                }
            }
        } else if next == '#' {
            self.consume_line_comment();
            return self.next();
        } else if next == '(' {
            if let Some('*') = self.buf.chars().nth(1) {
                if self.consume_comment() {
                    return self.next();
                }
            } else {
                lex_char!(self, next, Tok::Lparen, '(');
            }
        } else {
            lex_char!(self, next, Tok::Eq, '=');
            lex_char!(self, next, Tok::Rparen, ')');
            lex_char!(self, next, Tok::Lbrace, '{');
            lex_char!(self, next, Tok::Rbrace, '}');
            lex_char!(self, next, Tok::Lbrace, '{');
            lex_char!(self, next, Tok::Rbrace, '}');
            lex_char!(self, next, Tok::Lsquare, '[');
            lex_char!(self, next, Tok::Rsquare, ']');
            lex_char!(self, next, Tok::Tilde, '~');
            lex_char!(self, next, Tok::Bar, '|');
            lex_char!(self, next, Tok::Bar, '∪');
            lex_char!(self, next, Tok::Amp, '&');
            lex_char!(self, next, Tok::Amp, '∩');
            lex_char!(self, next, Tok::Star, '*');
            lex_char!(self, next, Tok::Star, '×');
            lex_char!(self, next, Tok::SemiColon, ';');
            lex_char!(self, next, Tok::Comma, ',');
            lex_char!(self, next, Tok::Backslash, '\\');
            lex_char!(self, next, Tok::Zero, '0');
            lex_char!(self, next, Tok::Question, '?');
            lex_regex!(self, Id, CAT_ID_REGEX)
        }

        Some(Err(LexError { pos: self.pos }))
    }
}
