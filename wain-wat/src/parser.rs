use crate::lexer::{LexError, Lexer, NumBase, Sign, Token};
use std::char;
use std::fmt;
use std::mem;
use wain_ast::*;

#[cfg_attr(test, derive(Debug))]
pub enum ParseErrorKind<'a> {
    LexError(LexError<'a>),
    UnexpectedToken {
        got: Token<'a>,
        expected: &'static str, // TODO: Make 'expected' better
    },
    UnexpectedEndOfFile {
        expected: &'static str, // TODO: Make 'expected' better
    },
    UnexpectedKeyword(&'a str),
    InvalidValType(&'a str),
    InvalidStringFormat(&'static str),
    NumberMustBePositive(NumBase, &'a str),
}

#[cfg_attr(test, derive(Debug))]
pub struct ParseError<'a> {
    kind: ParseErrorKind<'a>,
    offset: usize,
    source: &'a str,
}

impl<'a> fmt::Display for ParseError<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ParseErrorKind::*;
        match &self.kind {
            LexError(err) => return err.fmt(f),
            UnexpectedToken { got, expected } => {
                write!(f, "expected {} but got {}", expected, got)?
            }
            UnexpectedEndOfFile { expected } => write!(f, "expected {} but reached EOF", expected)?,
            UnexpectedKeyword(kw) => write!(f, "unexpected keyword '{}'", kw)?,
            InvalidValType(ty) => write!(
                f,
                "value type must be one of 'i32', 'i64', 'f32', 'f64' but got '{}'",
                ty
            )?,
            InvalidStringFormat(reason) => {
                write!(f, "could not decode string literal: {}", reason)?
            }
            NumberMustBePositive(base, s) => {
                write!(f, "number must be positive but got -{}{}", base.prefix(), s)?
            }
        };

        let start = self.offset;
        let end = self.source[start..]
            .find(['\n', '\r'].as_ref())
            .unwrap_or_else(|| self.source.len());
        write!(
            f,
            " at byte offset {}\n\n ... {}\n     ^\n     start from here",
            self.offset,
            &self.source[start..end],
        )
    }
}

impl<'a> From<Box<LexError<'a>>> for Box<ParseError<'a>> {
    fn from(err: Box<LexError<'a>>) -> Box<ParseError<'a>> {
        Box::new(ParseError {
            source: err.source(),
            offset: err.offset(),
            kind: ParseErrorKind::LexError(*err),
        })
    }
}

type Result<'a, T> = ::std::result::Result<T, Box<ParseError<'a>>>;

// iter::Peekable is not sufficient to parse WAT tokens
// WAT requires LL(1) parser to see a token after '('
struct LookAhead<I: Iterator> {
    it: I,
    current: Option<I::Item>,
    incoming: Option<I::Item>,
}

impl<I: Iterator> LookAhead<I> {
    fn new(mut it: I) -> Self {
        let current = it.next();
        let incoming = it.next();
        LookAhead {
            it,
            current,
            incoming,
        }
    }
    fn peek(&self) -> Option<&I::Item> {
        self.current.as_ref()
    }
    fn lookahead(&self) -> Option<&I::Item> {
        self.incoming.as_ref()
    }
}

impl<I: Iterator> Iterator for LookAhead<I> {
    type Item = I::Item;

    fn next(&mut self) -> Option<Self::Item> {
        // This implementation can be better. We can delay calling self.it.next() until it is really
        // necessary. In the case, peek() and lookahead() will be `&mut self` method.
        mem::replace(
            &mut self.current,
            mem::replace(&mut self.incoming, self.it.next()),
        )
    }
}

// TODO: Add index-to-id tables for types, funcs, tables, mems, globals, locals and labels
// https://webassembly.github.io/spec/core/text/modules.html#indices
pub struct Parser<'a> {
    source: &'a str,
    tokens: LookAhead<Lexer<'a>>,
}

impl<'a> Parser<'a> {
    pub fn new(source: &'a str) -> Self {
        Parser {
            source,
            tokens: LookAhead::new(Lexer::new(source)),
        }
    }

