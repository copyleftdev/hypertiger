//! A strict JSON reader that allocates nothing and does not recurse.
//!
//! Parsing writes a flat tape of tokens into a buffer the caller allocated at startup. Each token
//! records where its subtree ends, so skipping a field is one step. Nothing is decoded until it is
//! asked for: a field this crate cannot hold exactly fails on the read that wanted it, not on the
//! message that carried it. Nesting uses a fixed stack, so a hostile `[[[[...` is an error and
//! never a stack overflow.

/// The deepest nesting accepted. The deepest response measured (`orderStatus`, with children) is
/// six levels; the margin is for shapes the exchange adds later.
pub const DEPTH_MAX: usize = 32;
/// A byte offset is `u32`. Nothing the exchange sends approaches 4 GiB.
pub const SOURCE_SIZE_MAX: usize = u32::MAX as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonError {
    TooLarge,
    /// The tape the caller allocated is too small for this document.
    TokensExhausted,
    TooDeep,
    /// Bytes that are not JSON, with where they were found.
    Malformed {
        offset: u32,
    },
    /// The document ended early, which on a stream can mean a torn frame.
    Truncated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Null,
    False,
    True,
    Number,
    String,
    Array,
    Object,
}

/// One value on the tape, 20 bytes. A key is a `String` token directly before its value's.
#[derive(Clone, Copy, Debug)]
pub struct Token {
    kind: Kind,
    /// Whether a string holds a backslash and so differs from its raw bytes.
    escaped: bool,
    start: u32,
    end: u32,
    /// The index just past this token's subtree.
    next: u32,
    /// How many values this container holds. An object's entries count as one each.
    count: u32,
}

impl Token {
    pub const ZERO: Token =
        Token { kind: Kind::Null, escaped: false, start: 0, end: 0, next: 0, count: 0 };
}

const _: () = assert!(size_of::<Token>() <= 24);

/// A parsed document: the source and its tape, both borrowed.
#[derive(Clone, Copy, Debug)]
pub struct Document<'s, 't> {
    source: &'s [u8],
    tokens: &'t [Token],
}

impl<'s, 't> Document<'s, 't> {
    /// The root value. A parsed document always has exactly one.
    #[must_use]
    pub fn root(self) -> Value<'s, 't> {
        assert!(!self.tokens.is_empty());
        assert_eq!(self.tokens[0].next as usize, self.tokens.len());
        Value { document: self, index: 0 }
    }
}

/// What the parser is waiting for next.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Value,
    /// An array's first value, or its close.
    ValueOrClose,
    /// An object's first key, or its close.
    KeyOrClose,
    Key,
    Colon,
    /// A comma or the close of the current container.
    CommaOrClose,
    /// The root is complete, and only whitespace may follow.
    End,
}

struct Parser<'s, 't> {
    source: &'s [u8],
    tokens: &'t mut [Token],
    tokens_count: u32,
    cursor: usize,
    stack: [u32; DEPTH_MAX],
    depth: usize,
}

/// Parses `source` onto `tokens`, returning a view of the part used.
pub fn parse<'s, 't>(
    source: &'s [u8],
    tokens: &'t mut [Token],
) -> Result<Document<'s, 't>, JsonError> {
    if source.len() > SOURCE_SIZE_MAX {
        return Err(JsonError::TooLarge);
    }
    assert!(u32::try_from(tokens.len()).is_ok());
    let mut parser =
        Parser { source, tokens, tokens_count: 0, cursor: 0, stack: [0; DEPTH_MAX], depth: 0 };
    parser.run()?;

    let count = parser.tokens_count as usize;
    assert!(count > 0);
    assert_eq!(parser.depth, 0);
    let Parser { tokens, .. } = parser;
    Ok(Document { source, tokens: &tokens[..count] })
}

impl Parser<'_, '_> {
    fn run(&mut self) -> Result<(), JsonError> {
        let mut expect = Expect::Value;
        // Every pass consumes at least one byte or returns, so the source length bounds the loop.
        loop {
            self.skip_whitespace();
            let Some(&byte) = self.source.get(self.cursor) else {
                return if expect == Expect::End { Ok(()) } else { Err(JsonError::Truncated) };
            };
            let cursor_before = self.cursor;
            expect = self.step(expect, byte)?;
            assert!(self.cursor > cursor_before);
        }
    }

