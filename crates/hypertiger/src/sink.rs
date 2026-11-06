//! One walk, two encodings.
//!
//! The exchange recovers the signer from a hash of the action's msgpack, and it reads the action
//! from JSON. A client that builds those two separately can sign one thing and send another, and
//! the failure mode is an opaque "user does not exist". Here an action is a single `walk` over a
//! `Sink`: the hash and the body are the same traversal, so they cannot disagree.

use sha3::{Digest, Keccak256};

/// The deepest structure any action needs: envelope, action, order, type, trigger, and room.
pub const NESTING_MAX: usize = 8;

/// Something an action can be walked into. Maps and arrays declare their length up front, as
/// msgpack requires; both sinks check that the walk then keeps that promise.
pub trait Sink {
    fn map(&mut self, entries: u32);
    fn array(&mut self, items: u32);
    fn key(&mut self, key: &str);
    fn str(&mut self, value: &str);
    fn u64(&mut self, value: u64);
    fn i64(&mut self, value: i64);
    fn bool(&mut self, value: bool);
    fn nil(&mut self);
}

/// A value that knows how to walk itself. Implement this to send an action the crate has no type
/// for, such as a deployer action; it signs through the same path as every other.
pub trait Walk {
    fn walk<S: Sink>(&self, sink: &mut S);
}

#[derive(Clone, Copy)]
struct Frame {
    /// Values still owed: a map entry counts as two, the key and its value.
    values_remaining: u32,
    is_map: bool,
    /// Whether this frame has emitted a value yet, for JSON's commas.
    started: bool,
}

const FRAME_EMPTY: Frame = Frame { values_remaining: 0, is_map: false, started: false };

/// The promise-keeping shared by both sinks. A walk that declares three entries and writes two is
/// a bug in this crate, and in msgpack it would produce a valid-looking wrong hash, so it panics.
struct Frames {
    stack: [Frame; NESTING_MAX],
    depth: u32,
    /// Whether the single top-level value has been opened or written.
    opened: bool,
}

impl Frames {
    const fn new() -> Frames {
        Frames { stack: [FRAME_EMPTY; NESTING_MAX], depth: 0, opened: false }
    }

    fn top(&mut self) -> &mut Frame {
        assert!(self.depth > 0);
        &mut self.stack[self.depth as usize - 1]
    }

    /// Whether the next value in the current frame is a map key.
    fn expects_key(&mut self) -> bool {
        if self.depth == 0 {
            return false;
        }
        let top = self.top();
        top.is_map && top.values_remaining % 2 == 0
    }

    fn before_value(&mut self) {
        assert!(!self.expects_key(), "a map value was written where a key belongs");
        if self.depth == 0 {
            assert!(!self.opened, "a second top-level value");
            self.opened = true;
        }
    }

    fn before_key(&mut self) {
        assert!(self.expects_key(), "a key was written where a value belongs");
    }

    /// Records one value, returning how many frames that value completed.
    fn after_value(&mut self) -> u32 {
        let mut closed = 0u32;
        let mut depth = self.depth;
        // Bounded by the stack depth: each pass pops a frame or stops.
        for _ in 0..=NESTING_MAX {
            if depth == 0 {
                break;
            }
            let frame = &mut self.stack[depth as usize - 1];
            assert!(frame.values_remaining > 0, "more values written than declared");
            frame.values_remaining -= 1;
            frame.started = true;
            if frame.values_remaining > 0 {
                break;
            }
            depth -= 1;
            closed += 1;
        }
        self.depth = depth;
        closed
    }

    fn push(&mut self, values: u32, is_map: bool) {
        assert!((self.depth as usize) < NESTING_MAX, "nesting past NESTING_MAX");
        self.stack[self.depth as usize] =
            Frame { values_remaining: values, is_map, started: false };
        self.depth += 1;
    }

    fn finish(&self) {
        assert!(self.opened, "nothing was walked");
        assert!(self.depth == 0, "a walk ended with values still owed");
    }
}

/// Where msgpack bytes go.
pub trait ByteTarget {
    fn write(&mut self, bytes: &[u8]);
}

