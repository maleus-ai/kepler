//! Datadog-inspired query DSL parser and SQL generator.
//!
//! Converts a search expression like `@service:web AND @level:error @latency:>100`
//! into a SQL WHERE clause with bind parameters.
//!
//! The DSL is schema-configurable: use [`QueryDsl::builder()`] to define fields,
//! types, aliases, and an optional JSON attributes column for dynamic fields.
//!
//! # Full-text search
//!
//! Bare words and quoted strings search the column marked with `.fts()`:
//!
//! | Query | Matches |
//! |-------|---------|
//! | `error` | Rows containing "error" |
//! | `"connection timeout"` | Rows containing the exact phrase "connection timeout" |
//! | `'connection timeout'` | Same — single quotes work identically to double quotes |
//! | `error timeout` | Rows containing both "error" **and** "timeout" (implicit AND) |
//!
//! If no field has `.fts()`, bare text search returns an error.
//!
//! # Field matching
//!
//! All field matching requires the `@` prefix. Fields are resolved
//! through the schema: known fields map to their configured column,
//! unknown fields fall back to the JSON attributes column (if configured).
//!
//! | Query | SQL equivalent |
//! |-------|---------------|
//! | `@service:web` | `service = ?` |
//! | `@host:prod-*` | `host LIKE ? ESCAPE '\'` |
//!
//! # Boolean operators
//!
//! Boolean operators are case-insensitive: `AND`, `and`, `And` all work.
//!
//! | Operator | Syntax | Example |
//! |----------|--------|---------|
//! | AND (explicit) | `AND` | `@service:web AND @level:error` |
//! | AND (implicit) | space | `@service:web @level:error` |
//! | OR | `OR` | `@level:error OR @level:warn` |
//!
//! AND has higher precedence than OR.
//!
//! # Negation
//!
//! Three equivalent negation syntaxes: `NOT`, `-`, `!`.
//!
//! # Grouping
//!
//! Use parentheses to override precedence:
//! `(@service:web OR @service:api) AND @level:error`
//!
//! # Wildcards
//!
//! | Wildcard | Meaning | Example |
//! |----------|---------|---------|
//! | `*` | Any number of characters | `@service:web*`, `err*` |
//! | `?` | Exactly one character | `@level:e?r` matches `err` |
//!
//! Wildcards inside quoted strings are treated as literal characters.
//!
//! # Comparisons
//!
//! Numeric comparisons: `>`, `>=`, `<`, `<=`.
//!
//! | Example | SQL |
//! |---------|-----|
//! | `@latency:>100` | `CAST(latency AS REAL) > ?` |
//! | `@timestamp:>=1000` | `timestamp >= ?` (Int column, no CAST) |

use std::collections::HashMap;

// ============================================================================
// Public types
// ============================================================================

/// Column type for a field, affects SQL generation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColumnType {
    Text,
    Int,
    Real,
}

/// A field definition for the DSL schema.
#[derive(Debug, Clone)]
pub struct Field {
    name: String,
    column: String,
    column_type: ColumnType,
    fts: bool,
}

impl Field {
    /// Create a text field.
    pub fn text(name: &str) -> Self {
        Self { name: name.to_string(), column: name.to_string(), column_type: ColumnType::Text, fts: false }
    }

    /// Create an integer field.
    pub fn int(name: &str) -> Self {
        Self { name: name.to_string(), column: name.to_string(), column_type: ColumnType::Int, fts: false }
    }

    /// Create a real/float field.
    pub fn real(name: &str) -> Self {
        Self { name: name.to_string(), column: name.to_string(), column_type: ColumnType::Real, fts: false }
    }

    /// Set the actual SQL column name (alias).
    pub fn alias(mut self, column: &str) -> Self {
        self.column = column.to_string();
        self
    }

    /// Mark this field as the full-text search target for bare text queries.
    pub fn fts(mut self) -> Self {
        self.fts = true;
        self
    }
}

/// A SQL fragment with bind parameters.
#[derive(Debug, Clone)]
pub struct SqlFragment {
    pub sql: String,
    pub params: Vec<SqlValue>,
}

impl SqlFragment {
    /// Create a raw SQL fragment with no bind parameters (for raw SQL mode).
    pub fn raw(sql: String) -> Self {
        Self { sql, params: Vec::new() }
    }

    /// Return the SQL string with all `?N` placeholders shifted by `offset`.
    ///
    /// This is used to integrate DSL-generated SQL into a larger query that
    /// already has its own numbered bind parameters.
    pub fn sql_with_offset(&self, offset: usize) -> String {
        if offset == 0 {
            return self.sql.clone();
        }
        let bytes = self.sql.as_bytes();
        let mut result = String::with_capacity(self.sql.len() + offset.to_string().len() * self.params.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'?' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                let n: usize = self.sql[start..end].parse().unwrap();
                result.push_str(&format!("?{}", n + offset));
                i = end;
            } else {
                result.push(bytes[i] as char);
                i += 1;
            }
        }
        result
    }
}

/// A bind parameter value.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Text(String),
    Integer(i64),
    Real(f64),
}

impl rusqlite::types::ToSql for SqlValue {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        match self {
            SqlValue::Text(s) => s.to_sql(),
            SqlValue::Integer(i) => i.to_sql(),
            SqlValue::Real(f) => f.to_sql(),
        }
    }
}

/// Builder for constructing a [`QueryDsl`] instance.
pub struct QueryDslBuilder {
    fields: Vec<Field>,
    attributes_column: Option<String>,
}

impl QueryDslBuilder {
    /// Add a field to the schema.
    pub fn field(mut self, field: Field) -> Self {
        self.fields.push(field);
        self
    }

    /// Set the JSON column for unknown/dynamic fields.
    /// When set, unknown `@field` references use `json_extract(column, '$.field')`.
    pub fn attributes(mut self, column: &str) -> Self {
        self.attributes_column = Some(column.to_string());
        self
    }

    /// Build the [`QueryDsl`] instance.
    pub fn build(self) -> QueryDsl {
        let mut field_map = HashMap::new();
        let mut fts_column = None;

        for field in &self.fields {
            field_map.insert(field.name.to_ascii_lowercase(), field.clone());
            if field.fts {
                fts_column = Some(field.column.clone());
            }
        }

        QueryDsl {
            fields: field_map,
            fts_column,
            attributes_column: self.attributes_column,
        }
    }
}

/// A configured query DSL parser and SQL generator.
pub struct QueryDsl {
    fields: HashMap<String, Field>,
    fts_column: Option<String>,
    attributes_column: Option<String>,
}

/// Information about a resolved field.
enum ResolvedField {
    /// A known field with its column name and type.
    Known { column: String, column_type: ColumnType },
    /// A dynamic JSON attribute field.
    Attribute { expr: String },
}

impl QueryDsl {
    /// Create a new builder.
    pub fn builder() -> QueryDslBuilder {
        QueryDslBuilder {
            fields: Vec::new(),
            attributes_column: None,
        }
    }