    fn step(&mut self, expect: Expect, byte: u8) -> Result<Expect, JsonError> {
        match expect {
            Expect::End => self.malformed(),
            Expect::Colon => {
                if byte == b':' {
                    self.cursor += 1;
                    Ok(Expect::Value)
                } else {
                    self.malformed()
                }
            }
            Expect::CommaOrClose => self.step_comma_or_close(byte),
            Expect::Key | Expect::KeyOrClose => {
                if byte == b'}' && expect == Expect::KeyOrClose {
                    self.close(b'}')
                } else if byte == b'"' {
                    self.string()?;
                    // The key sits on the tape; its count stays with the object it belongs to.
                    Ok(Expect::Colon)
                } else {
                    self.malformed()
                }
            }
            Expect::ValueOrClose if byte == b']' => self.close(b']'),
            Expect::Value | Expect::ValueOrClose => self.value(byte),
        }
    }

    fn step_comma_or_close(&mut self, byte: u8) -> Result<Expect, JsonError> {
        assert!(self.depth > 0);
        let container = self.tokens[self.stack[self.depth - 1] as usize].kind;
        match byte {
            b',' => {
                self.cursor += 1;
                Ok(if container == Kind::Object { Expect::Key } else { Expect::Value })
            }
            b']' | b'}' => self.close(byte),
            _ => self.malformed(),
        }
    }

    fn value(&mut self, byte: u8) -> Result<Expect, JsonError> {
        match byte {
            b'{' => self.open(Kind::Object),
            b'[' => self.open(Kind::Array),
            b'"' => {
                self.string()?;
                Ok(self.after_value(1))
            }
            b't' => self.literal(b"true", Kind::True),
            b'f' => self.literal(b"false", Kind::False),
            b'n' => self.literal(b"null", Kind::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => self.malformed(),
        }
    }

    /// Bookkeeping once a whole value is done. `count_token` is 1 for a scalar, whose `next` is
    /// the following token, and 0 for a container, whose `next` was set when it closed.
    fn after_value(&mut self, count_token: u32) -> Expect {
        if count_token == 1 {
            let index = self.tokens_count as usize - 1;
            self.tokens[index].next = self.tokens_count;
        }
        if self.depth == 0 {
            Expect::End
        } else {
            let parent = self.stack[self.depth - 1] as usize;
            self.tokens[parent].count += 1;
            Expect::CommaOrClose
        }
    }

    fn open(&mut self, kind: Kind) -> Result<Expect, JsonError> {
        if self.depth == DEPTH_MAX {
            return Err(JsonError::TooDeep);
        }
        let index = self.push_token(kind, self.cursor, self.cursor + 1, false)?;
        self.stack[self.depth] = index;
        self.depth += 1;
        self.cursor += 1;
        Ok(if kind == Kind::Object { Expect::KeyOrClose } else { Expect::ValueOrClose })
    }

    fn close(&mut self, bracket: u8) -> Result<Expect, JsonError> {
        assert!(self.depth > 0);
        let index = self.stack[self.depth - 1] as usize;
        let expected = if self.tokens[index].kind == Kind::Object { b'}' } else { b']' };
        if bracket != expected {
            return self.malformed();
        }
        self.depth -= 1;
        self.cursor += 1;
        self.tokens[index].end = self.cursor as u32;
        self.tokens[index].next = self.tokens_count;
        assert!(self.tokens[index].next > index as u32);
        Ok(self.after_value(0))
    }

    fn push_token(
        &mut self,
        kind: Kind,
        start: usize,
        end: usize,
        escaped: bool,
    ) -> Result<u32, JsonError> {
        let index = self.tokens_count;
        let slot = self.tokens.get_mut(index as usize).ok_or(JsonError::TokensExhausted)?;
        assert!(start <= end);
        assert!(end <= SOURCE_SIZE_MAX);
        *slot = Token { kind, escaped, start: start as u32, end: end as u32, next: 0, count: 0 };
        self.tokens_count += 1;
        Ok(index)
    }

    fn string(&mut self) -> Result<(), JsonError> {
        assert_eq!(self.source[self.cursor], b'"');
        let start = self.cursor + 1;
        let mut index = start;
        let mut escaped = false;
        // Bounded by the source: each pass advances `index`.
        loop {
            let Some(&byte) = self.source.get(index) else {
                return Err(JsonError::Truncated);
            };
            match byte {
                b'"' => break,
                b'\\' => {
                    escaped = true;
                    index = self.escape(index)?;
                }
                0x00..=0x1f => return Err(malformed_at(index)),
                _ => index += 1,
            }
        }
        let token = self.push_token(Kind::String, start, index, escaped)?;
        self.tokens[token as usize].next = self.tokens_count;
        self.cursor = index + 1;
        Ok(())
    }

    /// Validates the escape at `index`, returning the index past it.
    fn escape(&self, index: usize) -> Result<usize, JsonError> {
        assert_eq!(self.source[index], b'\\');
        match self.source.get(index + 1) {
            None => Err(JsonError::Truncated),
            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => Ok(index + 2),
            Some(b'u') => {
                let digits = self.source.get(index + 2..index + 6).ok_or(JsonError::Truncated)?;
                if digits.iter().all(u8::is_ascii_hexdigit) {
                    Ok(index + 6)
                } else {
                    Err(malformed_at(index))
                }
            }
            Some(_) => Err(malformed_at(index)),
        }
    }

    fn literal(&mut self, text: &[u8], kind: Kind) -> Result<Expect, JsonError> {
        let end = self.cursor + text.len();
        match self.source.get(self.cursor..end) {
            None => Err(JsonError::Truncated),
            Some(found) if found == text => {
                self.push_token(kind, self.cursor, end, false)?;
                self.cursor = end;
                Ok(self.after_value(1))
            }
            Some(_) => self.malformed(),
        }
    }

    /// The grammar of RFC 8259: no leading zeros, no bare point, no leading plus.
    fn number(&mut self) -> Result<Expect, JsonError> {
        let start = self.cursor;
        let mut index = start;
        if self.source[index] == b'-' {
            index += 1;
        }
        match self.source.get(index) {
            None => return Err(JsonError::Truncated),
            Some(b'0') => index += 1,
            Some(b'1'..=b'9') => index = digits_end(self.source, index),
            Some(_) => return Err(malformed_at(index)),
        }
        if self.source.get(index) == Some(&b'.') {
            let after = digits_end(self.source, index + 1);
            if after == index + 1 {
                return Err(malformed_at(index));
            }
            index = after;
        }
        if matches!(self.source.get(index), Some(b'e' | b'E')) {
            index += 1;
            if matches!(self.source.get(index), Some(b'+' | b'-')) {
                index += 1;
            }
            let after = digits_end(self.source, index);
            if after == index {
                return Err(malformed_at(index));
            }
            index = after;
        }
        assert!(index > start);
        self.push_token(Kind::Number, start, index, false)?;
        self.cursor = index;
        Ok(self.after_value(1))
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.source.get(self.cursor), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.cursor += 1;
        }
    }