    pub fn source(&self) -> &'a str {
        self.source
    }

    pub fn reset(&mut self) {
        self.tokens = LookAhead::new(Lexer::new(self.source));
    }

    pub fn is_done(&self) -> bool {
        self.tokens.peek().is_none()
    }

    fn parse<P: Parse<'a>>(&mut self) -> Result<'a, P> {
        Parse::<'a>::parse(self)
    }

    fn error<T>(&self, kind: ParseErrorKind<'a>, offset: usize) -> Result<'a, T> {
        Err(Box::new(ParseError {
            source: self.source,
            offset,
            kind,
        }))
    }

    fn unexpected_token<T>(
        &self,
        got: Token<'a>,
        expected: &'static str,
        offset: usize,
    ) -> Result<'a, T> {
        self.error(ParseErrorKind::UnexpectedToken { got, expected }, offset)
    }

    fn unexpected_eof<T>(&self, expected: &'static str) -> Result<'a, T> {
        self.error(
            ParseErrorKind::UnexpectedEndOfFile { expected },
            self.source().len(),
        )
    }

    fn invalid_utf8_char<T>(&self, offset: usize) -> Result<'a, T> {
        self.error(
            ParseErrorKind::InvalidStringFormat(
                "UTF-8 character must be in format u{hexnum} like u{e38182} in range of hexnum < 0xd800 or 0xe0000 <= hexnum < 0x110000",
            ),
            offset,
        )
    }

    fn invalid_char_escape<T>(&self, offset: usize) -> Result<'a, T> {
        self.error(
            ParseErrorKind::InvalidStringFormat(
                r#"escape must be one of \t, \n, \r, \", \', \\, \u{hexnum}, \MN"#,
            ),
            offset,
        )
    }

    fn maybe_eof<'p>(
        &'p self,
        lexed: Option<&'p <Lexer<'a> as Iterator>::Item>,
        expected: &'static str,
    ) -> Result<'a, (&Token<'a>, usize)> {
        match lexed {
            Some(Ok((tok, offset))) => Ok((tok, *offset)),
            Some(Err(err)) => Err(err.clone().into()),
            None => self.unexpected_eof(expected),
        }
    }

    fn lookahead(&self, expected: &'static str) -> Result<'a, (&Token<'a>, usize)> {
        self.maybe_eof(self.tokens.lookahead(), expected)
    }

    fn peek(&self, expected: &'static str) -> Result<'a, (&Token<'a>, usize)> {
        self.maybe_eof(self.tokens.peek(), expected)
    }

    fn next_token(&mut self, expected: &'static str) -> Result<'a, (Token<'a>, usize)> {
        match self.tokens.next() {
            Some(lexed) => Ok(lexed?),
            None => self.unexpected_eof(expected),
        }
    }

    fn eat_token(&mut self) -> bool {
        self.tokens.next().is_some()
    }

    fn lookahead_keyword(&self, expected: &'static str) -> Result<'a, (&'a str, usize)> {
        match self.lookahead(expected)? {
            (Token::Keyword(kw), offset) => Ok((*kw, offset)),
            (tok, offset) => self.unexpected_token(tok.clone(), expected, offset),
        }
    }

    fn maybe_ident(&mut self, expected: &'static str) -> Result<'a, Option<&'a str>> {
        Ok(match self.peek(expected)? {
            (Token::Ident(id), _) => {
                let id = *id;
                self.eat_token();
                Some(id)
            }
            _ => None,
        })
    }
}

macro_rules! match_token {
    ($parser:ident, $expected:expr, $pattern:pat => $ret:expr) => {
        match $parser.next_token($expected)? {
            ($pattern, offset) => ($ret, offset),
            (tok, offset) => {
                return $parser.unexpected_token(tok, $expected, offset);
            }
        }
    };
    ($parser:ident, $expected:expr, $pattern:pat) => {
        match_token!($parser, $expected, $pattern => ())
    };
}

pub trait Parse<'a>: Sized {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self>;
}

// https://webassembly.github.io/spec/core/text/modules.html
impl<'a> Parse<'a> for SyntaxTree<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let mut module: Module<'a> = parser.parse()?;

        // The following restrictions are imposed on the composition of modules: `m1 ⊕ m2` is
        // defined if and only if
        //
        // - m1.start = ϵ ∨ m2.start = ϵ
        // - m1.funcs = m1.tables = m1.mems = m1.mems = ϵ ∨ m2.imports = ϵ
        while !parser.is_done() {
            let another: Module<'a> = parser.parse()?;
            // TODO: Check above restrictions
            for ty in another.types {
                module.types.push(ty);
                // TODO: Merge more fields
            }
        }

        Ok(SyntaxTree { module })
    }
}

