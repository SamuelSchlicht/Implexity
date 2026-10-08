// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};

use crate::error::{GResult, GeometryError};
use crate::pyfmt::{self, str_repr};

pub const FUNCS: [&str; 21] = [
    "min", "max", "abs", "sqrt", "floor", "ceil", "round", "sign", "clamp", "hypot", "exp", "log", "sin",
    "cos", "tan", "asin", "acos", "atan", "atan2", "radians", "degrees",
];

fn arity(name: &str) -> (usize, Option<usize>) {
    match name {
        "min" | "max" => (1, None),
        "round" | "log" => (1, Some(2)),
        "clamp" => (3, Some(3)),
        "hypot" | "atan2" => (2, Some(2)),
        _ => (1, Some(1)),
    }
}

pub const CONSTANTS: [(&str, f64); 3] =
    [("pi", std::f64::consts::PI), ("e", std::f64::consts::E), ("tau", 2.0 * std::f64::consts::PI)];

pub const OPERATORS: &str = "+-*/%^(),";

pub const MAX_EXPR_CHARS: usize = 512;

pub const MAX_EXPR_DEPTH: usize = 32;

fn constant(name: &str) -> Option<f64> {
    CONSTANTS.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

fn sorted_funcs() -> String {
    let mut v = FUNCS.to_vec();
    v.sort_unstable();
    v.join(", ")
}

#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    Num(f64, usize),
    Name(String, usize),
    Op(char, usize),
}

fn expr_err<T>(msg: String) -> GResult<T> {
    Err(GeometryError::Expr(msg))
}

fn char_repr(c: char) -> String {
    str_repr(&c.to_string())
}


pub fn tokenize(s: &str) -> GResult<Vec<Token>> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > MAX_EXPR_CHARS {
        return expr_err(format!("expression is {} characters; the cap is {}", chars.len(), MAX_EXPR_CHARS));
    }
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    let is_num = |c: char| c.is_ascii_digit() || c == '.';
    while i < n {
        let c = chars[i];
        if c == ' ' || c == '\t' {
            i += 1;
            continue;
        }
        if is_num(c) {
            let mut j = i;
            while j < n
                && (is_num(chars[j])
                    || chars[j] == 'e'
                    || chars[j] == 'E'
                    || ((chars[j] == '+' || chars[j] == '-') && (chars[j - 1] == 'e' || chars[j - 1] == 'E')))
            {
                j += 1;
            }
            let text: String = chars[i..j].iter().collect();
            let Some(val) = parse_py_float(&text) else {
                return expr_err(format!(
                    "{} at character {} is not a number (an expression has no attribute access and no member lookup: '.' only ever starts or continues a number)",
                    str_repr(&text),
                    i
                ));
            };
            if !val.is_finite() {
                return expr_err(format!("{} at character {} is not a finite number", str_repr(&text), i));
            }
            out.push(Token::Num(val, i));
            i = j;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let mut j = i;
            while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            out.push(Token::Name(chars[i..j].iter().collect(), i));
            i = j;
            continue;
        }
        if c == '*' && i + 1 < n && chars[i + 1] == '*' {
            out.push(Token::Op('^', i));
            i += 2;
            continue;
        }
        if OPERATORS.contains(c) {
            out.push(Token::Op(c, i));
            i += 1;
            continue;
        }
        return expr_err(format!(
            "character {} at position {} is not allowed in an expression; an expression is numbers, parameter names, the functions {}, and the operators + - * / % ^ ( ) ,",
            char_repr(c),
            i,
            sorted_funcs()
        ));
    }
    Ok(out)
}

fn parse_py_float(text: &str) -> Option<f64> {

    let bytes = text.as_bytes();
    let mut i = 0;
    let mut digits = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }
    text.parse::<f64>().ok()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Num(f64),
    Name(String),
    Call(String, Vec<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
    Neg(Box<Expr>),
}

impl Expr {
    #[must_use]
    pub fn py_repr(&self) -> String {
        match self {
            Self::Num(v) => format!("('num', {})", pyfmt::float_repr(*v)),
            Self::Name(n) => format!("('name', {})", str_repr(n)),
            Self::Call(n, args) => format!(
                "('call', {}, [{}])",
                str_repr(n),
                args.iter().map(Self::py_repr).collect::<Vec<_>>().join(", ")
            ),
            Self::Bin(op, a, b) => {
                format!("('bin', {}, {}, {})", str_repr(&op.to_string()), a.py_repr(), b.py_repr())
            }
            Self::Neg(a) => format!("('neg', {})", a.py_repr()),
        }
    }
}