    /// Parse a DSL query string and convert it to a SQL WHERE clause fragment
    /// with bind parameters.
    ///
    /// `param_offset` sets the starting parameter index. Parameters are numbered
    /// `?{offset+1}`, `?{offset+2}`, etc. This allows integration with queries
    /// that already have parameters.
    pub fn parse(&self, input: &str, param_offset: usize) -> Result<SqlFragment, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("empty query".to_string());
        }

        let mut tokenizer = Tokenizer::new(input);
        let tokens = tokenizer.tokenize()?;

        if tokens.is_empty() {
            return Err("empty query".to_string());
        }

        let mut parser = Parser::new(tokens);
        let ast = parser.parse()?;
        let mut counter = param_offset;
        self.expr_to_sql(&ast, &mut counter)
    }

    /// Resolve a field name through the schema.
    fn resolve_field(&self, name: &str) -> Result<ResolvedField, String> {
        let lower = name.to_ascii_lowercase();
        if let Some(field) = self.fields.get(&lower) {
            Ok(ResolvedField::Known {
                column: field.column.clone(),
                column_type: field.column_type,
            })
        } else if let Some(ref attr_col) = self.attributes_column {
            Ok(ResolvedField::Attribute {
                expr: format!("json_extract({}, '$.{}')", attr_col, sql_escape(name)),
            })
        } else {
            Err(format!("unknown field '{}'", name))
        }
    }

    /// Convert an AST expression to a SQL fragment with bind parameters.
    fn expr_to_sql(&self, expr: &Expr, counter: &mut usize) -> Result<SqlFragment, String> {
        match expr {
            Expr::FullText { text, has_wildcards } => {
                let fts_col = self.fts_column.as_ref()
                    .ok_or("bare text search is not supported (no field has .fts())")?;
                *counter += 1;
                let param_idx = *counter;
                let pattern = if *has_wildcards {
                    wildcard_to_like(text)
                } else {
                    let escaped = text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
                    format!("%{}%", escaped)
                };
                Ok(SqlFragment {
                    sql: format!("{} LIKE ?{} ESCAPE '\\'", fts_col, param_idx),
                    params: vec![SqlValue::Text(pattern)],
                })
            }
            Expr::FieldMatch { field, value, has_wildcards } => {
                let resolved = self.resolve_field(field)?;
                if *has_wildcards {
                    *counter += 1;
                    let param_idx = *counter;
                    let col = resolved_col(&resolved);
                    let pattern = wildcard_to_like(value);
                    Ok(SqlFragment {
                        sql: format!("{} LIKE ?{} ESCAPE '\\'", col, param_idx),
                        params: vec![SqlValue::Text(pattern)],
                    })
                } else {
                    match resolved {
                        ResolvedField::Known { column, column_type } => {
                            *counter += 1;
                            let param_idx = *counter;
                            let param = match column_type {
                                ColumnType::Int => {
                                    if let Ok(n) = value.parse::<i64>() {
                                        SqlValue::Integer(n)
                                    } else {
                                        SqlValue::Text(value.clone())
                                    }
                                }
                                ColumnType::Real => {
                                    if let Ok(n) = value.parse::<f64>() {
                                        if n.is_finite() { SqlValue::Real(n) } else { SqlValue::Text(value.clone()) }
                                    } else {
                                        SqlValue::Text(value.clone())
                                    }
                                }
                                ColumnType::Text => SqlValue::Text(value.clone()),
                            };
                            Ok(SqlFragment {
                                sql: format!("{} = ?{}", column, param_idx),
                                params: vec![param],
                            })
                        }
                        ResolvedField::Attribute { expr } => {
                            if let Ok(n) = value.parse::<f64>() {
                                if n.is_finite() {
                                    // Dual comparison for JSON: string and numeric
                                    *counter += 1;
                                    let p1 = *counter;
                                    *counter += 1;
                                    let p2 = *counter;
                                    Ok(SqlFragment {
                                        sql: format!("({} = ?{} OR {} = ?{})", expr, p1, expr, p2),
                                        params: vec![SqlValue::Text(value.clone()), SqlValue::Real(n)],
                                    })
                                } else {
                                    *counter += 1;
                                    Ok(SqlFragment {
                                        sql: format!("{} = ?{}", expr, *counter),
                                        params: vec![SqlValue::Text(value.clone())],
                                    })
                                }
                            } else {
                                *counter += 1;
                                Ok(SqlFragment {
                                    sql: format!("{} = ?{}", expr, *counter),
                                    params: vec![SqlValue::Text(value.clone())],
                                })
                            }
                        }
                    }
                }
            }
            Expr::Comparison { field, op, value } => {
                let resolved = self.resolve_field(field)?;
                *counter += 1;
                let param_idx = *counter;
                let col = resolved_col(&resolved);

                if let Ok(n) = value.parse::<f64>() {
                    if n.is_finite() {
                        let needs_cast = match &resolved {
                            ResolvedField::Known { column_type, .. } => *column_type == ColumnType::Text,
                            ResolvedField::Attribute { .. } => true,
                        };
                        let sql = if needs_cast {
                            format!("CAST({} AS REAL) {} ?{}", col, op.as_sql(), param_idx)
                        } else {
                            format!("{} {} ?{}", col, op.as_sql(), param_idx)
                        };
                        return Ok(SqlFragment {
                            sql,
                            params: vec![SqlValue::Real(n)],
                        });
                    }
                }
                Ok(SqlFragment {
                    sql: format!("{} {} ?{}", col, op.as_sql(), param_idx),
                    params: vec![SqlValue::Text(value.clone())],
                })
            }
            Expr::And(left, right) => {
                let l = self.expr_to_sql(left, counter)?;
                let r = self.expr_to_sql(right, counter)?;
                let mut params = l.params;
                params.extend(r.params);
                Ok(SqlFragment {
                    sql: format!("({} AND {})", l.sql, r.sql),
                    params,
                })
            }
            Expr::Or(left, right) => {
                let l = self.expr_to_sql(left, counter)?;
                let r = self.expr_to_sql(right, counter)?;
                let mut params = l.params;
                params.extend(r.params);
                Ok(SqlFragment {
                    sql: format!("({} OR {})", l.sql, r.sql),
                    params,
                })
            }
            Expr::Not(inner) => {
                let frag = self.expr_to_sql(inner, counter)?;
                Ok(SqlFragment {
                    sql: format!("NOT ({})", frag.sql),
                    params: frag.params,
                })
            }
        }
    }
}

/// Get the SQL column expression from a resolved field.
fn resolved_col(field: &ResolvedField) -> &str {
    match field {
        ResolvedField::Known { column, .. } => column,
        ResolvedField::Attribute { expr } => expr,
    }
}

// ============================================================================
// Tokens
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// Unquoted word (may contain *, ?)
    Word(String),
    /// Double-quoted string (wildcards are literal inside quotes)
    QuotedString(String),
    Colon,
    At,
    LeftParen,
    RightParen,
    And,
    Or,
    Not,
    /// `-` prefix for negation
    Minus,
    /// `!` prefix for negation
    Bang,
    /// `>`
    Gt,
    /// `>=`
    Gte,
    /// `<`
    Lt,
    /// `<=`
    Lte,
}