    fn malformed<T>(&self) -> Result<T, JsonError> {
        Err(malformed_at(self.cursor))
    }
}

fn malformed_at(offset: usize) -> JsonError {
    assert!(offset <= SOURCE_SIZE_MAX);
    JsonError::Malformed { offset: offset as u32 }
}

fn digits_end(source: &[u8], start: usize) -> usize {
    let mut index = start;
    while matches!(source.get(index), Some(b'0'..=b'9')) {
        index += 1;
    }
    assert!(index >= start);
    index
}

/// A position in a parsed document.
#[derive(Clone, Copy, Debug)]
pub struct Value<'s, 't> {
    document: Document<'s, 't>,
    index: u32,
}

impl<'s, 't> Value<'s, 't> {
    fn token(self) -> &'t Token {
        &self.document.tokens[self.index as usize]
    }

    #[must_use]
    pub fn kind(self) -> Kind {
        self.token().kind
    }

    #[must_use]
    pub fn is_null(self) -> bool {
        self.kind() == Kind::Null
    }

    fn raw(self) -> &'s [u8] {
        let token = self.token();
        &self.document.source[token.start as usize..token.end as usize]
    }

    /// A string's text. `None` for other kinds, for invalid UTF-8, and for a string with escapes:
    /// nothing this crate matches on has them, so they are refused rather than half-decoded. Use
    /// `unescape` for free text such as notifications.
    #[must_use]
    pub fn str(self) -> Option<&'s str> {
        let token = self.token();
        if token.kind == Kind::String && !token.escaped {
            core::str::from_utf8(self.raw()).ok()
        } else {
            None
        }
    }

    /// A number's or a string's bytes, for the decimal parsers. The docs call some fields numbers
    /// that the server sends as strings, so both are accepted here.
    #[must_use]
    pub fn text(self) -> Option<&'s [u8]> {
        let token = self.token();
        match token.kind {
            Kind::Number => Some(self.raw()),
            Kind::String if !token.escaped => Some(self.raw()),
            _ => None,
        }
    }

    #[must_use]
    pub fn bool(self) -> Option<bool> {
        match self.kind() {
            Kind::True => Some(true),
            Kind::False => Some(false),
            _ => None,
        }
    }

    /// An unsigned integer. A fraction or an exponent is refused rather than truncated.
    #[must_use]
    pub fn u64(self) -> Option<u64> {
        if self.kind() != Kind::Number {
            return None;
        }
        let digits = self.raw();
        let mut value = 0u64;
        for &byte in digits {
            if !byte.is_ascii_digit() {
                return None;
            }
            value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
        }
        Some(value)
    }

    /// Looks up a key in an object: the first match, in linear time, over a shape that is small.
    #[must_use]
    pub fn get(self, key: &str) -> Option<Value<'s, 't>> {
        for (name, value) in self.entries() {
            if name.raw() == key.as_bytes() && !name.token().escaped {
                return Some(value);
            }
        }
        None
    }

    /// How many values a container holds; zero for a scalar.
    #[must_use]
    pub fn count(self) -> u32 {
        self.token().count
    }

    /// The values of an array, or nothing for any other kind.
    #[must_use]
    pub fn items(self) -> Items<'s, 't> {
        let token = self.token();
        let (remaining, next) =
            if token.kind == Kind::Array { (token.count, self.index + 1) } else { (0, self.index) };
        Items { document: self.document, next, remaining }
    }

    /// The key and value pairs of an object, or nothing for any other kind.
    #[must_use]
    pub fn entries(self) -> Entries<'s, 't> {
        let token = self.token();
        let (remaining, next) = if token.kind == Kind::Object {
            (token.count, self.index + 1)
        } else {
            (0, self.index)
        };
        Entries { document: self.document, next, remaining }
    }

    /// Prepares a walk of `items` in reverse: writes each item's tape index to `target` and returns
    /// how many fit. A tape has only forward links, so walking backward takes this one pass and a
    /// caller's buffer.
    #[must_use]
    pub fn item_indices(self, target: &mut [u32]) -> usize {
        let mut count = 0usize;
        for item in self.items() {
            if count == target.len() {
                break;
            }
            target[count] = item.index;
            count += 1;
        }
        assert!(count <= target.len());
        count
    }

    /// The value at a tape index that `item_indices` returned for this document.
    #[must_use]
    pub fn at_index(self, index: u32) -> Value<'s, 't> {
        assert!((index as usize) < self.document.tokens.len());
        Value { document: self.document, index }
    }

    /// Writes a string's decoded text into `target`, returning the bytes written, or `None` when
    /// it is not a string, does not fit, or holds a surrogate pair this reader does not join.
    #[must_use]
    pub fn unescape(self, target: &mut [u8]) -> Option<usize> {
        if self.kind() != Kind::String {
            return None;
        }
        let source = self.raw();
        let mut size = 0usize;
        let mut index = 0usize;
        while index < source.len() {
            let byte = source[index];
            if byte == b'\\' {
                let (decoded, consumed) = decode_escape(&source[index..])?;
                let mut buffer = [0u8; 4];
                let encoded = decoded.encode_utf8(&mut buffer).as_bytes();
                target.get_mut(size..size + encoded.len())?.copy_from_slice(encoded);
                size += encoded.len();
                index += consumed;
            } else {
                *target.get_mut(size)? = byte;
                size += 1;
                index += 1;
            }
        }
        assert!(size <= target.len());
        Some(size)
    }
}