struct Parser<'a> {
    t: Vec<Token>,
    i: usize,
    src: &'a str,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.t.get(self.i)
    }
    fn take(&mut self) -> Option<Token> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn is_op(&self, ch: char) -> bool {
        matches!(self.peek(), Some(Token::Op(c, _)) if *c == ch)
    }
    fn deep(&self) -> GResult<()> {
        expr_err(format!("expression nests deeper than {} in {}", MAX_EXPR_DEPTH, str_repr(self.src)))
    }
    fn where_(pos: Option<usize>) -> String {
        pos.map_or_else(|| " (it ended early)".to_string(), |p| format!(" at character {p}"))
    }
    fn expect_op(&mut self, ch: char) -> GResult<()> {
        if self.is_op(ch) {
            self.take();
            return Ok(());
        }
        let pos = self.peek().map(tok_pos);
        expr_err(format!("expected {} in {}{}", char_repr(ch), str_repr(self.src), Self::where_(pos)))
    }
    fn parse(&mut self) -> GResult<Expr> {
        let e = self.expr(0)?;
        if self.i != self.t.len() {
            let tok = self.peek().cloned();
            let (text, pos) = match tok {
                Some(Token::Num(v, p)) => (pyfmt::float_repr(v), p),
                Some(Token::Name(n, p)) => (str_repr(&n), p),
                Some(Token::Op(c, p)) => (char_repr(c), p),
                None => ("None".into(), 0),
            };
            return expr_err(format!("unexpected {} at character {} in {}", text, pos, str_repr(self.src)));
        }
        Ok(e)
    }
    fn expr(&mut self, depth: usize) -> GResult<Expr> {
        if depth > MAX_EXPR_DEPTH {
            self.deep()?;
        }
        let mut left = self.term(depth + 1)?;
        loop {
            match self.peek() {
                Some(Token::Op(c, _)) if *c == '+' || *c == '-' => {
                    let c = *c;
                    self.take();
                    left = Expr::Bin(c, Box::new(left), Box::new(self.term(depth + 1)?));
                }
                _ => return Ok(left),
            }
        }
    }
    fn term(&mut self, depth: usize) -> GResult<Expr> {
        if depth > MAX_EXPR_DEPTH {
            self.deep()?;
        }
        let mut left = self.power(depth + 1)?;
        loop {
            match self.peek() {
                Some(Token::Op(c, _)) if *c == '*' || *c == '/' || *c == '%' => {
                    let c = *c;
                    self.take();
                    left = Expr::Bin(c, Box::new(left), Box::new(self.power(depth + 1)?));
                }
                _ => return Ok(left),
            }
        }
    }
    fn power(&mut self, depth: usize) -> GResult<Expr> {
        let base = self.unary(depth + 1)?;
        if self.is_op('^') {
            self.take();
            return Ok(Expr::Bin('^', Box::new(base), Box::new(self.power(depth + 1)?)));
        }
        Ok(base)
    }
    fn unary(&mut self, depth: usize) -> GResult<Expr> {
        match self.peek() {
            Some(Token::Op(c, _)) if *c == '+' || *c == '-' => {
                let c = *c;
                self.take();
                let inner = self.unary(depth + 1)?;
                Ok(if c == '+' { inner } else { Expr::Neg(Box::new(inner)) })
            }
            _ => self.atom(depth + 1),
        }
    }
    fn atom(&mut self, depth: usize) -> GResult<Expr> {
        if depth > MAX_EXPR_DEPTH {
            self.deep()?;
        }
        let tok = self.take();
        match tok {
            Some(Token::Num(v, _)) => Ok(Expr::Num(v)),
            Some(Token::Op('(', _)) => {
                let e = self.expr(depth + 1)?;
                self.expect_op(')')?;
                Ok(e)
            }
            Some(Token::Name(name, pos)) => {
                if self.is_op('(') {
                    self.take();
                    let mut args = Vec::new();
                    if !self.is_op(')') {
                        args.push(self.expr(depth + 1)?);
                        while self.is_op(',') {
                            self.take();
                            args.push(self.expr(depth + 1)?);
                        }
                    }
                    self.expect_op(')')?;
                    if !FUNCS.contains(&name.as_str()) {
                        return expr_err(format!(
                            "{} at character {} is not a function an expression may call; the functions are {}",
                            str_repr(&name),
                            pos,
                            sorted_funcs()
                        ));
                    }
                    let (lo, hi) = arity(&name);
                    if args.len() < lo || hi.is_some_and(|h| args.len() > h) {
                        let want = match hi {
                            Some(h) if h == lo => lo.to_string(),
                            None => format!("{lo} or more"),
                            Some(h) => format!("{lo} to {h}"),
                        };
                        return expr_err(format!(
                            "{}() takes {} argument(s), got {}",
                            name,
                            want,
                            args.len()
                        ));
                    }
                    return Ok(Expr::Call(name, args));
                }
                Ok(Expr::Name(name))
            }
            other => {
                let pos = other.as_ref().map(tok_pos);
                expr_err(format!(
                    "expected a number, a name or '(' in {}{}",
                    str_repr(self.src),
                    Self::where_(pos)
                ))
            }
        }
    }
}