// ============================================================================
// Tokenizer
// ============================================================================

struct Tokenizer<'a> {
    input: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
}

impl<'a> Tokenizer<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.char_indices().peekable(),
        }
    }

    fn tokenize(&mut self) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();
        while let Some(&(_, ch)) = self.chars.peek() {
            match ch {
                ' ' | '\t' | '\n' | '\r' => {
                    self.chars.next();
                }
                ':' => {
                    self.chars.next();
                    tokens.push(Token::Colon);
                }
                '@' => {
                    self.chars.next();
                    tokens.push(Token::At);
                }
                '(' => {
                    self.chars.next();
                    tokens.push(Token::LeftParen);
                }
                ')' => {
                    self.chars.next();
                    tokens.push(Token::RightParen);
                }
                '>' => {
                    self.chars.next();
                    if self.chars.peek().is_some_and(|&(_, c)| c == '=') {
                        self.chars.next();
                        tokens.push(Token::Gte);
                    } else {
                        tokens.push(Token::Gt);
                    }
                }
                '<' => {
                    self.chars.next();
                    if self.chars.peek().is_some_and(|&(_, c)| c == '=') {
                        self.chars.next();
                        tokens.push(Token::Lte);
                    } else {
                        tokens.push(Token::Lt);
                    }
                }
                '-' => {
                    self.chars.next();
                    tokens.push(Token::Minus);
                }
                '!' => {
                    self.chars.next();
                    tokens.push(Token::Bang);
                }
                '"' => {
                    tokens.push(self.read_quoted_string('"')?);
                }
                '\'' => {
                    tokens.push(self.read_quoted_string('\'')?);
                }
                _ => {
                    tokens.push(self.read_word());
                }
            }
        }
        Ok(tokens)
    }

    fn read_quoted_string(&mut self, quote_char: char) -> Result<Token, String> {
        // Skip opening quote
        self.chars.next();
        let start = self.chars.peek().map(|&(i, _)| i).unwrap_or(self.input.len());
        let end;

        loop {
            match self.chars.next() {
                Some((i, ch)) if ch == quote_char => {
                    end = i;
                    break;
                }
                Some((_, '\\')) => {
                    // Consume escaped character
                    if self.chars.next().is_none() {
                        return Err("unterminated quoted string".to_string());
                    }
                }
                Some(_) => {}
                None => {
                    return Err("unterminated quoted string".to_string());
                }
            }
        }

        // Re-process to handle escapes properly
        let raw = &self.input[start..end];
        let mut result = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    result.push(next);
                }
            } else {
                result.push(ch);
            }
        }
        Ok(Token::QuotedString(result))
    }

    fn read_word(&mut self) -> Token {
        let start = self.chars.peek().map(|&(i, _)| i).unwrap_or(self.input.len());
        let mut end = start;

        while let Some(&(i, ch)) = self.chars.peek() {
            match ch {
                ' ' | '\t' | '\n' | '\r' | ':' | '@' | '(' | ')' | '>' | '<' | '"' | '\'' | '!' => {
                    break;
                }
                _ => {
                    end = i + ch.len_utf8();
                    self.chars.next();
                }
            }
        }

        let word = &self.input[start..end];
        if word.eq_ignore_ascii_case("AND") {
            Token::And
        } else if word.eq_ignore_ascii_case("OR") {
            Token::Or
        } else if word.eq_ignore_ascii_case("NOT") {
            Token::Not
        } else {
            Token::Word(word.to_string())
        }
    }
}

// ============================================================================
// AST
// ============================================================================

/// Comparison operator for numeric field queries.
#[derive(Debug, Clone, PartialEq)]
enum CompOp {
    Gt,
    Gte,
    Lt,
    Lte,
}

impl CompOp {
    fn as_sql(&self) -> &'static str {
        match self {
            CompOp::Gt => ">",
            CompOp::Gte => ">=",
            CompOp::Lt => "<",
            CompOp::Lte => "<=",
        }
    }
}

/// AST node for the query DSL.
#[derive(Debug, Clone, PartialEq)]
enum Expr {
    /// Full-text search on the FTS column.
    FullText {
        text: String,
        has_wildcards: bool,
    },
    /// Field match: `@field:value`
    FieldMatch {
        field: String,
        value: String,
        has_wildcards: bool,
    },
    /// Numeric comparison: `@field:>100`
    Comparison {
        field: String,
        op: CompOp,
        value: String,
    },
    /// Boolean AND
    And(Box<Expr>, Box<Expr>),
    /// Boolean OR
    Or(Box<Expr>, Box<Expr>),
    /// Boolean NOT
    Not(Box<Expr>),
}