fn decode_escape(source: &[u8]) -> Option<(char, usize)> {
    assert_eq!(source[0], b'\\');
    let simple = match source.get(1)? {
        b'"' => '"',
        b'\\' => '\\',
        b'/' => '/',
        b'b' => '\u{8}',
        b'f' => '\u{c}',
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        b'u' => {
            let digits = core::str::from_utf8(source.get(2..6)?).ok()?;
            let code = u32::from_str_radix(digits, 16).ok()?;
            return Some((char::from_u32(code)?, 6));
        }
        _ => return None,
    };
    Some((simple, 2))
}

/// An array's values in order.
#[derive(Clone, Debug)]
pub struct Items<'s, 't> {
    document: Document<'s, 't>,
    next: u32,
    remaining: u32,
}

impl<'s, 't> Iterator for Items<'s, 't> {
    type Item = Value<'s, 't>;

    fn next(&mut self) -> Option<Value<'s, 't>> {
        if self.remaining == 0 {
            return None;
        }
        let value = Value { document: self.document, index: self.next };
        self.next = value.token().next;
        self.remaining -= 1;
        Some(value)
    }
}

/// An object's entries in order.
#[derive(Clone, Debug)]
pub struct Entries<'s, 't> {
    document: Document<'s, 't>,
    next: u32,
    remaining: u32,
}

