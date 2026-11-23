//! A small JSON reader/writer (no dependency). Objects keep their key order
//! and numbers keep their original text, so a settings file can be edited and
//! written back without disturbing anything else in it.

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// The number exactly as written.
    Num(String),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// String value of `key`, or "".
    pub fn text(&self, key: &str) -> &str {
        self.get(key).and_then(Value::str).unwrap_or("")
    }

    /// Mutable access to `key` of an object, inserting `default` if it is missing.
    /// Returns `None` when `self` is not an object.
    pub fn entry(&mut self, key: &str, default: Value) -> Option<&mut Value> {
        let Value::Obj(items) = self else { return None };
        if !items.iter().any(|(k, _)| k == key) {
            items.push((key.to_string(), default));
        }
        items.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Pretty-printed with two-space indentation.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, Some(0));
        out.push('\n');
        out
    }

    pub fn compact(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, None);
        out
    }

    fn write(&self, out: &mut String, indent: Option<usize>) {
        let newline = |out: &mut String, level: usize| {
            if indent.is_some() {
                out.push('\n');
                out.push_str(&"  ".repeat(level));
            }
        };
        let level = indent.unwrap_or(0);
        let deeper = indent.map(|l| l + 1);
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) => out.push_str(n),
            Value::Str(s) => out.push_str(&quote(s)),
            Value::Arr(items) if items.is_empty() => out.push_str("[]"),
            Value::Obj(items) if items.is_empty() => out.push_str("{}"),
            Value::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(out, level + 1);
                    item.write(out, deeper);
                }
                newline(out, level);
                out.push(']');
            }
            Value::Obj(items) => {
                out.push('{');
                for (i, (key, value)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    newline(out, level + 1);
                    out.push_str(&quote(key));
                    out.push_str(if indent.is_some() { ": " } else { ":" });
                    value.write(out, deeper);
                }
                newline(out, level);
                out.push('}');
            }
        }
    }
}

/// `s` as a JSON string literal.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parses one JSON document. `None` on any syntax error or trailing content.
pub fn parse(text: &str) -> Option<Value> {
    let mut p = Parser { bytes: text.as_bytes(), pos: 0 };
    let value = p.value(0)?;
    p.space();
    (p.pos == p.bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.pos).is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, literal: &str) -> bool {
        let found = self.bytes[self.pos..].starts_with(literal.as_bytes());
        if found {
            self.pos += literal.len();
        }
        found
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > 64 {
            return None;
        }
        self.space();
        match *self.bytes.get(self.pos)? {
            b'{' => {
                self.pos += 1;
                let mut items = Vec::new();
                self.space();
                if self.eat("}") {
                    return Some(Value::Obj(items));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.space();
                    if !self.eat(":") {
                        return None;
                    }
                    items.push((key, self.value(depth + 1)?));
                    self.space();
                    if self.eat("}") {
                        return Some(Value::Obj(items));
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                self.space();
                if self.eat("]") {
                    return Some(Value::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.space();
                    if self.eat("]") {
                        return Some(Value::Arr(items));
                    }
                    if !self.eat(",") {
                        return None;
                    }
                }
            }
            b'"' => self.string().map(Value::Str),
            b't' => self.eat("true").then_some(Value::Bool(true)),
            b'f' => self.eat("false").then_some(Value::Bool(false)),
            b'n' => self.eat("null").then_some(Value::Null),
            _ => {
                let start = self.pos;
                while self.bytes.get(self.pos).is_some_and(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E')) {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
                text.parse::<f64>().ok().map(|_| Value::Num(text.to_string()))
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let digits = std::str::from_utf8(self.bytes.get(self.pos..self.pos + 4)?).ok()?;
        self.pos += 4;
        u32::from_str_radix(digits, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if !self.eat("\"") {
            return None;
        }
        let mut out = Vec::new();
        loop {
            let b = *self.bytes.get(self.pos)?;
            self.pos += 1;
            match b {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let escaped = *self.bytes.get(self.pos)?;
                    self.pos += 1;
                    let c = match escaped {
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let mut code = self.hex4()?;
                            // A surrogate pair encodes one character beyond the BMP.
                            if (0xD800..0xDC00).contains(&code) && self.eat("\\u") {
                                let low = self.hex4()?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            char::from_u32(code).unwrap_or('\u{FFFD}')
                        }
                        other => other as char,
                    };
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                other => out.push(other),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hook_payload() {
        let v = parse(r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"git push \"origin\"\n","timeout":120000},"list":[1,true,null]}"#).unwrap();
        assert_eq!(v.text("hook_event_name"), "PermissionRequest");
        assert_eq!(v.get("tool_input").unwrap().text("command"), "git push \"origin\"\n");
        assert_eq!(v.get("tool_input").unwrap().get("timeout"), Some(&Value::Num("120000".into())));
        assert_eq!(v.text("missing"), "");
    }

    #[test]
    fn round_trips_and_keeps_order() {
        let text = "{\n  \"model\": \"x\",\n  \"permissions\": {\n    \"allow\": [\n      \"Bash(ls:*)\"\n    ]\n  },\n  \"n\": 1.50,\n  \"empty\": {}\n}\n";
        assert_eq!(parse(text).unwrap().pretty(), text);
    }

    #[test]
    fn unicode_and_errors() {
        assert_eq!(parse(r#""café 😀""#), Some(Value::Str("café 😀".into())));
        assert_eq!(quote("a\"b\\c\n"), r#""a\"b\\c\n""#);
        assert_eq!(parse("{\"a\":1,}"), None);
        assert_eq!(parse("{} x"), None);
        assert_eq!(parse(""), None);
    }
}