// ============================================================================
// Parser
// ============================================================================

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

    fn advance(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    fn expect(&mut self, expected: &Token) -> Result<(), String> {
        match self.advance() {
            Some(tok) if tok == expected => Ok(()),
            Some(tok) => Err(format!("expected {:?}, got {:?}", expected, tok)),
            None => Err(format!("expected {:?}, got end of input", expected)),
        }
    }

    /// Parse the full expression.
    fn parse(&mut self) -> Result<Expr, String> {
        let expr = self.parse_or()?;
        if self.pos < self.tokens.len() {
            return Err(format!(
                "unexpected token at position {}: {:?}",
                self.pos,
                self.tokens[self.pos]
            ));
        }
        Ok(expr)
    }

    /// OR has lowest precedence.
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// AND has higher precedence than OR.
    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Token::And) => {
                    self.advance();
                    let right = self.parse_unary()?;
                    left = Expr::And(Box::new(left), Box::new(right));
                }
                // Implicit AND: next token starts a new primary
                Some(Token::At | Token::Word(_) | Token::QuotedString(_) | Token::LeftParen
                     | Token::Not | Token::Minus | Token::Bang) => {
                    let right = self.parse_unary()?;
                    left = Expr::And(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    /// Unary: NOT, -, !
    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Token::Not | Token::Minus | Token::Bang) => {
                self.advance();
                let expr = self.parse_primary()?;
                Ok(Expr::Not(Box::new(expr)))
            }
            _ => self.parse_primary(),
        }
    }

    /// Primary: grouped expression, @field:value, or bare text.
    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.peek().cloned() {
            Some(Token::LeftParen) => {
                self.advance();
                let expr = self.parse_or()?;
                self.expect(&Token::RightParen)?;
                Ok(expr)
            }
            Some(Token::At) => {
                self.advance();
                self.parse_field_expr()
            }
            Some(Token::Word(word)) => {
                self.advance();
                // Bare field:value is not supported — require @ prefix
                if matches!(self.peek(), Some(Token::Colon)) {
                    return Err(format!(
                        "bare field '{}:' is not supported; use '@{}:' instead",
                        word, word
                    ));
                }
                // Bare word → full-text search
                let has_wildcards = word.contains('*') || word.contains('?');
                Ok(Expr::FullText {
                    text: word,
                    has_wildcards,
                })
            }
            Some(Token::QuotedString(s)) => {
                self.advance();
                Ok(Expr::FullText {
                    text: s,
                    has_wildcards: false,
                })
            }
            Some(tok) => Err(format!("unexpected token: {:?}", tok)),
            None => Err("unexpected end of input".to_string()),
        }
    }

    /// Parse `field_name:value` after seeing `@`.
    fn parse_field_expr(&mut self) -> Result<Expr, String> {
        let field_name = match self.advance().cloned() {
            Some(Token::Word(w)) => w,
            other => return Err(format!("expected field name after '@', got {:?}", other)),
        };
        self.expect(&Token::Colon)?;
        self.parse_field_value(field_name)
    }

    /// Parse the value part after `@field:`.
    fn parse_field_value(&mut self, field_name: String) -> Result<Expr, String> {
        let field = field_name;

        // Check for comparison operator
        match self.peek().cloned() {
            Some(Token::Gt) => {
                self.advance();
                let value = self.read_value()?;
                Ok(Expr::Comparison { field, op: CompOp::Gt, value })
            }
            Some(Token::Gte) => {
                self.advance();
                let value = self.read_value()?;
                Ok(Expr::Comparison { field, op: CompOp::Gte, value })
            }
            Some(Token::Lt) => {
                self.advance();
                let value = self.read_value()?;
                Ok(Expr::Comparison { field, op: CompOp::Lt, value })
            }
            Some(Token::Lte) => {
                self.advance();
                let value = self.read_value()?;
                Ok(Expr::Comparison { field, op: CompOp::Lte, value })
            }
            _ => {
                let (value, has_wildcards) = self.read_value_with_wildcards()?;
                Ok(Expr::FieldMatch { field, value, has_wildcards })
            }
        }
    }

    fn read_value(&mut self) -> Result<String, String> {
        // Allow a leading minus for negative numeric values (e.g. @field:>-100)
        if matches!(self.peek(), Some(Token::Minus)) {
            self.advance();
            match self.advance().cloned() {
                Some(Token::Word(w)) => Ok(format!("-{}", w)),
                Some(Token::QuotedString(s)) => Ok(format!("-{}", s)),
                other => Err(format!("expected value after '-', got {:?}", other)),
            }
        } else {
            match self.advance().cloned() {
                Some(Token::Word(w)) => Ok(w),
                Some(Token::QuotedString(s)) => Ok(s),
                other => Err(format!("expected value, got {:?}", other)),
            }
        }
    }

    fn read_value_with_wildcards(&mut self) -> Result<(String, bool), String> {
        // Allow a leading minus for negative numeric values (e.g. @field:-42)
        if matches!(self.peek(), Some(Token::Minus)) {
            self.advance();
            match self.advance().cloned() {
                Some(Token::Word(w)) => {
                    let val = format!("-{}", w);
                    let has_wildcards = val.contains('*') || val.contains('?');
                    Ok((val, has_wildcards))
                }
                Some(Token::QuotedString(s)) => Ok((format!("-{}", s), false)),
                other => Err(format!("expected value after '-', got {:?}", other)),
            }
        } else {
            match self.advance().cloned() {
                Some(Token::Word(w)) => {
                    let has_wildcards = w.contains('*') || w.contains('?');
                    Ok((w, has_wildcards))
                }
                Some(Token::QuotedString(s)) => {
                    Ok((s, false))
                }
                other => Err(format!("expected value, got {:?}", other)),
            }
        }
    }
}

// ============================================================================
// SQL helpers
// ============================================================================

