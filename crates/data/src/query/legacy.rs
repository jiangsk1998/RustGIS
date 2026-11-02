//! The WHERE clause parser and evaluator as they were before the expression language, kept
//! verbatim as the oracle of the differential tests (every clause it accepts must select the
//! same rows through the new one) but for the one rule the expression language changes: a NULL
//! in an IN list matches nothing, where the text `<Null>` once matched it.
#![allow(dead_code)]

use crate::{Dataset, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Cmp { field: usize, op: Op, lit: Value },
    Like { field: usize, pattern: String, negate: bool },
    IsNull { field: usize, negate: bool },
    In { field: usize, values: Vec<Value>, negate: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    /// A double-quoted field name: never a keyword or literal.
    QuotedIdent(String),
    Str(String),
    Num(f64),
    Op(String),
    LParen,
    RParen,
    Comma,
}

/// Longest clause accepted (characters).
pub const MAX_LEN: usize = 10_000;
/// Deepest nesting accepted.
const MAX_DEPTH: usize = 64;
/// Most terms in one AND/OR chain (chains nest left, so this also bounds recursion).
const MAX_TERMS: usize = 256;

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    if s.chars().count() > MAX_LEN {
        return Err("the expression is too long".into());
    }
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while let Some(&c) = cs.get(i) {
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1
            }
            '\'' | '"' => {
                let q = c;
                let mut v = String::new();
                i += 1;
                loop {
                    match cs.get(i) {
                        None => return Err("unterminated quote".into()),
                        Some(&ch) if ch == q => {
                            if cs.get(i + 1) == Some(&q) {
                                v.push(q);
                                i += 2;
                            } else {
                                i += 1;
                                break;
                            }
                        }
                        Some(&ch) => {
                            v.push(ch);
                            i += 1
                        }
                    }
                }
                out.push(if q == '"' { Tok::QuotedIdent(v) } else { Tok::Str(v) });
            }
            '=' | '<' | '>' | '!' => {
                let mut op = c.to_string();
                if let Some(&n) = cs.get(i + 1)
                    && (n == '=' || (c == '<' && n == '>'))
                {
                    op.push(n);
                    i += 1;
                }
                i += 1;
                if op == "!" {
                    return Err("unexpected '!'".into());
                }
                out.push(Tok::Op(op));
            }
            c if c.is_ascii_digit() || c == '.' || c == '-' => {
                let st = i;
                i += 1;
                while cs.get(i).is_some_and(|c| c.is_ascii_digit() || *c == '.' || *c == 'e' || *c == 'E') {
                    i += 1;
                }
                let t: String = cs.get(st..i).map(|s| s.iter().collect()).unwrap_or_default();
                out.push(Tok::Num(t.parse().map_err(|_| format!("bad number '{t}'"))?));
            }
            c if c.is_alphanumeric() || c == '_' => {
                let st = i;
                while cs.get(i).is_some_and(|c| c.is_alphanumeric() || *c == '_') {
                    i += 1;
                }
                out.push(Tok::Ident(cs.get(st..i).map(|s| s.iter().collect()).unwrap_or_default()));
            }
            other => return Err(format!("unexpected '{other}'")),
        }
    }
    Ok(out)
}

struct Parser<'a> {
    toks: Vec<Tok>,
    pos: usize,
    ds: &'a Dataset,
    depth: usize,
}

impl Parser<'_> {
    fn peek_kw(&self, kw: &str) -> bool {
        matches!(self.toks.get(self.pos), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(kw))
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        let y = self.peek_kw(kw);
        if y {
            self.pos += 1;
        }
        y
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn or(&mut self) -> Result<Expr, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("the expression is nested too deeply".into());
        }
        let mut l = self.and()?;
        let mut terms = 1;
        while self.eat_kw("OR") {
            terms += 1;
            if terms > MAX_TERMS {
                return Err("the expression has too many terms".into());
            }
            l = Expr::Or(Box::new(l), Box::new(self.and()?));
        }
        self.depth -= 1;
        Ok(l)
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.not()?;
        let mut terms = 1;
        while self.eat_kw("AND") {
            terms += 1;
            if terms > MAX_TERMS {
                return Err("the expression has too many terms".into());
            }
            l = Expr::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.eat_kw("NOT") {
            self.depth += 1;
            if self.depth > MAX_DEPTH {
                return Err("the expression is nested too deeply".into());
            }
            let e = self.not()?;
            self.depth -= 1;
            return Ok(Expr::Not(Box::new(e)));
        }
        self.cmp()
    }
    fn literal(&mut self) -> Result<Value, String> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(Value::Text(s)),
            Some(Tok::Num(n)) => Ok(if n.fract() == 0.0 && n.abs() < 9e15 { Value::Int(n as i64) } else { Value::Float(n) }),
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("true") => Ok(Value::Bool(true)),
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("false") => Ok(Value::Bool(false)),
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("null") => Ok(Value::Null),
            other => Err(format!("expected a value, found {}", show(other.as_ref()))),
        }
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        if self.toks.get(self.pos) == Some(&Tok::LParen) {
            self.pos += 1;
            let e = self.or()?;
            if self.next() != Some(Tok::RParen) {
                return Err("missing ')'".into());
            }
            return Ok(e);
        }
        let name = match self.next() {
            Some(Tok::Ident(s)) | Some(Tok::QuotedIdent(s)) => s,
            other => return Err(format!("expected a field name, found {}", show(other.as_ref()))),
        };
        let field = self.ds.field_index(&name).ok_or_else(|| format!("no field named '{name}'"))?;
        if self.eat_kw("IS") {
            let negate = self.eat_kw("NOT");
            if !self.eat_kw("NULL") {
                return Err("expected NULL after IS".into());
            }
            return Ok(Expr::IsNull { field, negate });
        }
        let negate = self.eat_kw("NOT");
        if self.eat_kw("LIKE") {
            return match self.next() {
                Some(Tok::Str(p)) => Ok(Expr::Like { field, pattern: p, negate }),
                other => Err(format!("expected a quoted pattern after LIKE, found {}", show(other.as_ref()))),
            };
        }
        if self.eat_kw("IN") {
            if self.next() != Some(Tok::LParen) {
                return Err("expected '(' after IN".into());
            }
            let mut values = vec![self.literal()?];
            loop {
                match self.next() {
                    Some(Tok::Comma) => values.push(self.literal()?),
                    Some(Tok::RParen) => break,
                    other => return Err(format!("expected ',' or ')', found {}", show(other.as_ref()))),
                }
            }
            return Ok(Expr::In { field, values, negate });
        }
        if negate {
            return Err("expected LIKE or IN after NOT".into());
        }
        let op = match self.next() {
            Some(Tok::Op(o)) => match o.as_str() {
                "=" | "==" => Op::Eq,
                "<>" | "!=" => Op::Ne,
                "<" => Op::Lt,
                "<=" => Op::Le,
                ">" => Op::Gt,
                ">=" => Op::Ge,
                _ => return Err(format!("unknown operator '{o}'")),
            },
            other => return Err(format!("expected an operator, found {}", show(other.as_ref()))),
        };
        Ok(Expr::Cmp { field, op, lit: self.literal()? })
    }
}