// https://webassembly.github.io/spec/core/text/modules.html#text-module
impl<'a> Parse<'a> for Module<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        // https://webassembly.github.io/spec/core/text/modules.html#text-module
        let (_, start) = match_token!(parser, "opening paren for module", Token::LParen);
        match_token!(parser, "'module' keyword", Token::Keyword("module"));
        let (ident, _) = match_token!(parser, "identifier for module", Token::Ident(id) => id);

        let mut types = vec![];
        let mut imports = vec![];

        while let (Token::LParen, _) = parser.peek("opening paren for starting module field")? {
            match parser.parse()? {
                ModuleField::Type(ty) => types.push(ty),
                ModuleField::Import(import) => imports.push(import),
                // TODO: Add more fields
            }
        }

        match_token!(parser, "closing paren for module", Token::RParen);
        Ok(Module {
            start,
            ident,
            types,
            imports,
        })
    }
}

// https://webassembly.github.io/spec/core/text/modules.html#text-modulefield
impl<'a> Parse<'a> for ModuleField<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (keyword, offset) = parser.lookahead_keyword("keyword for module field")?;
        match keyword {
            "type" => Ok(ModuleField::Type(parser.parse()?)),
            "import" => Ok(ModuleField::Import(parser.parse()?)),
            // TODO: Add more fields
            kw => parser.error(ParseErrorKind::UnexpectedKeyword(kw), offset),
        }
    }
}

// https://webassembly.github.io/spec/core/text/modules.html#text-typedef
impl<'a> Parse<'a> for TypeDef<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (_, start) = match_token!(parser, "opening paren for type", Token::LParen);
        match_token!(parser, "'type' keyword", Token::Keyword("type"));
        let id = parser.maybe_ident("identifier for type")?;
        let ty = parser.parse()?;
        match_token!(parser, "closing paren for type", Token::RParen);
        Ok(TypeDef { start, id, ty })
    }
}

// https://webassembly.github.io/spec/core/text/types.html#text-functype
impl<'a> Parse<'a> for FuncType<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (_, start) = match_token!(parser, "opening paren for functype", Token::LParen);
        match_token!(parser, "'func' keyword", Token::Keyword("func"));

        let mut params = vec![];
        loop {
            match parser.peek("opening paren for param in functype")? {
                (Token::LParen, _)
                    if parser.lookahead_keyword("param keyword in functype")?.0 == "param" =>
                {
                    params.push(parser.parse()?);
                }
                _ => break,
            }
        }

        let mut results = vec![];
        while let (Token::LParen, _) = parser.peek("opening paren for result in functype")? {
            results.push(parser.parse()?);
        }

        match_token!(parser, "closing paren for functype", Token::RParen);
        Ok(FuncType {
            start,
            params,
            results,
        })
    }
}

// https://webassembly.github.io/spec/core/text/types.html#text-param
impl<'a> Parse<'a> for Param<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (_, start) = match_token!(parser, "opening paren for param", Token::LParen);
        match_token!(parser, "'param' keyword", Token::Keyword("param"));
        let id = parser.maybe_ident("identifier for param")?;
        let ty = parser.parse()?;
        match_token!(parser, "closing paren for param", Token::RParen);
        Ok(Param { start, id, ty })
    }
}

// https://webassembly.github.io/spec/core/text/types.html#text-valtype
impl<'a> Parse<'a> for ValType {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let expected = "keyword for value type";
        match parser.next_token(expected)? {
            (Token::Keyword("i32"), _) => Ok(ValType::I32),
            (Token::Keyword("i64"), _) => Ok(ValType::I64),
            (Token::Keyword("f32"), _) => Ok(ValType::F32),
            (Token::Keyword("f64"), _) => Ok(ValType::F64),
            (Token::Keyword(id), offset) => {
                parser.error(ParseErrorKind::InvalidValType(id), offset)
            }
            (tok, offset) => parser.unexpected_token(tok, expected, offset),
        }
    }
}

// https://webassembly.github.io/spec/core/text/types.html#text-result
impl<'a> Parse<'a> for FuncResult {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (_, start) = match_token!(
            parser,
            "opening paren for function result type",
            Token::LParen
        );
        match_token!(parser, "'result' keyword", Token::Keyword("result"));
        let ty = parser.parse()?;
        match_token!(
            parser,
            "closing paren for function result type",
            Token::RParen
        );
        Ok(FuncResult { start, ty })
    }
}

// https://webassembly.github.io/spec/core/text/modules.html#text-import
impl<'a> Parse<'a> for Import<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (_, start) = match_token!(
            parser,
            "opening paren for import description",
            Token::LParen
        );
        match_token!(parser, "'import' keyword", Token::Keyword("import"));
        let mod_name = parser.parse()?;
        let name = parser.parse()?;
        let desc = parser.parse()?;
        match_token!(
            parser,
            "closing paren for import description",
            Token::RParen
        );
        Ok(Import {
            start,
            mod_name,
            name,
            desc,
        })
    }
}