/// Escape a string value for safe embedding in a SQL string literal.
/// Single quotes are doubled: `'` → `''`.
fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// Convert a DSL wildcard pattern to a SQL LIKE pattern.
/// `*` → `%`, `?` → `_`, and existing `%`, `_`, and `\` are escaped with `\`.
pub fn wildcard_to_like(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '*' => result.push('%'),
            '?' => result.push('_'),
            '\\' => { result.push('\\'); result.push('\\'); }
            '%' => { result.push('\\'); result.push('%'); }
            '_' => { result.push('\\'); result.push('_'); }
            _ => result.push(ch),
        }
    }
    result
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Log-specific DSL schema for tests.
    fn log_dsl() -> QueryDsl {
        QueryDsl::builder()
            .field(Field::text("service"))
            .field(Field::text("level"))
            .field(Field::text("message").alias("line").fts())
            .field(Field::text("hook"))
            .field(Field::int("timestamp"))
            .attributes("attributes")
            .build()
    }

    /// Simple DSL schema (no attributes, no FTS) for tests.
    fn simple_dsl() -> QueryDsl {
        QueryDsl::builder()
            .field(Field::text("name"))
            .field(Field::text("status"))
            .field(Field::int("count"))
            .build()
    }

    fn parse(dsl: &QueryDsl, input: &str) -> SqlFragment {
        dsl.parse(input, 0).unwrap()
    }

    fn parse_err(dsl: &QueryDsl, input: &str) -> String {
        dsl.parse(input, 0).unwrap_err()
    }

    // ---- Full-text search ----

    #[test]
    fn bare_word() {
        let frag = parse(&log_dsl(), "error");
        assert_eq!(frag.sql, "line LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text("%error%".into())]);
    }

    #[test]
    fn quoted_phrase() {
        let frag = parse(&log_dsl(), r#""connection timeout""#);
        assert_eq!(frag.sql, "line LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text("%connection timeout%".into())]);
    }

    #[test]
    fn bare_word_with_wildcard() {
        let frag = parse(&log_dsl(), "err*");
        assert_eq!(frag.sql, "line LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text("err%".into())]);
    }

    #[test]
    fn bare_word_with_question_mark() {
        let frag = parse(&log_dsl(), "f?o");
        assert_eq!(frag.sql, "line LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text("f_o".into())]);
    }

    #[test]
    fn fts_disabled_rejects_bare_text() {
        let err = parse_err(&simple_dsl(), "error");
        assert!(err.contains("not supported"));
    }

    // ---- Field matching ----

    #[test]
    fn service_match() {
        let frag = parse(&log_dsl(), "@service:web");
        assert_eq!(frag.sql, "service = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("web".into())]);
    }

    #[test]
    fn level_match() {
        let frag = parse(&log_dsl(), "@level:error");
        assert_eq!(frag.sql, "level = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("error".into())]);
    }

    #[test]
    fn message_alias() {
        let frag = parse(&log_dsl(), "@message:hello");
        assert_eq!(frag.sql, "line = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("hello".into())]);
    }

    #[test]
    fn hook_match() {
        let frag = parse(&log_dsl(), "@hook:pre_start");
        assert_eq!(frag.sql, "hook = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("pre_start".into())]);
    }

    #[test]
    fn field_with_wildcard_value() {
        let frag = parse(&log_dsl(), "@service:web*");
        assert_eq!(frag.sql, "service LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text("web%".into())]);
    }

    #[test]
    fn field_with_quoted_value() {
        let frag = parse(&log_dsl(), r#"@service:"my-web-app""#);
        assert_eq!(frag.sql, "service = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("my-web-app".into())]);
    }

    #[test]
    fn case_insensitive_field() {
        let frag = parse(&log_dsl(), "@Service:web");
        assert_eq!(frag.sql, "service = ?1");
        let frag = parse(&log_dsl(), "@LEVEL:error");
        assert_eq!(frag.sql, "level = ?1");
    }

    // ---- Timestamp (Int field) ----

    #[test]
    fn timestamp_exact_match() {
        let frag = parse(&log_dsl(), "@timestamp:1000");
        assert_eq!(frag.sql, "timestamp = ?1");
        assert_eq!(frag.params, vec![SqlValue::Integer(1000)]);
    }

    #[test]
    fn timestamp_comparison_gt() {
        let frag = parse(&log_dsl(), "@timestamp:>1000");
        assert_eq!(frag.sql, "timestamp > ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(1000.0)]);
    }

    #[test]
    fn timestamp_comparison_gte() {
        let frag = parse(&log_dsl(), "@timestamp:>=1000");
        assert_eq!(frag.sql, "timestamp >= ?1");
    }

    #[test]
    fn timestamp_comparison_lt() {
        let frag = parse(&log_dsl(), "@timestamp:<2000");
        assert_eq!(frag.sql, "timestamp < ?1");
    }

    #[test]
    fn timestamp_comparison_lte() {
        let frag = parse(&log_dsl(), "@timestamp:<=2000");
        assert_eq!(frag.sql, "timestamp <= ?1");
    }

    #[test]
    fn timestamp_range() {
        let frag = parse(&log_dsl(), "@timestamp:>=1000 @timestamp:<2000");
        assert_eq!(frag.sql, "(timestamp >= ?1 AND timestamp < ?2)");
        assert_eq!(frag.params, vec![SqlValue::Real(1000.0), SqlValue::Real(2000.0)]);
    }

    // ---- JSON attribute search ----

    #[test]
    fn attribute_match() {
        let frag = parse(&log_dsl(), "@http_status:200");
        assert_eq!(frag.sql, "(json_extract(attributes, '$.http_status') = ?1 OR json_extract(attributes, '$.http_status') = ?2)");
        assert_eq!(frag.params, vec![SqlValue::Text("200".into()), SqlValue::Real(200.0)]);
    }

    #[test]
    fn attribute_string() {
        let frag = parse(&log_dsl(), "@user:john");
        assert_eq!(frag.sql, "json_extract(attributes, '$.user') = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("john".into())]);
    }

    #[test]
    fn attribute_wildcard() {
        let frag = parse(&log_dsl(), "@host:prod-*");
        assert_eq!(frag.sql, "json_extract(attributes, '$.host') LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text("prod-%".into())]);
    }

    #[test]
    fn attribute_comparison() {
        let frag = parse(&log_dsl(), "@latency:>100");
        assert_eq!(frag.sql, "CAST(json_extract(attributes, '$.latency') AS REAL) > ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(100.0)]);
    }

    #[test]
    fn unknown_field_no_attributes_errors() {
        let err = parse_err(&simple_dsl(), "@unknown:value");
        assert!(err.contains("unknown field"));
    }

    // ---- Comparison operators ----

    #[test]
    fn comparison_float() {
        let frag = parse(&log_dsl(), "@latency:>0.5");
        assert_eq!(frag.sql, "CAST(json_extract(attributes, '$.latency') AS REAL) > ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(0.5)]);
    }

    #[test]
    fn comparison_on_text_field_casts() {
        // Text fields need CAST for numeric comparisons
        let frag = parse(&log_dsl(), "@level:>100");
        assert_eq!(frag.sql, "CAST(level AS REAL) > ?1");
    }

    #[test]
    fn comparison_on_int_field_no_cast() {
        // Int fields don't need CAST
        let frag = parse(&log_dsl(), "@timestamp:>1000");
        assert_eq!(frag.sql, "timestamp > ?1");
    }

    // ---- Boolean operators ----

    #[test]
    fn explicit_and() {
        let frag = parse(&log_dsl(), "@service:web AND @level:error");
        assert_eq!(frag.sql, "(service = ?1 AND level = ?2)");
        assert_eq!(frag.params, vec![SqlValue::Text("web".into()), SqlValue::Text("error".into())]);
    }

    #[test]
    fn implicit_and() {
        let frag = parse(&log_dsl(), "@service:web @level:error");
        assert_eq!(frag.sql, "(service = ?1 AND level = ?2)");
    }

    #[test]
    fn or_operator() {
        let frag = parse(&log_dsl(), "@level:error OR @level:warn");
        assert_eq!(frag.sql, "(level = ?1 OR level = ?2)");
    }

    #[test]
    fn and_higher_precedence_than_or() {
        let frag = parse(&log_dsl(), "@level:info OR @service:web AND @level:error");
        assert_eq!(frag.sql, "(level = ?1 OR (service = ?2 AND level = ?3))");
    }

    // ---- Case-insensitive operators ----

    #[test]
    fn lowercase_and() {
        let frag = parse(&log_dsl(), "@service:web and @level:error");
        assert_eq!(frag.sql, "(service = ?1 AND level = ?2)");
    }

    #[test]
    fn lowercase_or() {
        let frag = parse(&log_dsl(), "@level:error or @level:warn");
        assert_eq!(frag.sql, "(level = ?1 OR level = ?2)");
    }

    #[test]
    fn lowercase_not() {
        let frag = parse(&log_dsl(), "not @level:info");
        assert_eq!(frag.sql, "NOT (level = ?1)");
    }

    #[test]
    fn mixed_case_operators() {
        let frag = parse(&log_dsl(), "@service:web And @level:error");
        assert_eq!(frag.sql, "(service = ?1 AND level = ?2)");
    }

    // ---- Negation ----

    #[test]
    fn not_keyword() {
        let frag = parse(&log_dsl(), "NOT @level:info");
        assert_eq!(frag.sql, "NOT (level = ?1)");
    }

    #[test]
    fn minus_negation() {
        let frag = parse(&log_dsl(), "-@service:web");
        assert_eq!(frag.sql, "NOT (service = ?1)");
    }

    #[test]
    fn bang_negation() {
        let frag = parse(&log_dsl(), "!@level:debug");
        assert_eq!(frag.sql, "NOT (level = ?1)");
    }

    // ---- Grouping ----

    #[test]
    fn simple_group() {
        let frag = parse(&log_dsl(), "(@service:web OR @service:api)");
        assert_eq!(frag.sql, "(service = ?1 OR service = ?2)");
    }

    #[test]
    fn negated_group() {
        let frag = parse(&log_dsl(), "NOT (@service:web OR @service:api)");
        assert_eq!(frag.sql, "NOT ((service = ?1 OR service = ?2))");
    }

    // ---- Param offset ----

    #[test]
    fn param_offset() {
        let frag = log_dsl().parse("@service:web AND @level:error", 3).unwrap();
        assert_eq!(frag.sql, "(service = ?4 AND level = ?5)");
    }

    // ---- Error cases ----

    #[test]
    fn empty_query() {
        assert!(log_dsl().parse("", 0).is_err());
    }

    #[test]
    fn unterminated_quote() {
        assert!(log_dsl().parse(r#""hello"#, 0).is_err());
    }

    #[test]
    fn unmatched_paren() {
        assert!(log_dsl().parse("(@service:web", 0).is_err());
    }

    #[test]
    fn bare_field_rejected() {
        let err = parse_err(&log_dsl(), "service:web");
        assert!(err.contains("@service:"));
    }

    // ---- Complex queries ----

    #[test]
    fn complex_mixed() {
        let frag = parse(&log_dsl(), "@service:web @level:error @http_status:>400 -@user:bot*");
        assert_eq!(
            frag.sql,
            "(((service = ?1 AND level = ?2) AND CAST(json_extract(attributes, '$.http_status') AS REAL) > ?3) AND NOT (json_extract(attributes, '$.user') LIKE ?4 ESCAPE '\\'))"
        );
    }

    #[test]
    fn fulltext_and_field() {
        let frag = parse(&log_dsl(), "error @service:web");
        assert_eq!(frag.sql, "(line LIKE ?1 ESCAPE '\\' AND service = ?2)");
    }

    // ---- Security tests ----

    #[test]
    fn injection_single_quote_in_value() {
        // With bind params, single quotes in values are safe
        let frag = parse(&log_dsl(), r#"@service:"O'Reilly""#);
        assert_eq!(frag.sql, "service = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("O'Reilly".into())]);
    }

    #[test]
    fn injection_or_tautology() {
        let frag = parse(&log_dsl(), "@service:web OR 1=1");
        // "1=1" is a bare word → fulltext search, not SQL tautology
        assert!(frag.sql.contains("service = ?1"));
        assert!(frag.sql.contains("line LIKE ?2"));
    }

    #[test]
    fn injection_inf_nan_treated_as_string() {
        let frag = parse(&log_dsl(), "@latency:>inf");
        assert_eq!(frag.sql, "json_extract(attributes, '$.latency') > ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("inf".into())]);
    }

    // ---- Single-quoted strings ----

    #[test]
    fn single_quoted_phrase() {
        let frag = parse(&log_dsl(), "'connection timeout'");
        assert_eq!(frag.sql, "line LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text("%connection timeout%".into())]);
    }

    #[test]
    fn single_quoted_field_value() {
        let frag = parse(&log_dsl(), "@service:'my-web-app'");
        assert_eq!(frag.sql, "service = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("my-web-app".into())]);
    }

    // ---- Escape sequences in quotes ----

    #[test]
    fn escaped_double_quote_inside_string() {
        let frag = parse(&log_dsl(), r#""hello \"world\"""#);
        assert_eq!(frag.sql, "line LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text(r#"%hello "world"%"#.into())]);
    }

    #[test]
    fn escaped_backslash_inside_string() {
        let frag = parse(&log_dsl(), r#""back\\slash""#);
        assert_eq!(frag.sql, "line LIKE ?1 ESCAPE '\\'");
        // Tokenizer un-escapes \\ → \, then LIKE escaping re-escapes \ → \\
        assert_eq!(frag.params, vec![SqlValue::Text("%back\\\\slash%".into())]);
    }

    #[test]
    fn escaped_single_quote_inside_single_quotes() {
        let frag = parse(&log_dsl(), r"'it\'s fine'");
        assert_eq!(frag.params, vec![SqlValue::Text("%it's fine%".into())]);
    }

    // ---- Negative numbers ----

    #[test]
    fn negative_comparison_value() {
        let frag = parse(&log_dsl(), "@timestamp:>-100");
        assert_eq!(frag.sql, "timestamp > ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(-100.0)]);
    }

    #[test]
    fn negative_comparison_lte() {
        let frag = parse(&log_dsl(), "@timestamp:<=-50");
        assert_eq!(frag.sql, "timestamp <= ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(-50.0)]);
    }

    #[test]
    fn negative_exact_match_int_field() {
        let frag = parse(&log_dsl(), "@timestamp:-42");
        assert_eq!(frag.sql, "timestamp = ?1");
        assert_eq!(frag.params, vec![SqlValue::Integer(-42)]);
    }

    #[test]
    fn negative_fulltext_bare_is_negation() {
        // Bare -42 means NOT fulltext "42"
        let frag = parse(&log_dsl(), "-42");
        assert_eq!(frag.sql, "NOT (line LIKE ?1 ESCAPE '\\')");
        assert_eq!(frag.params, vec![SqlValue::Text("%42%".into())]);
    }

    #[test]
    fn negative_fulltext_quoted_is_literal() {
        // Quoted "-42" searches for the literal string "-42"
        let frag = parse(&log_dsl(), r#""-42""#);
        assert_eq!(frag.sql, "line LIKE ?1 ESCAPE '\\'");
        assert_eq!(frag.params, vec![SqlValue::Text("%-42%".into())]);
    }

    #[test]
    fn negative_attribute_value() {
        let frag = parse(&log_dsl(), "@offset:-10");
        // JSON attribute, numeric → dual comparison
        assert_eq!(frag.sql, "(json_extract(attributes, '$.offset') = ?1 OR json_extract(attributes, '$.offset') = ?2)");
        assert_eq!(frag.params, vec![SqlValue::Text("-10".into()), SqlValue::Real(-10.0)]);
    }

    // ---- Real (float) field ----

    fn real_dsl() -> QueryDsl {
        QueryDsl::builder()
            .field(Field::text("name"))
            .field(Field::real("score"))
            .field(Field::real("weight").alias("w"))
            .build()
    }

    #[test]
    fn real_field_exact_numeric() {
        let frag = parse(&real_dsl(), "@score:3.14");
        assert_eq!(frag.sql, "score = ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(3.14)]);
    }

    #[test]
    fn real_field_exact_integer_value() {
        let frag = parse(&real_dsl(), "@score:42");
        assert_eq!(frag.sql, "score = ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(42.0)]);
    }

    #[test]
    fn real_field_non_numeric_fallback() {
        let frag = parse(&real_dsl(), "@score:abc");
        assert_eq!(frag.sql, "score = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("abc".into())]);
    }

    #[test]
    fn real_field_comparison() {
        let frag = parse(&real_dsl(), "@score:>0.5");
        assert_eq!(frag.sql, "score > ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(0.5)]);
    }

    #[test]
    fn real_field_alias() {
        let frag = parse(&real_dsl(), "@weight:1.0");
        assert_eq!(frag.sql, "w = ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(1.0)]);
    }

    #[test]
    fn real_field_inf_treated_as_text() {
        let frag = parse(&real_dsl(), "@score:inf");
        assert_eq!(frag.params, vec![SqlValue::Text("inf".into())]);
    }

    // ---- Non-numeric comparison ----

    #[test]
    fn comparison_non_numeric_value() {
        let frag = parse(&log_dsl(), "@timestamp:>abc");
        assert_eq!(frag.sql, "timestamp > ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("abc".into())]);
    }

    #[test]
    fn comparison_non_numeric_on_attribute() {
        let frag = parse(&log_dsl(), "@version:>2.0.0");
        assert_eq!(frag.sql, "json_extract(attributes, '$.version') > ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("2.0.0".into())]);
    }

    // ---- Int field with non-numeric value ----

    #[test]
    fn int_field_non_numeric_fallback() {
        let frag = parse(&log_dsl(), "@timestamp:abc");
        assert_eq!(frag.sql, "timestamp = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("abc".into())]);
    }

    // ---- wildcard_to_like escaping ----

    #[test]
    fn wildcard_to_like_literal_percent() {
        assert_eq!(wildcard_to_like("100%"), "100\\%");
    }

    #[test]
    fn wildcard_to_like_literal_underscore() {
        assert_eq!(wildcard_to_like("a_b"), "a\\_b");
    }

    #[test]
    fn wildcard_to_like_literal_backslash() {
        assert_eq!(wildcard_to_like("a\\b"), "a\\\\b");
    }

    #[test]
    fn wildcard_to_like_combined() {
        assert_eq!(wildcard_to_like("pre*_suf?100%"), "pre%\\_suf_100\\%");
    }

    // ---- Empty value after field ----

    #[test]
    fn empty_value_after_colon() {
        let err = parse_err(&log_dsl(), "@field:");
        assert!(err.contains("expected value") || err.contains("end of input"), "got: {}", err);
    }

    #[test]
    fn empty_value_after_comparison() {
        let err = parse_err(&log_dsl(), "@timestamp:>");
        assert!(err.contains("expected value") || err.contains("end of input"), "got: {}", err);
    }

    // ---- sql_with_offset ----

    #[test]
    fn sql_with_offset_zero_returns_same() {
        let frag = parse(&log_dsl(), "@service:web AND @level:err");
        assert_eq!(frag.sql_with_offset(0), frag.sql);
    }

    #[test]
    fn sql_with_offset_shifts_all_params() {
        let frag = parse(&log_dsl(), "@service:web AND @level:err");
        assert_eq!(frag.sql_with_offset(5), "(service = ?6 AND level = ?7)");
    }

    // ---- SqlFragment::raw ----

    #[test]
    fn sql_fragment_raw() {
        let frag = SqlFragment::raw("level = 'err'".into());
        assert_eq!(frag.sql, "level = 'err'");
        assert!(frag.params.is_empty());
    }

    // ---- Deeply nested expressions ----

    #[test]
    fn deeply_nested() {
        let frag = parse(&log_dsl(), "((@service:web OR @service:api) AND (@level:err OR @level:error))");
        assert_eq!(
            frag.sql,
            "((service = ?1 OR service = ?2) AND (level = ?3 OR level = ?4))"
        );
        assert_eq!(frag.params.len(), 4);
    }

    // ---- Multiple FTS terms (implicit AND) ----

    #[test]
    fn multiple_fts_terms() {
        let frag = parse(&log_dsl(), "error timeout");
        assert_eq!(frag.sql, "(line LIKE ?1 ESCAPE '\\' AND line LIKE ?2 ESCAPE '\\')");
        assert_eq!(frag.params, vec![
            SqlValue::Text("%error%".into()),
            SqlValue::Text("%timeout%".into()),
        ]);
    }

    // ---- Simple schema tests ----

    #[test]
    fn simple_schema_field_match() {
        let frag = parse(&simple_dsl(), "@name:alice");
        assert_eq!(frag.sql, "name = ?1");
        assert_eq!(frag.params, vec![SqlValue::Text("alice".into())]);
    }

    #[test]
    fn simple_schema_int_field() {
        let frag = parse(&simple_dsl(), "@count:42");
        assert_eq!(frag.sql, "count = ?1");
        assert_eq!(frag.params, vec![SqlValue::Integer(42)]);
    }

    #[test]
    fn simple_schema_int_comparison() {
        let frag = parse(&simple_dsl(), "@count:>10");
        assert_eq!(frag.sql, "count > ?1");
        assert_eq!(frag.params, vec![SqlValue::Real(10.0)]);
    }

    // ========================================================================
    // SQLite execution tests
    // ========================================================================

    mod sqlite_exec {
        use super::*;
        use rusqlite::Connection;

        fn setup_db() -> Connection {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE logs (
                    id         INTEGER PRIMARY KEY,
                    timestamp  INTEGER NOT NULL,
                    service    TEXT    NOT NULL,
                    hook       TEXT,
                    level      TEXT    NOT NULL,
                    line       TEXT    NOT NULL,
                    attributes TEXT
                )",
            )
            .unwrap();
            conn.execute_batch(
                r#"INSERT INTO logs VALUES (1, 1000, 'web',    NULL,        'out',   'server started on port 8080', NULL);
                   INSERT INTO logs VALUES (2, 1001, 'web',    NULL,        'err',   'connection refused to database', NULL);
                   INSERT INTO logs VALUES (3, 1002, 'api',    NULL,        'out',   'request received GET /users', '{"http_status": 200, "latency": 45.2}');
                   INSERT INTO logs VALUES (4, 1003, 'api',    NULL,        'err',   'internal server error', '{"http_status": 500, "latency": 120.5, "user": "bot-crawler"}');
                   INSERT INTO logs VALUES (5, 1004, 'worker', NULL,        'out',   'processing job 42', '{"queue": "emails", "priority": "high"}');
                   INSERT INTO logs VALUES (6, 1005, 'web',    'pre_start', 'out',   'running pre_start hook', NULL);
                   INSERT INTO logs VALUES (7, 1006, 'api',    NULL,        'info',  'health check ok', '{"http_status": 200}');
                   INSERT INTO logs VALUES (8, 1007, 'web',    NULL,        'error', 'fatal: out of memory', NULL);"#,
            )
            .unwrap();
            conn
        }

        fn query_ids(conn: &Connection, dsl_str: &str) -> Vec<i64> {
            let dsl = log_dsl();
            let frag = dsl.parse(dsl_str, 0).unwrap();
            let sql = format!("SELECT id FROM logs WHERE {} ORDER BY id", frag.sql);
            let params: Vec<&dyn rusqlite::types::ToSql> =
                frag.params.iter().map(|p| p as &dyn rusqlite::types::ToSql).collect();
            let mut stmt = conn.prepare(&sql).unwrap();
            stmt.query_map(params.as_slice(), |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        }

        #[test]
        fn exec_fulltext_search() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "connection"), vec![2]);
        }

        #[test]
        fn exec_fulltext_phrase() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, r#""server started""#), vec![1]);
        }

        #[test]
        fn exec_fulltext_wildcard() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "process*"), vec![5]);
        }

        #[test]
        fn exec_service_filter() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@service:web"), vec![1, 2, 6, 8]);
        }

        #[test]
        fn exec_service_wildcard() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@service:w*"), vec![1, 2, 5, 6, 8]);
        }

        #[test]
        fn exec_level_filter() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@level:err"), vec![2, 4]);
        }

        #[test]
        fn exec_hook_filter() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@hook:pre_start"), vec![6]);
        }

        #[test]
        fn exec_attribute_match() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@http_status:200"), vec![3, 7]);
        }

        #[test]
        fn exec_attribute_comparison_gt() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@latency:>100"), vec![4]);
        }

        #[test]
        fn exec_attribute_comparison_lte() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@latency:<=50"), vec![3]);
        }

        #[test]
        fn exec_attribute_wildcard() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@user:bot*"), vec![4]);
        }

        #[test]
        fn exec_attribute_string_match() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@priority:high"), vec![5]);
        }

        #[test]
        fn exec_and() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@service:api AND @level:err"), vec![4]);
        }

        #[test]
        fn exec_implicit_and() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@service:api @level:err"), vec![4]);
        }

        #[test]
        fn exec_or() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@level:err OR @level:error"), vec![2, 4, 8]);
        }

        #[test]
        fn exec_not() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "NOT @service:web"), vec![3, 4, 5, 7]);
        }

        #[test]
        fn exec_minus_negation() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@service:api -@level:err"), vec![3, 7]);
        }

        #[test]
        fn exec_grouped_or_with_and() {
            let conn = setup_db();
            assert_eq!(
                query_ids(&conn, "(@service:web OR @service:api) AND @level:err"),
                vec![2, 4]
            );
        }

        #[test]
        fn exec_complex_query() {
            let conn = setup_db();
            let result = query_ids(&conn, "@service:api @http_status:>400 -@user:bot*");
            assert!(result.is_empty());
        }

        #[test]
        fn exec_timestamp_exact() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@timestamp:1000"), vec![1]);
        }

        #[test]
        fn exec_timestamp_gt() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@timestamp:>1005"), vec![7, 8]);
        }

        #[test]
        fn exec_timestamp_gte() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@timestamp:>=1005"), vec![6, 7, 8]);
        }

        #[test]
        fn exec_timestamp_lt() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@timestamp:<1002"), vec![1, 2]);
        }

        #[test]
        fn exec_timestamp_range() {
            let conn = setup_db();
            assert_eq!(
                query_ids(&conn, "@timestamp:>=1002 @timestamp:<1005"),
                vec![3, 4, 5]
            );
        }

        #[test]
        fn exec_lowercase_operators() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "@service:api and @level:err"), vec![4]);
            assert_eq!(query_ids(&conn, "@level:err or @level:error"), vec![2, 4, 8]);
            assert_eq!(query_ids(&conn, "not @service:web"), vec![3, 4, 5, 7]);
        }

        #[test]
        fn exec_quoted_vs_unquoted() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, r#""server error""#), vec![4]);
            assert_eq!(query_ids(&conn, "server error"), vec![4]);
            assert!(query_ids(&conn, r#""started port""#).is_empty());
            assert_eq!(query_ids(&conn, "started port"), vec![1]);
        }

        #[test]
        fn exec_param_offset() {
            let conn = setup_db();
            let dsl = log_dsl();
            // Simulate existing params by using offset=2
            let frag = dsl.parse("@service:web", 2).unwrap();
            assert_eq!(frag.sql, "service = ?3");
            let sql = format!("SELECT id FROM logs WHERE {} ORDER BY id", frag.sql);
            // Bind as if params 1-2 were already used (pass dummy + our params)
            let mut stmt = conn.prepare(&sql).unwrap();
            let ids: Vec<i64> = stmt.query_map(
                rusqlite::params_from_iter(
                    std::iter::repeat(&SqlValue::Text("unused".into())).take(2)
                        .chain(frag.params.iter())
                ),
                |row| row.get(0),
            ).unwrap().map(|r| r.unwrap()).collect();
            assert_eq!(ids, vec![1, 2, 6, 8]);
        }

        #[test]
        fn exec_single_quoted_phrase() {
            let conn = setup_db();
            assert_eq!(query_ids(&conn, "'server started'"), vec![1]);
        }

        #[test]
        fn exec_negative_timestamp_comparison() {
            let conn = setup_db();
            // All timestamps are >= 1000, so >-1 should match all
            assert_eq!(query_ids(&conn, "@timestamp:>-1"), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        }

        #[test]
        fn exec_negative_timestamp_range() {
            let conn = setup_db();
            // No rows with timestamp < -100
            assert!(query_ids(&conn, "@timestamp:<-100").is_empty());
        }

        #[test]
        fn exec_deeply_nested() {
            let conn = setup_db();
            assert_eq!(
                query_ids(&conn, "((@service:web OR @service:api) AND (@level:err OR @level:error))"),
                vec![2, 4, 8]
            );
        }

        #[test]
        fn exec_multiple_fts_terms() {
            let conn = setup_db();
            // "server" AND "error" → only row 4 matches both
            assert_eq!(query_ids(&conn, "server error"), vec![4]);
        }

        #[test]
        fn exec_fts_with_literal_percent() {
            let conn = setup_db();
            // No rows contain a literal '%' character
            assert!(query_ids(&conn, "100%").is_empty());
            // But 'port' should still match
            assert_eq!(query_ids(&conn, "port"), vec![1]);
        }

        // Real field execution tests
        fn setup_real_db() -> Connection {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch(
                "CREATE TABLE items (
                    id    INTEGER PRIMARY KEY,
                    name  TEXT NOT NULL,
                    score REAL,
                    w     REAL
                )",
            ).unwrap();
            conn.execute_batch(
                "INSERT INTO items VALUES (1, 'alpha', 3.14, 0.5);
                 INSERT INTO items VALUES (2, 'beta',  1.0,  2.0);
                 INSERT INTO items VALUES (3, 'gamma', -5.0, 10.0);
                 INSERT INTO items VALUES (4, 'delta', 42.0, 0.1);",
            ).unwrap();
            conn
        }

        fn query_real_ids(conn: &Connection, dsl_str: &str) -> Vec<i64> {
            let dsl = real_dsl();
            let frag = dsl.parse(dsl_str, 0).unwrap();
            let sql = format!("SELECT id FROM items WHERE {} ORDER BY id", frag.sql);
            let params: Vec<&dyn rusqlite::types::ToSql> =
                frag.params.iter().map(|p| p as &dyn rusqlite::types::ToSql).collect();
            let mut stmt = conn.prepare(&sql).unwrap();
            stmt.query_map(params.as_slice(), |row| row.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        }

        #[test]
        fn exec_real_field_exact() {
            let conn = setup_real_db();
            assert_eq!(query_real_ids(&conn, "@score:3.14"), vec![1]);
        }

        #[test]
        fn exec_real_field_comparison() {
            let conn = setup_real_db();
            assert_eq!(query_real_ids(&conn, "@score:>10"), vec![4]);
        }

        #[test]
        fn exec_real_field_negative() {
            let conn = setup_real_db();
            assert_eq!(query_real_ids(&conn, "@score:<0"), vec![3]);
        }

        #[test]
        fn exec_real_field_alias() {
            let conn = setup_real_db();
            assert_eq!(query_real_ids(&conn, "@weight:>1"), vec![2, 3]);
        }
    }
}
