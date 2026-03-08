//! Parser for AgentCodeRepo canonical type syntax.
//!
//! Parses the text form produced by [`Ty::fmt`](crate::ty::Ty) back into
//! a [`Ty`] AST, enabling `Display` -> `parse` roundtrips.
//!
//! # Grammar (informal)
//!
//! ```text
//! ty         = 'forall' bindings '.' [constraints '=>'] ty
//!            | fun_ty
//! fun_ty     = app_ty '->' fun_ty              (pure, right-assoc)
//!            | app_ty '->{' effects '}' fun_ty  (effectful)
//!            | app_ty
//! app_ty     = atom+                            (left-assoc application)
//! atom       = PRIM | LOWER_IDENT | UPPER_IDENT
//!            | '(' ty ')'  | '(' ty ',' ty_list ')'  | '()'
//!            | '{' fields '}'  | '<' variants '>'
//! ```

use crate::ty::*;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, thiserror::Error)]
pub enum ParseError {
    #[error("unexpected character '{0}' at position {1}")]
    UnexpectedChar(char, usize),
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("unexpected token {0}")]
    UnexpectedToken(String),
    #[error("expected {0}")]
    Expected(String),
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Arrow,        // ->
    EffArrowOpen, // ->{
    FatArrow,     // =>
    LParen,
    RParen,
    LBrace,
    RBrace,
    LAngle,
    RAngle,
    Pipe,
    Colon,
    Comma,
    Dot,
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