fn tok_pos(t: &Token) -> usize {
    match t {
        Token::Num(_, p) | Token::Name(_, p) | Token::Op(_, p) => *p,
    }
}


pub fn parse_expr(s: &str) -> GResult<Expr> {
    Parser { t: tokenize(s)?, i: 0, src: s }.parse()
}

#[must_use]
pub fn expr_names(tree: &Expr) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    fn rec(t: &Expr, out: &mut BTreeSet<String>) {
        match t {
            Expr::Name(n) => {
                if constant(n).is_none() {
                    out.insert(n.clone());
                }
            }
            Expr::Call(_, args) => args.iter().for_each(|a| rec(a, out)),
            Expr::Bin(_, a, b) => {
                rec(a, out);
                rec(b, out);
            }
            Expr::Neg(a) => rec(a, out),
            Expr::Num(_) => {}
        }
    }
    rec(tree, &mut out);
    out
}

fn finite(v: f64, what: &str) -> GResult<f64> {
    if v.is_finite() { Ok(v) } else { expr_err(format!("{what} produced {}", pyfmt::float_repr(v))) }
}

fn py_round(x: f64, n: i64) -> Result<f64, String> {
    if !x.is_finite() {
        return Ok(x);
    }
    if n > 308 {
        return Ok(x);
    }
    if n < -308 {
        return Ok(0.0 * x);
    }
    if n >= 0 {

        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let n = n as usize;
        let s = format!("{:.1100}", x.abs());
        let (ip, fp) = s.split_once('.').unwrap_or((s.as_str(), ""));
        let mut digits: Vec<u8> = ip.bytes().chain(fp.bytes().take(n)).map(|b| b - b'0').collect();
        let rest: Vec<u8> = fp.bytes().skip(n).map(|b| b - b'0').collect();
        let first = rest.first().copied().unwrap_or(0);
        let tail_nonzero = rest.iter().skip(1).any(|d| *d != 0);
        let last_odd = digits.last().is_some_and(|d| d % 2 == 1);
        let up = first > 5 || (first == 5 && (tail_nonzero || last_odd));
        if up {
            let mut k = digits.len();
            loop {
                if k == 0 {
                    digits.insert(0, 1);
                    break;
                }
                k -= 1;
                if digits[k] == 9 {
                    digits[k] = 0;
                } else {
                    digits[k] += 1;
                    break;
                }
            }
        }
        let int_len = digits.len() - n;
        let mut text: String = digits[..int_len].iter().map(|d| char::from(b'0' + d)).collect();
        if n > 0 {
            text.push('.');
            text.extend(digits[int_len..].iter().map(|d| char::from(b'0' + d)));
        }
        let v: f64 = text.parse().map_err(|_| "rounded value is not a number".to_string())?;
        return Ok(if x < 0.0 { -v } else { v });
    }

    #[allow(clippy::cast_possible_truncation)]
    let p = 10f64.powi((-n) as i32);
    let q = x / p;
    let r = q.round_ties_even();
    let y = r * p;
    if !y.is_finite() {
        return Err("rounded value too large to represent".into());
    }
    Ok(y)
}