// https://webassembly.github.io/spec/core/text/values.html#text-name
impl<'a> Parse<'a> for Name {
    // TODO: Move this logic to parser as encoding literal into Vec<u8> then use it in this method
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (src, offset) = match_token!(parser, "string literal", Token::String(s) => s);

        // A name string must form a valid UTF-8 encoding as defined by Unicode (Section 2.5)
        let mut name = String::new();
        let mut chars = src.char_indices();
        for (i, c) in chars.next() {
            if c != '\\' {
                name.push(c);
            } else {
                // Note: Lexer guarantees that at least one char follows after '\'
                match chars.next().unwrap().1 {
                    't' => name.push('\t'),
                    'n' => name.push('\n'),
                    'r' => name.push('\r'),
                    '"' => name.push('"'),
                    '\'' => name.push('\''),
                    '\\' => name.push('\\'),
                    'u' => {
                        match chars.next() {
                            Some((i, '{')) => {
                                let start = i + 1; // next to '{'
                                let end = loop {
                                    match chars.next() {
                                        Some((i, '}')) => break i,
                                        Some(_) => continue,
                                        None => return parser.invalid_utf8_char(offset + i),
                                    }
                                };
                                if let Some(c) = u32::from_str_radix(&src[start..end], 16)
                                    .ok()
                                    .and_then(char::from_u32)
                                {
                                    name.push(c);
                                } else {
                                    return parser.invalid_utf8_char(offset + i);
                                }
                            }
                            _ => return parser.invalid_utf8_char(offset + i),
                        }
                    }
                    c => {
                        let hi = c.to_digit(16);
                        let lo = chars.next().and_then(|(_, c)| c.to_digit(16));
                        match (hi, lo) {
                            (Some(hi), Some(lo)) => match char::from_u32(hi * 16 + lo) {
                                Some(c) => name.push(c),
                                None => return parser.invalid_char_escape(offset + i),
                            },
                            _ => return parser.invalid_char_escape(offset + i),
                        }
                    }
                }
            }
        }
        Ok(Name(name))
    }
}

// https://webassembly.github.io/spec/core/text/modules.html#text-importdesc
impl<'a> Parse<'a> for ImportDesc<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (_, start) = match_token!(
            parser,
            "opening paren for import description",
            Token::LParen
        );
        let id = parser.maybe_ident("identifier for import description")?;
        let (keyword, offset) = match_token!(parser, "one of 'func', 'table', 'memory', 'global'", Token::Keyword(kw) => kw);
        let desc = match keyword {
            "func" => ImportDesc::Func {
                start,
                id,
                ty: parser.parse()?,
            },
            // TODO: Add table, memory, global
            kw => return parser.error(ParseErrorKind::UnexpectedKeyword(kw), offset),
        };
        match_token!(
            parser,
            "closing paren for import description",
            Token::RParen
        );
        Ok(desc)
    }
}

// https://webassembly.github.io/spec/core/text/modules.html#type-uses
impl<'a> Parse<'a> for TypeUse<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let (_, start) = match_token!(parser, "opening paren for typeuse", Token::LParen);
        match_token!(parser, "'type' keyword for typeuse", Token::Keyword("type"));

        let idx = parser.parse()?;

        match_token!(parser, "closing paren for typeuse", Token::RParen);

        let mut params = vec![];
        loop {
            match parser.peek("opening paren for param in typeuse")? {
                (Token::LParen, _)
                    if parser.lookahead_keyword("param keyword in typeuse")?.0 == "param" =>
                {
                    params.push(parser.parse()?);
                }
                _ => break,
            }
        }

        let mut results = vec![];
        while let (Token::LParen, _) = parser.peek("opening paren for result in typeuse")? {
            results.push(parser.parse()?);
        }

        Ok(TypeUse {
            start,
            idx,
            params,
            results,
        })
    }
}