impl Lexer {
    fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn tokenize(&mut self) -> Result<Vec<Token>, ParseError> {
        let mut tokens = Vec::new();
        while self.pos < self.chars.len() {
            self.skip_whitespace();
            if self.pos >= self.chars.len() {
                break;
            }
            let ch = self.chars[self.pos];
            match ch {
                '(' => {
                    tokens.push(Token::LParen);
                    self.pos += 1;
                }
                ')' => {
                    tokens.push(Token::RParen);
                    self.pos += 1;
                }
                '{' => {
                    tokens.push(Token::LBrace);
                    self.pos += 1;
                }
                '}' => {
                    tokens.push(Token::RBrace);
                    self.pos += 1;
                }
                '<' => {
                    tokens.push(Token::LAngle);
                    self.pos += 1;
                }
                '>' => {
                    tokens.push(Token::RAngle);
                    self.pos += 1;
                }
                '|' => {
                    tokens.push(Token::Pipe);
                    self.pos += 1;
                }
                ':' => {
                    tokens.push(Token::Colon);
                    self.pos += 1;
                }
                ',' => {
                    tokens.push(Token::Comma);
                    self.pos += 1;
                }
                '.' => {
                    tokens.push(Token::Dot);
                    self.pos += 1;
                }
                '-' => {
                    if self.peek_at(1) == Some('>') {
                        if self.peek_at(2) == Some('{') {
                            tokens.push(Token::EffArrowOpen);
                            self.pos += 3;
                        } else {
                            tokens.push(Token::Arrow);
                            self.pos += 2;
                        }
                    } else {
                        return Err(ParseError::UnexpectedChar(ch, self.pos));
                    }
                }
                '=' => {
                    if self.peek_at(1) == Some('>') {
                        tokens.push(Token::FatArrow);
                        self.pos += 2;
                    } else {
                        return Err(ParseError::UnexpectedChar(ch, self.pos));
                    }
                }
                c if c.is_alphabetic() || c == '_' => {
                    let start = self.pos;
                    while self.pos < self.chars.len()
                        && (self.chars[self.pos].is_alphanumeric() || self.chars[self.pos] == '_')
                    {
                        self.pos += 1;
                    }
                    let word: String = self.chars[start..self.pos].iter().collect();
                    tokens.push(Token::Ident(word));
                }
                _ => return Err(ParseError::UnexpectedChar(ch, self.pos)),
            }
        }
        Ok(tokens)
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<Token> {
        if self.pos < self.tokens.len() {
            let tok = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn expect_token(&mut self, expected: &Token) -> Result<(), ParseError> {
        match self.advance() {
            Some(ref t) if t == expected => Ok(()),
            Some(t) => Err(ParseError::Expected(format!("{expected:?}, got {t:?}"))),
            None => Err(ParseError::UnexpectedEof),
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.advance() {
            Some(Token::Ident(s)) => Ok(s),
            Some(t) => Err(ParseError::Expected(format!("identifier, got {t:?}"))),
            None => Err(ParseError::UnexpectedEof),
        }
    }

    // -----------------------------------------------------------------------
    // ty = forall ... | fun_ty
    // -----------------------------------------------------------------------

    fn parse_ty(&mut self) -> Result<Ty, ParseError> {
        if matches!(self.peek(), Some(Token::Ident(s)) if s == "forall") {
            self.parse_forall()
        } else {
            self.parse_fun_ty()
        }
    }

    // -----------------------------------------------------------------------
    // forall bindings . [constraints =>] body
    // -----------------------------------------------------------------------

    fn parse_forall(&mut self) -> Result<Ty, ParseError> {
        self.advance(); // consume "forall"
        let vars = self.parse_bindings()?;
        self.expect_token(&Token::Dot)?;
        let constraints = self.try_parse_constraints()?;
        let body = self.parse_ty()?;
        Ok(Ty::Forall {
            vars,
            constraints,
            body: Box::new(body),
        })
    }

    fn parse_bindings(&mut self) -> Result<Vec<TyVarBinding>, ParseError> {
        let mut bindings = Vec::new();
        loop {
            match self.peek() {
                Some(Token::Dot) => break,
                Some(Token::LParen) => {
                    self.advance(); // (
                    let name = self.expect_ident()?;
                    self.expect_token(&Token::Colon)?;
                    let kind = self.parse_kind()?;
                    self.expect_token(&Token::RParen)?;
                    bindings.push(TyVarBinding::higher(name, kind));
                }
                Some(Token::Ident(_)) => {
                    let name = self.expect_ident()?;
                    bindings.push(TyVarBinding::simple(name));
                }
                _ => return Err(ParseError::Expected("type variable binding".into())),
            }
        }
        if bindings.is_empty() {
            return Err(ParseError::Expected(
                "at least one type variable binding".into(),
            ));
        }
        Ok(bindings)
    }

    /// Look ahead for `=>` at paren-depth 0 before any `->`. If found, parse
    /// constraints; otherwise return empty.
    fn try_parse_constraints(&mut self) -> Result<Vec<Constraint>, ParseError> {
        let mut depth = 0i32;
        let mut has_fat_arrow = false;
        for i in self.pos..self.tokens.len() {
            match &self.tokens[i] {
                Token::LParen | Token::LBrace | Token::LAngle => depth += 1,
                Token::RParen | Token::RBrace | Token::RAngle => depth -= 1,
                Token::FatArrow if depth == 0 => {
                    has_fat_arrow = true;
                    break;
                }
                Token::Arrow | Token::EffArrowOpen if depth == 0 => break,
                _ => {}
            }
        }

        if !has_fat_arrow {
            return Ok(Vec::new());
        }

        let constraints = if self.peek() == Some(&Token::LParen) {
            self.advance(); // (
            let mut cs = vec![self.parse_constraint()?];
            while self.peek() == Some(&Token::Comma) {
                self.advance();
                cs.push(self.parse_constraint()?);
            }
            self.expect_token(&Token::RParen)?;
            cs
        } else {
            vec![self.parse_constraint()?]
        };

        self.expect_token(&Token::FatArrow)?;
        Ok(constraints)
    }

    fn parse_constraint(&mut self) -> Result<Constraint, ParseError> {
        let class = self.expect_ident()?;
        let mut args = Vec::new();
        while let Some(tok) = self.peek() {
            match tok {
                Token::RParen | Token::Comma | Token::FatArrow => break,
                _ => args.push(self.parse_atom()?),
            }
        }
        Ok(Constraint { class, args })
    }

    // -----------------------------------------------------------------------
    // Kinds
    // -----------------------------------------------------------------------

    fn parse_kind(&mut self) -> Result<Kind, ParseError> {
        let left = self.parse_kind_atom()?;
        if self.peek() == Some(&Token::Arrow) {
            self.advance();
            let right = self.parse_kind()?;
            Ok(Kind::Arrow(Box::new(left), Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_kind_atom(&mut self) -> Result<Kind, ParseError> {
        match self.peek() {
            Some(Token::Ident(s)) if s == "Type" => {
                self.advance();
                Ok(Kind::Type)
            }
            Some(Token::LParen) => {
                self.advance();
                let k = self.parse_kind()?;
                self.expect_token(&Token::RParen)?;
                Ok(k)
            }
            _ => Err(ParseError::Expected("kind (Type or (...))".into())),
        }
    }

    // -----------------------------------------------------------------------
    // fun_ty = app_ty ['->' fun_ty | '->{' effects '}' fun_ty]
    // -----------------------------------------------------------------------

    fn parse_fun_ty(&mut self) -> Result<Ty, ParseError> {
        let left = self.parse_app_ty()?;
        match self.peek() {
            Some(Token::Arrow) => {
                self.advance();
                let ret = self.parse_fun_ty()?;
                Ok(Ty::Fun {
                    param: Box::new(left),
                    effects: EffectSet::pure(),
                    ret: Box::new(ret),
                })
            }
            Some(Token::EffArrowOpen) => {
                self.advance();
                let effects = self.parse_effect_list()?;
                self.expect_token(&Token::RBrace)?;
                let ret = self.parse_fun_ty()?;
                Ok(Ty::Fun {
                    param: Box::new(left),
                    effects: EffectSet(effects),
                    ret: Box::new(ret),
                })
            }
            _ => Ok(left),
        }
    }

    // -----------------------------------------------------------------------
    // Effects
    // -----------------------------------------------------------------------

    fn parse_effect_list(&mut self) -> Result<Vec<Effect>, ParseError> {
        let mut effects = vec![self.parse_effect()?];
        while self.peek() == Some(&Token::Comma) {
            self.advance();
            effects.push(self.parse_effect()?);
        }
        Ok(effects)
    }

    fn parse_effect(&mut self) -> Result<Effect, ParseError> {
        let name = self.expect_ident()?;
        match name.as_str() {
            "IO" => Ok(Effect::IO),
            "Async" => Ok(Effect::Async),
            "Rand" => Ok(Effect::Rand),
            "Alloc" => Ok(Effect::Alloc),
            "Fail" => Ok(Effect::Fail(self.parse_atom()?)),
            "State" => Ok(Effect::State(self.parse_atom()?)),
            _ => Ok(Effect::Named(name)),
        }
    }

    // -----------------------------------------------------------------------
    // app_ty = atom atom*   (first = constructor, rest = args)
    // -----------------------------------------------------------------------

    fn parse_app_ty(&mut self) -> Result<Ty, ParseError> {
        let head = self.parse_atom()?;
        let mut args = Vec::new();

        while let Some(tok) = self.peek() {
            match tok {
                // Stop tokens — these cannot begin a type-argument atom.
                Token::Arrow
                | Token::EffArrowOpen
                | Token::FatArrow
                | Token::RParen
                | Token::RBrace
                | Token::RAngle
                | Token::Comma
                | Token::Pipe
                | Token::Colon
                | Token::Dot => break,
                _ => args.push(self.parse_atom()?),
            }
        }

        if args.is_empty() {
            Ok(head)
        } else {
            Ok(Ty::App {
                con: Box::new(head),
                args,
            })
        }
    }

    // -----------------------------------------------------------------------
    // atom = PRIM | var | named | '(' ... ')' | '{' ... '}' | '<' ... '>'
    // -----------------------------------------------------------------------

    fn parse_atom(&mut self) -> Result<Ty, ParseError> {
        match self.peek() {
            Some(Token::LParen) => {
                self.advance();
                // () = unit tuple
                if self.peek() == Some(&Token::RParen) {
                    self.advance();
                    return Ok(Ty::Tuple(Vec::new()));
                }
                let first = self.parse_ty()?;
                if self.peek() == Some(&Token::Comma) {
                    // Tuple: (a, b, ...)
                    let mut elems = vec![first];
                    while self.peek() == Some(&Token::Comma) {
                        self.advance();
                        elems.push(self.parse_ty()?);
                    }
                    self.expect_token(&Token::RParen)?;
                    Ok(Ty::Tuple(elems))
                } else {
                    // Grouping parens
                    self.expect_token(&Token::RParen)?;
                    Ok(first)
                }
            }

            Some(Token::LBrace) => {
                self.advance();
                if self.peek() == Some(&Token::RBrace) {
                    self.advance();
                    return Ok(Ty::Record(Vec::new()));
                }
                let fields = self.parse_field_list(Token::Comma)?;
                self.expect_token(&Token::RBrace)?;
                Ok(Ty::Record(fields))
            }

            Some(Token::LAngle) => {
                self.advance();
                if self.peek() == Some(&Token::RAngle) {
                    self.advance();
                    return Ok(Ty::Variant(Vec::new()));
                }
                let cases = self.parse_field_list(Token::Pipe)?;
                self.expect_token(&Token::RAngle)?;
                Ok(Ty::Variant(cases))
            }

            Some(Token::Ident(_)) => {
                let name = self.expect_ident()?;
                match name.as_str() {
                    "Int" => Ok(Ty::Prim(Prim::Int)),
                    "Float" => Ok(Ty::Prim(Prim::Float)),
                    "String" => Ok(Ty::Prim(Prim::String)),
                    "Bool" => Ok(Ty::Prim(Prim::Bool)),
                    "Bytes" => Ok(Ty::Prim(Prim::Bytes)),
                    "Unit" => Ok(Ty::Prim(Prim::Unit)),
                    "Never" => Ok(Ty::Prim(Prim::Never)),
                    _ if name.starts_with(|c: char| c.is_lowercase()) => {
                        Ok(Ty::Var(TyVar(name)))
                    }
                    _ => Ok(Ty::Named(name)),
                }
            }

            Some(t) => Err(ParseError::UnexpectedToken(format!("{t:?}"))),
            None => Err(ParseError::UnexpectedEof),
        }
    }

    // -----------------------------------------------------------------------
    // Fields (shared by records and variants, differing only in separator)
    // -----------------------------------------------------------------------

    fn parse_field_list(&mut self, sep: Token) -> Result<Vec<Field>, ParseError> {
        let mut fields = vec![self.parse_field()?];
        while self.peek() == Some(&sep) {
            self.advance();
            fields.push(self.parse_field()?);
        }
        Ok(fields)
    }

    fn parse_field(&mut self) -> Result<Field, ParseError> {
        let name = self.expect_ident()?;
        self.expect_token(&Token::Colon)?;
        let ty = self.parse_ty()?;
        Ok(Field { name, ty })
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a AgentCodeRepo type from its canonical text form.
///
/// ```
/// use agentcoderepo_types::parse::parse_ty;
/// use agentcoderepo_types::ty::*;
///
/// let ty = parse_ty("forall a. Ord a => List a -> List a").unwrap();
/// assert_eq!(ty.to_string(), "forall a. Ord a => List a -> List a");
/// ```
pub fn parse_ty(input: &str) -> Result<Ty, ParseError> {
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize()?;
    let mut parser = Parser::new(tokens);
    let ty = parser.parse_ty()?;
    if parser.pos < parser.tokens.len() {
        return Err(ParseError::UnexpectedToken(format!(
            "{:?}",
            parser.tokens[parser.pos]
        )));
    }
    Ok(ty)
}

/// Parse a AgentCodeRepo kind from its canonical text form.
pub fn parse_kind(input: &str) -> Result<Kind, ParseError> {
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize()?;
    let mut parser = Parser::new(tokens);
    let kind = parser.parse_kind()?;
    if parser.pos < parser.tokens.len() {
        return Err(ParseError::UnexpectedToken(format!(
            "{:?}",
            parser.tokens[parser.pos]
        )));
    }
    Ok(kind)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: parse a type, display it, parse again, and assert equality.
    fn roundtrip(input: &str) {
        let ty = parse_ty(input).unwrap_or_else(|e| panic!("parse failed for {input:?}: {e}"));
        let displayed = ty.to_string();
        let reparsed = parse_ty(&displayed)
            .unwrap_or_else(|e| panic!("re-parse failed for {displayed:?}: {e}"));
        assert_eq!(ty, reparsed, "roundtrip mismatch:\n  input:     {input}\n  displayed: {displayed}");
        assert_eq!(displayed, input, "display mismatch for {input:?}");
    }

    #[test]
    fn prim_int() {
        roundtrip("Int");
    }

    #[test]
    fn prim_string() {
        roundtrip("String");
    }

    #[test]
    fn type_var() {
        let ty = parse_ty("a").unwrap();
        assert_eq!(ty, Ty::Var(TyVar("a".into())));
    }

    #[test]
    fn named_type() {
        let ty = parse_ty("HttpError").unwrap();
        assert_eq!(ty, Ty::Named("HttpError".into()));
    }

    #[test]
    fn pure_arrow() {
        roundtrip("Int -> String");
    }

    #[test]
    fn right_assoc_arrows() {
        roundtrip("Int -> String -> Bool");
    }

    #[test]
    fn parens_on_arrow_param() {
        roundtrip("(a -> b) -> List a -> List b");
    }

    #[test]
    fn simple_app() {
        roundtrip("List Int");
    }

    #[test]
    fn nested_app_parens() {
        roundtrip("Map String (List Int)");
    }

    #[test]
    fn effectful_arrow() {
        roundtrip("String ->{IO, Fail HttpError} Response");
    }

    #[test]
    fn forall_simple() {
        roundtrip("forall a. List a -> List a");
    }

    #[test]
    fn forall_with_constraint() {
        roundtrip("forall a. Ord a => List a -> List a");
    }

    #[test]
    fn forall_multiple_constraints() {
        roundtrip("forall a. (Ord a, Show a) => a -> String");
    }

    #[test]
    fn forall_higher_kinded() {
        roundtrip("forall (f : Type -> Type) a b. Functor f => (a -> b) -> f a -> f b");
    }

    #[test]
    fn record_type() {
        roundtrip("{ name: String, age: Int }");
    }

    #[test]
    fn variant_type() {
        roundtrip("< Ok: a | Err: e >");
    }

    #[test]
    fn tuple_type() {
        roundtrip("(Int, String, Bool)");
    }

    #[test]
    fn unit_tuple() {
        let ty = parse_ty("()").unwrap();
        assert_eq!(ty, Ty::Tuple(Vec::new()));
    }

    #[test]
    fn complex_record_fields() {
        roundtrip("{ handler: String ->{IO} Response, name: String }");
    }

    #[test]
    fn variant_with_app_fields() {
        roundtrip("< Some: List a | None: Unit >");
    }

    #[test]
    fn effect_with_complex_type() {
        roundtrip("String ->{Fail (List Error)} Int");
    }

    #[test]
    fn state_effect() {
        roundtrip("a ->{State Int} a");
    }

    #[test]
    fn kind_simple() {
        let k = parse_kind("Type").unwrap();
        assert_eq!(k, Kind::Type);
    }

    #[test]
    fn kind_arrow() {
        let k = parse_kind("Type -> Type").unwrap();
        assert_eq!(k, Kind::Arrow(Box::new(Kind::Type), Box::new(Kind::Type)));
    }

    #[test]
    fn kind_nested() {
        let k = parse_kind("Type -> Type -> Type").unwrap();
        assert_eq!(k.to_string(), "Type -> Type -> Type");
    }

    #[test]
    fn kind_parens() {
        let k = parse_kind("(Type -> Type) -> Type").unwrap();
        assert_eq!(k.to_string(), "(Type -> Type) -> Type");
    }

    /// Build a type programmatically, display it, parse, compare.
    #[test]
    fn programmatic_roundtrip() {
        let ty = Ty::Forall {
            vars: vec![
                TyVarBinding::higher(
                    "f",
                    Kind::Arrow(Box::new(Kind::Type), Box::new(Kind::Type)),
                ),
                TyVarBinding::simple("a"),
                TyVarBinding::simple("b"),
            ],
            constraints: vec![Constraint {
                class: "Functor".into(),
                args: vec![Ty::var("f")],
            }],
            body: Box::new(Ty::fun(
                Ty::fun(Ty::var("a"), Ty::var("b")),
                Ty::fun(
                    Ty::App {
                        con: Box::new(Ty::var("f")),
                        args: vec![Ty::var("a")],
                    },
                    Ty::App {
                        con: Box::new(Ty::var("f")),
                        args: vec![Ty::var("b")],
                    },
                ),
            )),
        };
        let text = ty.to_string();
        let parsed = parse_ty(&text).unwrap();
        assert_eq!(ty, parsed);
    }
}