/// Streams into Keccak, so hashing an action needs no buffer and has no size limit to get wrong.
pub struct KeccakTarget(Keccak256);

impl core::fmt::Debug for KeccakTarget {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("KeccakTarget")
    }
}

impl KeccakTarget {
    #[must_use]
    pub fn new() -> KeccakTarget {
        KeccakTarget(Keccak256::new())
    }

    #[must_use]
    pub fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

impl Default for KeccakTarget {
    fn default() -> KeccakTarget {
        KeccakTarget::new()
    }
}

impl ByteTarget for KeccakTarget {
    fn write(&mut self, bytes: &[u8]) {
        Digest::update(&mut self.0, bytes);
    }
}

/// Encodes msgpack exactly as Python's `msgpack.packb` does: the smallest form of each value, and
/// `str` rather than `bin` for text. Field order comes from the walk.
#[derive(Debug)]
pub struct MsgpackSink<T: ByteTarget> {
    target: T,
    frames_state: FramesState,
}

// Keeps `Frames` private while letting `MsgpackSink` derive `Debug`.
struct FramesState(Frames);

impl core::fmt::Debug for FramesState {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "FramesState(depth {})", self.0.depth)
    }
}

impl<T: ByteTarget> MsgpackSink<T> {
    pub fn new(target: T) -> MsgpackSink<T> {
        MsgpackSink { target, frames_state: FramesState(Frames::new()) }
    }

    /// Checks that the walk kept its promises and hands the target back.
    pub fn finish(self) -> T {
        self.frames_state.0.finish();
        self.target
    }

    /// Writes bytes that were not walked, for the fields the L1 hash appends after the action.
    pub fn target_mut(&mut self) -> &mut T {
        &mut self.target
    }

    fn head_u16(&mut self, tag: u8, value: u16) {
        self.target.write(&[tag]);
        self.target.write(&value.to_be_bytes());
    }

    fn head_u32(&mut self, tag: u8, value: u32) {
        self.target.write(&[tag]);
        self.target.write(&value.to_be_bytes());
    }

    fn value_done(&mut self) {
        let _ = self.frames_state.0.after_value();
    }
}

impl<T: ByteTarget> Sink for MsgpackSink<T> {
    fn map(&mut self, entries: u32) {
        self.frames_state.0.before_value();
        match entries {
            0..=15 => self.target.write(&[0x80 | entries as u8]),
            16..=0xffff => self.head_u16(0xde, entries as u16),
            _ => self.head_u32(0xdf, entries),
        }
        if entries == 0 {
            self.value_done();
        } else {
            self.frames_state.0.push(entries * 2, true);
        }
    }

    fn array(&mut self, items: u32) {
        self.frames_state.0.before_value();
        match items {
            0..=15 => self.target.write(&[0x90 | items as u8]),
            16..=0xffff => self.head_u16(0xdc, items as u16),
            _ => self.head_u32(0xdd, items),
        }
        if items == 0 {
            self.value_done();
        } else {
            self.frames_state.0.push(items, false);
        }
    }

    fn key(&mut self, key: &str) {
        self.frames_state.0.before_key();
        msgpack_str(&mut self.target, key);
        self.value_done();
    }

    fn str(&mut self, value: &str) {
        self.frames_state.0.before_value();
        msgpack_str(&mut self.target, value);
        self.value_done();
    }

    fn u64(&mut self, value: u64) {
        self.frames_state.0.before_value();
        if value <= 0x7f {
            self.target.write(&[value as u8]);
        } else if value <= 0xff {
            self.target.write(&[0xcc, value as u8]);
        } else if value <= 0xffff {
            self.head_u16(0xcd, value as u16);
        } else if value <= 0xffff_ffff {
            self.head_u32(0xce, value as u32);
        } else {
            self.target.write(&[0xcf]);
            self.target.write(&value.to_be_bytes());
        }
        self.value_done();
    }