fn call(name: &str, a: &[f64]) -> Result<f64, String> {
    let dom = || "math domain error".to_string();
    Ok(match name {
        "min" => a.iter().copied().fold(f64::INFINITY, f64::min),
        "max" => a.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        "abs" => a[0].abs(),
        "sqrt" => {
            if a[0] < 0.0 {
                return Err(dom());
            }
            a[0].sqrt()
        }
        "floor" => a[0].floor(),
        "ceil" => a[0].ceil(),
        "round" => {
            #[allow(clippy::cast_possible_truncation)]
            let n = if a.len() > 1 { a[1].trunc() as i64 } else { 0 };
            py_round(a[0], n)?
        }
        "sign" => f64::from(i8::from(a[0] > 0.0) - i8::from(a[0] < 0.0)),
        "clamp" => a[0].max(a[1]).min(a[2]),
        "hypot" => a[0].hypot(a[1]),
        "exp" => {
            let v = a[0].exp();
            if v.is_infinite() {
                return Err("math range error".into());
            }
            v
        }
        "log" => {
            if a[0] <= 0.0 {
                return Err(dom());
            }
            if a.len() > 1 {
                if a[1] <= 0.0 {
                    return Err(dom());
                }
                let d = a[1].ln();
                if d == 0.0 {
                    return Err("float division by zero".into());
                }
                a[0].ln() / d
            } else {
                a[0].ln()
            }
        }
        "sin" | "cos" | "tan" => {
            if a[0].is_infinite() {
                return Err(dom());
            }
            match name {
                "sin" => a[0].sin(),
                "cos" => a[0].cos(),
                _ => a[0].tan(),
            }
        }
        "asin" | "acos" => {
            if !(-1.0..=1.0).contains(&a[0]) {
                return Err(dom());
            }
            if name == "asin" { a[0].asin() } else { a[0].acos() }
        }
        "atan" => a[0].atan(),
        "atan2" => a[0].atan2(a[1]),
        "radians" => a[0] * (std::f64::consts::PI / 180.0),
        "degrees" => a[0] * (180.0 / std::f64::consts::PI),
        _ => return Err(format!("unknown function {name}")),
    })
}


pub fn eval_tree(tree: &Expr, env: &BTreeMap<String, f64>) -> GResult<f64> {
    eval_depth(tree, env, 0)
}

fn eval_depth(tree: &Expr, env: &BTreeMap<String, f64>, depth: usize) -> GResult<f64> {
    if depth > MAX_EXPR_DEPTH {
        return expr_err(format!("expression nests deeper than {MAX_EXPR_DEPTH}"));
    }
    match tree {
        Expr::Num(v) => Ok(*v),
        Expr::Name(name) => {
            if let Some(c) = constant(name) {
                return Ok(c);
            }
            env.get(name).copied().ok_or_else(|| {
                let names: Vec<&str> = env.keys().map(String::as_str).collect();
                GeometryError::Expr(format!(
                    "unknown name {}; the names an expression may read are the document's parameters ({}) and the constants e, pi, tau",
                    str_repr(name),
                    if names.is_empty() { "none".to_string() } else { names.join(", ") }
                ))
            })
        }
        Expr::Neg(a) => Ok(-eval_depth(a, env, depth + 1)?),
        Expr::Call(name, args) => {
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval_depth(a, env, depth + 1)?);
            }
            match call(name, &vals) {
                Ok(v) => finite(v, &format!("{name}()")),
                Err(msg) => expr_err(format!(
                    "{}({}) failed: {}",
                    name,
                    vals.iter().map(|v| pyfmt::g(*v)).collect::<Vec<_>>().join(", "),
                    msg
                )),
            }
        }
        Expr::Bin(op, a, b) => {
            let a = eval_depth(a, env, depth + 1)?;
            let b = eval_depth(b, env, depth + 1)?;
            match op {
                '+' => finite(a + b, "+"),
                '-' => finite(a - b, "-"),
                '*' => finite(a * b, "*"),
                '/' | '%' => {
                    if b == 0.0 {
                        return expr_err("division by zero".into());
                    }
                    let v = if *op == '/' { a / b } else { a % b };
                    finite(v, &op.to_string())
                }
                '^' => {
                    if a == 0.0 && b < 0.0 {
                        return expr_err("0.0 cannot be raised to a negative power".into());
                    }
                    if a < 0.0 && b.fract() != 0.0 && b.is_finite() {
                        return expr_err(format!("{} ^ {} is not a real number", pyfmt::g(a), pyfmt::g(b)));
                    }
                    let v = a.powf(b);
                    if v.is_infinite() && a.is_finite() && b.is_finite() {
                        return expr_err(format!(
                            "{} ^ {}: (34, 'Numerical result out of range')",
                            pyfmt::g(a),
                            pyfmt::g(b)
                        ));
                    }
                    finite(v, "^")
                }
                other => expr_err(format!("unreachable expression node {}", str_repr(&other.to_string()))),
            }
        }
    }
}


