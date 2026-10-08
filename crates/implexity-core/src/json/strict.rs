// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Number, Value};

pub const MAX_DEPTH: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}: line {line} column {column} (char {offset})")]
pub struct JsonError {
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    pub reject_duplicate_keys: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self { reject_duplicate_keys: true }
    }
}


pub fn parse_strict(text: &str) -> Result<Value, JsonError> {
    parse_with(text, ParseOptions::default())
}


pub fn parse_strict_bytes(bytes: &[u8]) -> Result<Value, JsonError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => parse_strict(text),
        Err(e) => {
            let valid = e.valid_up_to();
            let prefix = String::from_utf8_lossy(&bytes[..valid]);
            Err(error_at(&prefix, prefix.len(), "document is not valid UTF-8"))
        }
    }
}


pub fn parse_with(text: &str, options: ParseOptions) -> Result<Value, JsonError> {
    parse_with_depth(text, options, MAX_DEPTH)
}



pub fn parse_with_depth(text: &str, options: ParseOptions, max_depth: usize) -> Result<Value, JsonError> {
    if text.starts_with('\u{feff}') {
        return Err(error_at(text, 0, "Unexpected UTF-8 BOM (decode using utf-8-sig)"));
    }
    let max_depth = max_depth.min(MAX_DEPTH);
    let mut p = Parser { text, bytes: text.as_bytes(), pos: 0, options, max_depth };
    p.skip_ws();
    let value = p.value(0)?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(p.err("Extra data"));
    }
    Ok(value)
}

fn error_at(text: &str, byte_pos: usize, message: &str) -> JsonError {
    let before = &text[..byte_pos.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..].chars().count() + 1;
    let offset = before.chars().count();
    JsonError { message: message.to_string(), line, column, offset }
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    options: ParseOptions,
    max_depth: usize,
}

impl Parser<'_> {
    fn err(&self, message: &str) -> JsonError {
        error_at(self.text, self.pos, message)
    }

    fn skip_ws(&mut self) {
        while let Some(&b) = self.bytes.get(self.pos) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth >= self.max_depth {
            return Err(self.err("maximum nesting depth exceeded"));
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b'N') if self.bytes[self.pos..].starts_with(b"NaN") => {
                Err(self.err("non-finite number NaN is not admissible"))
            }
            Some(b'I') if self.bytes[self.pos..].starts_with(b"Infinity") => {
                Err(self.err("non-finite number Infinity is not admissible"))
            }
            Some(b'-') if self.bytes[self.pos..].starts_with(b"-Infinity") => {
                Err(self.err("non-finite number -Infinity is not admissible"))
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            None | Some(_) => Err(self.err("Expecting value")),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, JsonError> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.err("Expecting value"))
        }
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => {
                self.pos = start;
                return Err(self.err("Expecting value"));
            }
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') && matches!(self.bytes.get(self.pos + 1), Some(b'0'..=b'9')) {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let mark = self.pos;
            let mut q = self.pos + 1;
            if matches!(self.bytes.get(q), Some(b'+' | b'-')) {
                q += 1;
            }
            if matches!(self.bytes.get(q), Some(b'0'..=b'9')) {
                while matches!(self.bytes.get(q), Some(b'0'..=b'9')) {
                    q += 1;
                }
                is_float = true;
                self.pos = q;
            } else {

                self.pos = mark;
            }
        }
        let token = &self.text[start..self.pos];
        if is_float {
            let parsed: f64 = token.parse().map_err(|_| error_at(self.text, start, "invalid number"))?;
            if !parsed.is_finite() {
                return Err(error_at(self.text, start, "number overflows to a non-finite value"));
            }
            Number::from_f64(parsed)
                .map(Value::Number)
                .ok_or_else(|| error_at(self.text, start, "non-finite number is not admissible"))
        } else if let Ok(i) = token.parse::<i64>() {
            Ok(Value::Number(Number::from(i)))
        } else if let Ok(u) = token.parse::<u64>() {
            Ok(Value::Number(Number::from(u)))
        } else {
            Err(error_at(self.text, start, "integer outside the 64-bit range is not supported"))
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let digits = self
            .bytes
            .get(self.pos..self.pos + 4)
            .filter(|d| d.iter().all(u8::is_ascii_hexdigit))
            .ok_or_else(|| self.err("Invalid \\uXXXX escape"))?;
        let mut v = 0u32;
        for &d in digits {
            v = v * 16 + char::from(d).to_digit(16).unwrap_or(0);
        }
        self.pos += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, JsonError> {

        let open = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let run_start = self.pos;
            while let Some(&b) = self.bytes.get(self.pos) {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.pos += 1;
            }
            out.push_str(&self.text[run_start..self.pos]);
            match self.peek() {
                None => {
                    return Err(error_at(self.text, open, "Unterminated string starting at"));
                }
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    let esc = self
                        .peek()
                        .ok_or_else(|| error_at(self.text, open, "Unterminated string starting at"))?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&first) {
                                if self.bytes.get(self.pos..self.pos + 2) == Some(b"\\u") {
                                    let save = self.pos;
                                    self.pos += 2;
                                    let second = self.hex4()?;
                                    if (0xdc00..0xe000).contains(&second) {
                                        0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                                    } else {
                                        self.pos = save;
                                        return Err(
                                            self.err("unpaired UTF-16 surrogate escape is not admissible")
                                        );
                                    }
                                } else {
                                    return Err(
                                        self.err("unpaired UTF-16 surrogate escape is not admissible")
                                    );
                                }
                            } else if (0xdc00..0xe000).contains(&first) {
                                return Err(self.err("unpaired UTF-16 surrogate escape is not admissible"));
                            } else {
                                first
                            };
                            match char::from_u32(code) {
                                Some(c) => out.push(c),
                                None => return Err(self.err("Invalid \\uXXXX escape")),
                            }
                        }
                        _ => {
                            self.pos -= 1;
                            return Err(self.err("Invalid \\escape"));
                        }
                    }
                }
                Some(_) => return Err(self.err("Invalid control character at")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.pos += 1;
        let mut out = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(out));
        }
        loop {
            self.skip_ws();
            out.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(b']') {
                        return Err(self.err("Illegal trailing comma before end of array"));
                    }
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(out));
                }
                _ => return Err(self.err("Expecting ',' delimiter")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.pos += 1;
        let mut out = Map::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(out));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("Expecting property name enclosed in double quotes"));
            }
            let key_pos = self.pos;
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.err("Expecting ':' delimiter"));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.value(depth + 1)?;
            if out.contains_key(&key) {
                if self.options.reject_duplicate_keys {
                    return Err(error_at(
                        self.text,
                        key_pos,
                        &format!("duplicate key {}", crate::py_repr::repr_str(&key)),
                    ));
                }

                if let Some(slot) = out.get_mut(&key) {
                    *slot = value;
                }
            } else {
                out.insert(key, value);
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(b'}') {
                        return Err(self.err("Illegal trailing comma before end of object"));
                    }
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(out));
                }
                _ => return Err(self.err("Expecting ',' delimiter")),
            }
        }
    }
}