    fn i64(&mut self, value: i64) {
        if value >= 0 {
            self.u64(value.unsigned_abs());
            return;
        }
        self.frames_state.0.before_value();
        if value >= -32 {
            self.target.write(&[value as i8 as u8]);
        } else if value >= i64::from(i8::MIN) {
            self.target.write(&[0xd0, value as i8 as u8]);
        } else if value >= i64::from(i16::MIN) {
            self.target.write(&[0xd1]);
            self.target.write(&(value as i16).to_be_bytes());
        } else if value >= i64::from(i32::MIN) {
            self.target.write(&[0xd2]);
            self.target.write(&(value as i32).to_be_bytes());
        } else {
            self.target.write(&[0xd3]);
            self.target.write(&value.to_be_bytes());
        }
        self.value_done();
    }

    fn bool(&mut self, value: bool) {
        self.frames_state.0.before_value();
        self.target.write(&[if value { 0xc3 } else { 0xc2 }]);
        self.value_done();
    }

    fn nil(&mut self) {
        self.frames_state.0.before_value();
        self.target.write(&[0xc0]);
        self.value_done();
    }
}

fn msgpack_str<T: ByteTarget>(target: &mut T, text: &str) {
    let size = text.len();
    assert!(u32::try_from(size).is_ok());
    match size {
        0..=31 => target.write(&[0xa0 | size as u8]),
        32..=0xff => target.write(&[0xd9, size as u8]),
        0x100..=0xffff => {
            target.write(&[0xda]);
            target.write(&(size as u16).to_be_bytes());
        }
        _ => {
            target.write(&[0xdb]);
            target.write(&(size as u32).to_be_bytes());
        }
    }
    target.write(text.as_bytes());
}

/// The caller's buffer was too small.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferFull;

/// Writes JSON into a caller's buffer. Overflow is sticky and reported once by `finish`, which
/// keeps every walk free of error plumbing; nothing truncated can be mistaken for a whole body,
/// because `finish` is the only way to learn the length.
#[derive(Debug)]
pub struct JsonSink<'b> {
    target: &'b mut [u8],
    size: usize,
    overflowed: bool,
    frames_state: FramesState,
}

impl<'b> JsonSink<'b> {
    pub fn new(target: &'b mut [u8]) -> JsonSink<'b> {
        JsonSink { target, size: 0, overflowed: false, frames_state: FramesState(Frames::new()) }
    }

    /// Checks the walk and returns the bytes written.
    pub fn finish(self) -> Result<usize, BufferFull> {
        self.frames_state.0.finish();
        if self.overflowed {
            return Err(BufferFull);
        }
        assert!(self.size <= self.target.len());
        Ok(self.size)
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.overflowed {
            return;
        }
        match self.target.get_mut(self.size..self.size + bytes.len()) {
            Some(window) => {
                window.copy_from_slice(bytes);
                self.size += bytes.len();
            }
            None => self.overflowed = true,
        }
    }

    fn comma(&mut self) {
        let frames = &mut self.frames_state.0;
        if frames.depth == 0 {
            return;
        }
        let top = frames.top();
        let separate = top.started && (!top.is_map || top.values_remaining % 2 == 0);
        if separate {
            self.write(b",");
        }
    }

    fn value_done(&mut self) {
        // A frame's closing bracket is owed only for those this value completed, innermost first.
        let depth_before = self.frames_state.0.depth;
        let closed = self.frames_state.0.after_value();
        assert!(closed <= depth_before);
        let mut index = depth_before;
        for _ in 0..closed {
            index -= 1;
            let bracket =
                if self.frames_state.0.stack[index as usize].is_map { b"}" } else { b"]" };
            self.write(bracket);
        }
    }

    fn quoted(&mut self, text: &str) {
        self.write(b"\"");
        let bytes = text.as_bytes();
        let mut start = 0usize;
        let mut index = 0usize;
        while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'"' || byte == b'\\' || byte < 0x20 {
                self.write(&bytes[start..index]);
                self.escape(byte);
                start = index + 1;
            }
            index += 1;
        }
        self.write(&bytes[start..]);
        self.write(b"\"");
    }