pub fn evaluate_expr(s: &str, env: &BTreeMap<String, f64>) -> GResult<f64> {
    eval_tree(&parse_expr(s)?, env)
}


pub fn eval_tree_directional(
    tree: &Expr,
    env: &BTreeMap<String, f64>,
    direction: &BTreeMap<String, f64>,
) -> GResult<(f64, f64)> {
    use implexity_ad::{Dual, Scalar};
    fn walk(tree: &Expr, env: &BTreeMap<String, f64>, direction: &BTreeMap<String, f64>) -> GResult<Dual<1>> {
        let out = match tree {
            Expr::Num(v) => Dual::constant(*v),
            Expr::Name(name) => match constant(name) {
                Some(v) => Dual::constant(v),
                None => Dual::new(*env.get(name).ok_or_else(|| GeometryError::Expr(format!("unknown parameter {name}")))?, [direction.get(name).copied().unwrap_or(0.0)]),
            },
            Expr::Neg(a) => -walk(a, env, direction)?,
            Expr::Bin(op, a, b) => {
                let a = walk(a, env, direction)?;
                let b = walk(b, env, direction)?;
                if a.eps[0] == 0.0 && b.eps[0] == 0.0 {
                    return Ok(Dual::constant(eval_tree(tree, env)?));
                }
                match op {
                    '+' => a + b,
                    '-' => a - b,
                    '*' => a * b,
                    '/' => a / b,
                    '%' => a - b * (a.re / b.re).trunc(),
                    '^' if b.eps[0] == 0.0 && b.re == 0.0 => Dual::constant(1.0),
                    '^' if b.eps[0] == 0.0 => a.powf(b.re),
                    '^' => a.pow(b),
                    _ => return expr_err("unsupported expression operator".into()),
                }
            }
            Expr::Call(name, args) => {
                let a = args.iter().map(|a| walk(a, env, direction)).collect::<GResult<Vec<_>>>()?;
                if a.iter().all(|value| value.eps[0] == 0.0) {
                    return Ok(Dual::constant(call(name, &a.iter().map(|a| a.re).collect::<Vec<_>>()).map_err(GeometryError::Expr)?));
                }
                match name.as_str() {
                    "min" => a.iter().copied().reduce(Scalar::minimum).ok_or_else(|| GeometryError::Expr("empty min".into()))?,
                    "max" => a.iter().copied().reduce(Scalar::maximum).ok_or_else(|| GeometryError::Expr("empty max".into()))?,
                    "abs" => a[0].abs(),
                    "sqrt" => a[0].sqrt(),
                    "floor" | "ceil" | "round" | "sign" => Dual::constant(call(name, &a.iter().map(|a| a.re).collect::<Vec<_>>()).map_err(GeometryError::Expr)?),
                    "clamp" => a[0].maximum(a[1]).minimum(a[2]),
                    "hypot" => a[0].hypot(a[1]),
                    "exp" => a[0].exp(),
                    "log" if a.len() == 2 => a[0].ln() / a[1].ln(),
                    "log" => a[0].ln(),
                    "sin" => a[0].sin(),
                    "cos" => a[0].cos(),
                    "tan" => a[0].tan(),
                    "asin" => a[0].asin(),
                    "acos" => a[0].acos(),
                    "atan" => a[0].atan(),
                    "atan2" => a[0].atan2(a[1]),
                    "radians" => a[0] * (std::f64::consts::PI / 180.0),
                    "degrees" => a[0] * (180.0 / std::f64::consts::PI),
                    _ => return expr_err(format!("unsupported expression function {name}")),
                }
            }
        };
        Ok(out)
    }
    let value = eval_tree(tree, env)?;
    let derivative = walk(tree, env, direction)?.eps[0];
    if !derivative.is_finite() {
        return expr_err("expression derivative is not finite".into());
    }
    Ok((value, derivative))
}