impl<'s, 't> Iterator for Entries<'s, 't> {
    type Item = (Value<'s, 't>, Value<'s, 't>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let key = Value { document: self.document, index: self.next };
        assert!(key.kind() == Kind::String, "the parser only accepts strings as keys");
        let value = Value { document: self.document, index: key.token().next };
        self.next = value.token().next;
        self.remaining -= 1;
        Some((key, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;

    fn with_doc<R>(source: &str, read: impl FnOnce(Value<'_, '_>) -> R) -> R {
        let mut tokens = vec![Token::ZERO; 256];
        let document = parse(source.as_bytes(), &mut tokens).unwrap();
        read(document.root())
    }

    fn error_of(source: &[u8]) -> JsonError {
        let mut tokens = vec![Token::ZERO; 256];
        parse(source, &mut tokens).unwrap_err()
    }

    #[test]
    fn reads_every_kind() {
        with_doc(
            r#" {"a": [1, -2.5, 1e3, true, false, null, "s", {}, []], "b": {"c": "d"}} "#,
            |root| {
                assert_eq!(root.kind(), Kind::Object);
                assert_eq!(root.count(), 2);
                let a = root.get("a").unwrap();
                assert_eq!(a.count(), 9);
                let items: std::vec::Vec<_> = a.items().collect();
                assert_eq!(items[0].u64(), Some(1));
                assert_eq!(items[1].u64(), None);
                assert_eq!(items[1].text(), Some(&b"-2.5"[..]));
                assert_eq!(items[2].u64(), None);
                assert_eq!(items[3].bool(), Some(true));
                assert_eq!(items[4].bool(), Some(false));
                assert!(items[5].is_null());
                assert_eq!(items[6].str(), Some("s"));
                assert_eq!(items[7].count(), 0);
                assert_eq!(items[8].items().count(), 0);
                assert_eq!(root.get("b").unwrap().get("c").unwrap().str(), Some("d"));
                assert!(root.get("missing").is_none());
                assert_eq!(a.get("x").map(Value::kind), None);
            },
        );
    }

    #[test]
    fn skipping_a_subtree_lands_on_its_sibling() {
        with_doc(r#"{"deep":{"x":[[1,2],[3,{"y":4}]]},"after":9,"last":[0]}"#, |root| {
            assert_eq!(root.get("after").unwrap().u64(), Some(9));
            assert_eq!(root.get("last").unwrap().items().next().unwrap().u64(), Some(0));
            let names: std::vec::Vec<_> = root.entries().map(|(k, _)| k.str().unwrap()).collect();
            assert_eq!(names, ["deep", "after", "last"]);
        });
    }

    #[test]
    fn refuses_what_is_not_json() {
        for source in [
            "",
            " ",
            "{",
            "[",
            "\"",
            "tru",
            "nul",
            "-",
            "1.",
            "1e",
            "1e+",
            ".5",
            "+1",
            "01",
            "-01",
            "[1,]",
            "{\"a\":1,}",
            "{\"a\"}",
            "{\"a\":}",
            "{1:2}",
            "[1 2]",
            "1 2",
            "{}{}",
            "[]]",
            "{]",
            "[}",
            "\"\\x\"",
            "\"\\u12\"",
            "\"\\u12g4\"",
            "\"\t\"",
            "nulll",
            "truex",
            "'a'",
            "{\"a\":1 \"b\":2}",
            "[,1]",
            "{,}",
            ",",
            ":",
        ] {
            let mut tokens = vec![Token::ZERO; 64];
            assert!(parse(source.as_bytes(), &mut tokens).is_err(), "{source:?}");
        }
    }

    #[test]
    fn errors_say_which_way_they_failed() {
        assert_eq!(error_of(b"{\"a\":1"), JsonError::Truncated);
        assert_eq!(error_of(b"[1,2,x]"), JsonError::Malformed { offset: 5 });
        assert_eq!(error_of(b"\"unterminated"), JsonError::Truncated);
        assert_eq!(error_of(b"1 x"), JsonError::Malformed { offset: 2 });
    }

    #[test]
    fn nesting_is_bounded_without_recursion() {
        let mut deep = vec![b'['; DEPTH_MAX + 1];
        deep.extend(vec![b']'; DEPTH_MAX + 1]);
        let mut tokens = vec![Token::ZERO; DEPTH_MAX + 2];
        assert_eq!(parse(&deep, &mut tokens).unwrap_err(), JsonError::TooDeep);

        let mut exact = vec![b'['; DEPTH_MAX];
        exact.extend(vec![b']'; DEPTH_MAX]);
        assert!(parse(&exact, &mut tokens).is_ok());

        let hostile = vec![b'['; 100_000];
        assert_eq!(parse(&hostile, &mut tokens).unwrap_err(), JsonError::TooDeep);
    }

    #[test]
    fn a_full_tape_is_an_error_at_exact_capacity_plus_one() {
        let mut tokens = vec![Token::ZERO; 3];
        assert!(parse(b"[1,2]", &mut tokens).is_ok());
        let mut tokens = vec![Token::ZERO; 2];
        assert_eq!(parse(b"[1,2]", &mut tokens).unwrap_err(), JsonError::TokensExhausted);
    }

    #[test]
    fn integer_reads_refuse_overflow_and_fractions() {
        with_doc("[18446744073709551615,18446744073709551616,1.0,-1,0]", |root| {
            let items: std::vec::Vec<_> = root.items().collect();
            assert_eq!(items[0].u64(), Some(u64::MAX));
            assert_eq!(items[1].u64(), None);
            assert_eq!(items[2].u64(), None);
            assert_eq!(items[3].u64(), None);
            assert_eq!(items[4].u64(), Some(0));
        });
    }

    #[test]
    fn escapes_are_refused_by_str_and_decoded_by_unescape() {
        with_doc(r#"["a\nb\u0041\"\\","plain","\ud83d"]"#, |root| {
            let items: std::vec::Vec<_> = root.items().collect();
            assert_eq!(items[0].str(), None);
            assert_eq!(items[0].text(), None);
            let mut buffer = [0u8; 16];
            let size = items[0].unescape(&mut buffer).unwrap();
            assert_eq!(&buffer[..size], b"a\nbA\"\\");
            let mut small = [0u8; 2];
            assert_eq!(items[0].unescape(&mut small), None);
            assert_eq!(items[1].str(), Some("plain"));
            // A lone surrogate is not a character; refusing it is the honest answer.
            assert_eq!(items[2].unescape(&mut buffer), None);
        });
    }

    #[test]
    fn invalid_utf8_is_refused_on_read_not_on_parse() {
        let source = b"[\"\xff\xfe\",\"ok\"]";
        let mut tokens = vec![Token::ZERO; 8];
        let document = parse(source, &mut tokens).unwrap();
        let items: std::vec::Vec<_> = document.root().items().collect();
        assert_eq!(items[0].str(), None);
        assert_eq!(items[1].str(), Some("ok"));
    }

    #[test]
    fn a_key_with_an_escape_never_matches_a_literal() {
        with_doc(r#"{"\u0061":1,"a":2}"#, |root| {
            assert_eq!(root.get("a").unwrap().u64(), Some(2));
        });
    }