// https://webassembly.github.io/spec/core/text/modules.html#indices
impl<'a> Parse<'a> for Index<'a> {
    fn parse(parser: &mut Parser<'a>) -> Result<'a, Self> {
        let expected = "number or identifier for index";
        match parser.next_token(expected)? {
            (Token::Int(sign, base, s), offset) if sign == Sign::Minus => {
                parser.error(ParseErrorKind::NumberMustBePositive(base, s), offset)
            }
            (Token::Int(_, base, s), _) => {
                let u = u32::from_str_radix(s, base.radix()).unwrap();
                Ok(Index::Num(u))
            }
            (Token::Ident(id), _) => Ok(Index::Ident(id)),
            (tok, offset) => parser.unexpected_token(tok.clone(), expected, offset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookahead() {
        let v = vec![1, 2, 3, 4];
        let mut i = LookAhead::new(v.into_iter());
        assert_eq!(i.peek(), Some(&1));
        assert_eq!(i.lookahead(), Some(&2));
        assert_eq!(i.next(), Some(1));

        assert_eq!(i.peek(), Some(&2));
        assert_eq!(i.lookahead(), Some(&3));
        assert_eq!(i.next(), Some(2));

        assert_eq!(i.peek(), Some(&3));
        assert_eq!(i.lookahead(), Some(&4));
        assert_eq!(i.next(), Some(3));

        assert_eq!(i.peek(), Some(&4));
        assert_eq!(i.lookahead(), None);
        assert_eq!(i.next(), Some(4));

        assert_eq!(i.peek(), None);
        assert_eq!(i.lookahead(), None);
        assert_eq!(i.next(), None);

        assert_eq!(i.peek(), None);
        assert_eq!(i.lookahead(), None);

        let v: Vec<i32> = vec![];
        let mut i = LookAhead::new(v.into_iter());
        assert_eq!(i.peek(), None);
        assert_eq!(i.lookahead(), None);
        assert_eq!(i.next(), None);
        assert_eq!(i.peek(), None);
        assert_eq!(i.lookahead(), None);
    }

    macro_rules! assert_parse {
        ($input:expr, $node:ty, $expect:pat if $cond:expr) => {
            let input = $input;
            let mut parser = Parser::new(input);
            let node: $node = parser.parse().unwrap();
            match node {
                $expect if $cond => { /* OK */ }
                _ => panic!(
                    "assertion failed: {:?} did not match to {}",
                    node,
                    stringify!($expect if $cond)
                ),
            }
            assert!(parser.is_done(), "{:?}", parser.tokens.collect::<Vec<_>>());
        };
        ($input:expr, $node:ty, $expect:pat) => {
            assert_parse!($input, $node, $expect if true);
        };
    }

    macro_rules! assert_error {
        ($input:expr, $node:ty, $expect:pat if $cond:expr) => {{
            use ParseErrorKind::*;
            let input = $input;
            match Parser::new(input).parse::<$node>().unwrap_err().kind {
                $expect if $cond => { /* OK */ }
                err => panic!("assertion failed: {:?} did not match to pattern {}", err, stringify!($expect if $cond)),
            }
        }};
        ($input:expr, $node:ty, $expect:pat) => {
            assert_error!($input, $node, $expect if true);
        };
    }

    #[test]
    fn root() {
        assert_parse!(r#"
            (module $foo)
            (module $foo)
            (module $foo)
        "#, SyntaxTree<'_>, SyntaxTree{ module: Module { ident: "$foo", types, .. } } if types.is_empty());
        assert_parse!(r#"
            (module $foo (type $f1 (func)))
            (module $foo (type $f1 (func)))
            (module $foo (type $f1 (func)))
        "#, SyntaxTree<'_>, SyntaxTree{ module: Module { ident: "$foo", types, .. } } if types.len() == 3);
    }

    #[test]
    fn module() {
        assert_parse!(r#"(module $foo)"#, Module<'_>, Module { ident: "$foo", types, .. } if types.is_empty());
        assert_parse!(r#"(module $foo (type $f1 (func)))"#, Module<'_>, Module { ident: "$foo", types, .. } if types.len() == 1);
        assert_parse!(r#"
            (module $foo
                (type $f1 (func))
                (type $f1 (func)))
        "#, Module<'_>, Module { ident: "$foo", types, .. } if types.len() == 2);

        assert_error!(r#"module"#, Module<'_>, UnexpectedToken{ expected: "opening paren for module", ..});
        assert_error!(r#"(module )"#, Module<'_>, UnexpectedToken{ expected: "identifier for module", ..});
        assert_error!(r#"(module"#, Module<'_>, UnexpectedEndOfFile{..});
        assert_error!(r#"(module $foo (type $f (func))"#, Module<'_>, UnexpectedEndOfFile{..});
    }

    #[test]
    fn module_field() {
        assert_parse!(
            r#"(type $f1 (func))"#,
            ModuleField<'_>,
            ModuleField::Type(..)
        );

        assert_error!(r#"((type $f1 (func)))"#, ModuleField<'_>, UnexpectedToken{ expected: "keyword for module field", ..});
        assert_error!(r#"(hello!)"#, ModuleField<'_>, UnexpectedKeyword("hello!"));
    }

    #[test]
    fn type_def() {
        assert_parse!(r#"(type $f1 (func))"#, TypeDef<'_>, TypeDef{ id: Some("$f1"), .. });
        assert_parse!(r#"(type (func))"#, TypeDef<'_>, TypeDef{ id: None, .. });

        assert_error!(r#"(type (func) (func))"#, TypeDef<'_>, UnexpectedToken { expected: "closing paren for type", .. });
        assert_error!(r#"(type)"#, TypeDef<'_>, UnexpectedToken { expected: "opening paren for functype", .. });
        assert_error!(r#"(type"#, TypeDef<'_>, UnexpectedEndOfFile { .. });
    }

    #[test]
    fn func_type() {
        assert_parse!(r#"(func)"#, FuncType<'_>, FuncType{ params, results, .. } if params.is_empty() && results.is_empty());
        assert_parse!(r#"(func (param $a i32) (result i32))"#, FuncType<'_>, FuncType{ params, results, .. } if params.len() == 1 && results.len() == 1);
        assert_parse!(r#"(func (param $a i32))"#, FuncType<'_>, FuncType{ params, results, .. } if params.len() == 1 && results.is_empty());
        assert_parse!(r#"(func (result i32))"#, FuncType<'_>, FuncType{ params, results, .. } if params.is_empty() && results.len() == 1);
        assert_parse!(r#"
            (func
                (param $a i32)
                (param $a i32)
                (result i32)
                (result i32))
        "#, FuncType<'_>, FuncType{ params, results, .. } if params.len() == 2 && results.len() == 2);

        assert_error!(r#"func"#, FuncType<'_>, UnexpectedToken{ expected: "opening paren for functype", .. });
        assert_error!(r#"(type"#, FuncType<'_>, UnexpectedToken{ expected: "'func' keyword", .. });
        assert_error!(r#"(func "#, FuncType<'_>, UnexpectedEndOfFile{ expected: "opening paren for param in functype", .. });
        assert_error!(r#"(func (result i32)"#, FuncType<'_>, UnexpectedEndOfFile{ expected: "opening paren for result in functype", .. });
        assert_error!(r#"(func (result i32) foo"#, FuncType<'_>, UnexpectedToken{ expected: "closing paren for functype", .. });
    }

    #[test]
    fn param() {
        assert_parse!(r#"(param $a i32)"#, Param<'_>, Param { id: Some("$a"), .. });
        assert_parse!(r#"(param i32)"#, Param<'_>, Param { id: None, .. });

        assert_error!(r#"param"#, Param<'_>, UnexpectedToken{ expected: "opening paren for param", .. });
        assert_error!(r#"(module i32)"#, Param<'_>, UnexpectedToken{ expected: "'param' keyword", .. });
        assert_error!(r#"(param)"#, Param<'_>, UnexpectedToken{ .. });
        assert_error!(r#"(param i32 i64)"#, Param<'_>, UnexpectedToken{ expected: "closing paren for param", .. });
        assert_error!(r#"(param i32"#, Param<'_>, UnexpectedEndOfFile{ expected: "closing paren for param", .. });
    }

    #[test]
    fn value_type() {
        assert_parse!(r#"i32"#, ValType, ValType::I32);
        assert_parse!(r#"i64"#, ValType, ValType::I64);
        assert_parse!(r#"f32"#, ValType, ValType::F32);
        assert_parse!(r#"f64"#, ValType, ValType::F64);

        assert_error!(r#"string"#, ValType, InvalidValType("string"));
        assert_error!(r#"$hello"#, ValType, UnexpectedToken{ expected: "keyword for value type", .. });
    }

    #[test]
    fn func_result() {
        assert_parse!(r#"(result i32)"#, FuncResult, FuncResult { .. });

        assert_error!(r#"result"#, FuncResult, UnexpectedToken { expected: "opening paren for function result type", .. });
        assert_error!(r#"(hello"#, FuncResult, UnexpectedToken { expected: "'result' keyword", .. });
        assert_error!(r#"(result)"#, FuncResult, UnexpectedToken { .. });
        assert_error!(r#"(result i32"#, FuncResult, UnexpectedEndOfFile { expected: "closing paren for function result type", .. });
    }
}