    fn escape(&mut self, byte: u8) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        match byte {
            b'"' => self.write(b"\\\""),
            b'\\' => self.write(b"\\\\"),
            _ => {
                assert!(byte < 0x20);
                self.write(b"\\u00");
                self.write(&[HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0x0f)]]);
            }
        }
    }

    fn decimal(&mut self, value: u64) {
        let mut digits = [0u8; 20];
        let mut position = digits.len();
        let mut remaining = value;
        // A u64 has at most 20 digits, and the zero case needs the one pass this always makes.
        for _ in 0..digits.len() {
            position -= 1;
            digits[position] = b'0' + (remaining % 10) as u8;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }
        assert_eq!(remaining, 0);
        self.write(&digits[position..]);
    }
}

impl Sink for JsonSink<'_> {
    fn map(&mut self, entries: u32) {
        self.frames_state.0.before_value();
        self.comma();
        self.write(b"{");
        if entries == 0 {
            self.write(b"}");
            self.value_done();
        } else {
            self.frames_state.0.push(entries * 2, true);
        }
    }

    fn array(&mut self, items: u32) {
        self.frames_state.0.before_value();
        self.comma();
        self.write(b"[");
        if items == 0 {
            self.write(b"]");
            self.value_done();
        } else {
            self.frames_state.0.push(items, false);
        }
    }

    fn key(&mut self, key: &str) {
        self.frames_state.0.before_key();
        self.comma();
        self.quoted(key);
        self.write(b":");
        self.value_done();
    }

    fn str(&mut self, value: &str) {
        self.frames_state.0.before_value();
        self.comma();
        self.quoted(value);
        self.value_done();
    }

    fn u64(&mut self, value: u64) {
        self.frames_state.0.before_value();
        self.comma();
        self.decimal(value);
        self.value_done();
    }

    fn i64(&mut self, value: i64) {
        self.frames_state.0.before_value();
        self.comma();
        if value < 0 {
            self.write(b"-");
        }
        self.decimal(value.unsigned_abs());
        self.value_done();
    }

    fn bool(&mut self, value: bool) {
        self.frames_state.0.before_value();
        self.comma();
        self.write(if value { b"true" } else { b"false" });
        self.value_done();
    }

    fn nil(&mut self) {
        self.frames_state.0.before_value();
        self.comma();
        self.write(b"null");
        self.value_done();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bytes {
        buffer: [u8; 512],
        size: usize,
    }

    impl ByteTarget for Bytes {
        fn write(&mut self, bytes: &[u8]) {
            self.buffer[self.size..self.size + bytes.len()].copy_from_slice(bytes);
            self.size += bytes.len();
        }
    }

    fn msgpack(walk: impl FnOnce(&mut MsgpackSink<Bytes>)) -> std::vec::Vec<u8> {
        let mut sink = MsgpackSink::new(Bytes { buffer: [0; 512], size: 0 });
        walk(&mut sink);
        let target = sink.finish();
        target.buffer[..target.size].to_vec()
    }

    fn json(walk: impl FnOnce(&mut JsonSink<'_>)) -> std::string::String {
        let mut buffer = [0u8; 512];
        let mut sink = JsonSink::new(&mut buffer);
        walk(&mut sink);
        let size = sink.finish().unwrap();
        std::string::String::from_utf8(buffer[..size].to_vec()).unwrap()
    }

    #[test]
    fn msgpack_uses_the_smallest_integer_form_at_every_boundary() {
        for (value, expected) in [
            (0u64, &[0x00u8][..]),
            (0x7f, &[0x7f]),
            (0x80, &[0xcc, 0x80]),
            (0xff, &[0xcc, 0xff]),
            (0x100, &[0xcd, 0x01, 0x00]),
            (0xffff, &[0xcd, 0xff, 0xff]),
            (0x1_0000, &[0xce, 0x00, 0x01, 0x00, 0x00]),
            (0xffff_ffff, &[0xce, 0xff, 0xff, 0xff, 0xff]),
            (0x1_0000_0000, &[0xcf, 0, 0, 0, 1, 0, 0, 0, 0]),
            (u64::MAX, &[0xcf, 255, 255, 255, 255, 255, 255, 255, 255]),
        ] {
            assert_eq!(msgpack(|sink| sink.u64(value)), expected, "{value}");
        }
    }

    #[test]
    fn msgpack_signed_boundaries() {
        for (value, expected) in [
            (5i64, &[0x05u8][..]),
            (-1, &[0xff]),
            (-32, &[0xe0]),
            (-33, &[0xd0, 0xdf]),
            (-128, &[0xd0, 0x80]),
            (-129, &[0xd1, 0xff, 0x7f]),
            (-32768, &[0xd1, 0x80, 0x00]),
            (-32769, &[0xd2, 0xff, 0xff, 0x7f, 0xff]),
            (i64::from(i32::MIN) - 1, &[0xd3, 0xff, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff]),
        ] {
            assert_eq!(msgpack(|sink| sink.i64(value)), expected, "{value}");
        }
    }

    #[test]
    fn msgpack_string_and_container_headers() {
        let text31 = "a".repeat(31);
        let text32 = "a".repeat(32);
        let header = |text: &str| msgpack(|sink| sink.str(text))[..2].to_vec();
        assert_eq!(header(&text31), [0xbf, b'a']);
        assert_eq!(header(&text32), [0xd9, 32]);
        assert_eq!(msgpack(|sink| sink.map(0)), [0x80]);
        assert_eq!(msgpack(|sink| sink.array(0)), [0x90]);
        let items = msgpack(|sink| {
            sink.array(16);
            for _ in 0..16 {
                sink.nil();
            }
        });
        assert_eq!(items[..3], [0xdc, 0x00, 0x10]);
    }

    #[test]
    fn json_writes_commas_and_closes_nested_structures() {
        let text = json(|sink| {
            sink.map(3);
            sink.key("a");
            sink.array(2);
            sink.u64(1);
            sink.map(1);
            sink.key("b");
            sink.bool(true);
            sink.key("c");
            sink.i64(-7);
            sink.key("d");
            sink.array(0);
        });
        assert_eq!(text, r#"{"a":[1,{"b":true}],"c":-7,"d":[]}"#);
    }

    #[test]
    fn json_escapes_what_must_be_escaped_and_nothing_else() {
        let text = json(|sink| sink.str("a\"b\\c\nd\u{1}é"));
        assert_eq!(text, "\"a\\\"b\\\\c\\u000ad\\u0001é\"");
        assert_eq!(json(|sink| sink.u64(0)), "0");
        assert_eq!(json(|sink| sink.u64(u64::MAX)), "18446744073709551615");
        assert_eq!(json(|sink| sink.i64(i64::MIN)), "-9223372036854775808");
        assert_eq!(json(|sink| sink.nil()), "null");
    }

    #[test]
    fn json_overflow_is_reported_and_never_a_short_body() {
        let mut buffer = [0u8; 4];
        let mut sink = JsonSink::new(&mut buffer);
        sink.str("longer than four");
        assert_eq!(sink.finish(), Err(BufferFull));

        let mut exact = [0u8; 3];
        let mut sink = JsonSink::new(&mut exact);
        sink.str("a");
        assert_eq!(sink.finish(), Ok(3));
    }

    #[test]
    #[should_panic(expected = "values still owed")]
    fn a_walk_that_breaks_its_promise_is_a_bug() {
        msgpack(|sink| {
            sink.map(2);
            sink.key("only one");
            sink.u64(1);
        });
    }

    #[test]
    #[should_panic(expected = "where a key belongs")]
    fn a_value_in_a_key_position_is_a_bug() {
        msgpack(|sink| {
            sink.map(1);
            sink.u64(1);
        });
    }

    #[test]
    #[should_panic(expected = "second top-level")]
    fn two_top_level_values_are_a_bug() {
        json(|sink| {
            sink.u64(1);
            sink.u64(2);
        });
    }

    #[test]
    #[should_panic(expected = "nesting past")]
    fn nesting_is_bounded() {
        msgpack(|sink| {
            for _ in 0..=NESTING_MAX {
                sink.array(1);
            }
        });
    }
}