    #[test]
    fn item_indices_allow_a_walk_in_reverse() {
        with_doc(r#"[1,[2,3],{"a":4},5]"#, |root| {
            let mut indices = [0u32; 8];
            let count = root.item_indices(&mut indices);
            assert_eq!(count, 4);
            assert_eq!(root.at_index(indices[3]).u64(), Some(5));
            assert_eq!(root.at_index(indices[2]).get("a").unwrap().u64(), Some(4));
            assert_eq!(root.at_index(indices[1]).count(), 2);
            assert_eq!(root.at_index(indices[0]).u64(), Some(1));
            let mut small = [0u32; 2];
            assert_eq!(root.item_indices(&mut small), 2);
        });
    }

    #[test]
    fn scalar_roots_are_documents() {
        with_doc("7", |root| assert_eq!(root.u64(), Some(7)));
        with_doc("\"x\"", |root| assert_eq!(root.str(), Some("x")));
        with_doc("null", |root| assert!(root.is_null()));
    }

    /// Every prefix of a document is rejected: a torn frame can never parse as a shorter message.
    #[test]
    fn every_strict_prefix_is_refused() {
        let source = br#"{"channel":"bbo","data":{"px":"1.5","n":[1,2,null,true]}}"#;
        let mut tokens = vec![Token::ZERO; 64];
        assert!(parse(source, &mut tokens).is_ok());
        for size in 0..source.len() {
            assert!(parse(&source[..size], &mut tokens).is_err(), "prefix of {size}");
        }
    }
}