fn show(t: Option<&Tok>) -> String {
    match t {
        None => "the end".into(),
        Some(Tok::Ident(s)) => format!("'{s}'"),
        Some(Tok::QuotedIdent(s)) => format!("\"{s}\""),
        Some(Tok::Str(s)) => format!("'{s}'"),
        Some(Tok::Num(n)) => n.to_string(),
        Some(Tok::Op(o)) => format!("'{o}'"),
        Some(Tok::LParen) => "'('".into(),
        Some(Tok::RParen) => "')'".into(),
        Some(Tok::Comma) => "','".into(),
    }
}

/// Parses a clause against a dataset's schema.
pub fn parse(text: &str, ds: &Dataset) -> Result<Expr, String> {
    let toks = lex(text)?;
    if toks.is_empty() {
        return Err("the expression is empty".into());
    }
    let mut p = Parser { toks, pos: 0, ds, depth: 0 };
    let e = p.or()?;
    if p.pos < p.toks.len() {
        return Err(format!("unexpected {}", show(p.toks.get(p.pos))));
    }
    Ok(e)
}

fn eq(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if !matches!(a, Value::Text(_)) || !matches!(b, Value::Text(_)) => x == y,
        _ => a.to_string().eq_ignore_ascii_case(&b.to_string()),
    }
}

/// SQL LIKE with `%` and `_`, case-insensitive.
pub fn like(text: &str, pat: &str) -> bool {
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    // Iterative wildcard matching with backtracking on the last '%'.
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        match p.get(pi) {
            Some('%') => {
                star = Some(pi);
                mark = ti;
                pi += 1;
            }
            Some(c) if *c == '_' || Some(c) == t.get(ti) => {
                ti += 1;
                pi += 1;
            }
            _ => match star {
                Some(s) => {
                    pi = s + 1;
                    mark += 1;
                    ti = mark;
                }
                None => return false,
            },
        }
    }
    p.get(pi..).is_some_and(|rest| rest.iter().all(|c| *c == '%'))
}

/// Evaluates for one row: whether the row matches.
pub fn eval(e: &Expr, ds: &Dataset, row: usize) -> bool {
    eval3(e, ds, row) == Some(true)
}

/// SQL's three-valued logic: `None` is "unknown", what a comparison with a null gives. `NOT`
/// leaves it unknown, so `NOT (pop > 100)` doesn't match a row whose `pop` is null.
fn eval3(e: &Expr, ds: &Dataset, row: usize) -> Option<bool> {
    match e {
        Expr::And(a, b) => match (eval3(a, ds, row), eval3(b, ds, row)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
        Expr::Or(a, b) => match (eval3(a, ds, row), eval3(b, ds, row)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        Expr::Not(a) => eval3(a, ds, row).map(|v| !v),
        Expr::IsNull { field, negate } => Some(ds.value(row, *field).is_null() != *negate),
        Expr::Like { field, pattern, negate } => {
            let v = ds.value(row, *field);
            (!v.is_null()).then(|| like(&v.to_string(), pattern) != *negate)
        }
        Expr::In { field, values, negate } => {
            let v = ds.value(row, *field);
            (!v.is_null()).then(|| values.iter().any(|x| !x.is_null() && eq(v, x)) != *negate)
        }
        Expr::Cmp { field, op, lit } => {
            let v = ds.value(row, *field);
            if v.is_null() || lit.is_null() {
                return None;
            }
            Some(match op {
                Op::Eq => eq(v, lit),
                Op::Ne => !eq(v, lit),
                _ => match v.partial_cmp(lit) {
                    Some(o) => match op {
                        Op::Lt => o.is_lt(),
                        Op::Le => o.is_le(),
                        Op::Gt => o.is_gt(),
                        Op::Ge => o.is_ge(),
                        _ => false,
                    },
                    None => false,
                },
            })
        }
    }
}

/// Rows matching the clause.
pub fn select(text: &str, ds: &Dataset) -> Result<Vec<usize>, String> {
    let e = parse(text, ds)?;
    Ok((0..ds.features.len()).filter(|r| eval(&e, ds, *r)).collect())
}
