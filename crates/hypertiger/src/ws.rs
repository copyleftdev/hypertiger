//! WebSocket (RFC 6455), sans-IO: the handshake and frames, over buffers the caller owns.
//!
//! SHA-1 and base64 are written out here because the handshake needs nothing else from them. This
//! is not the crate's cryptography rule being bent: `Sec-WebSocket-Accept` protects against talking
//! to the wrong kind of server, not against an attacker, and TLS carries the security. The RFC's
//! own test vectors pin both.

const GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A control frame's payload limit, from the RFC.
pub const CONTROL_PAYLOAD_MAX: usize = 125;
/// A client frame's overhead: two header bytes, eight of length, four of mask.
pub const CLIENT_HEADER_SIZE_MAX: usize = 14;
/// `Sec-WebSocket-Key` is 16 bytes, base64 encoded.
pub const KEY_ENCODED_SIZE: usize = 24;
/// The accept value is a 20-byte digest, base64 encoded.
pub const ACCEPT_ENCODED_SIZE: usize = 28;

const _: () = assert!(16usize.div_ceil(3) * 4 == KEY_ENCODED_SIZE);
const _: () = assert!(20usize.div_ceil(3) * 4 == ACCEPT_ENCODED_SIZE);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsError {
    /// Bytes that break the protocol. The connection is over.
    Protocol,
    /// A frame or message past the caller's buffer.
    TooLarge,
    /// The server did not agree to upgrade.
    HandshakeRefused,
    BufferTooSmall,
}

/// SHA-1 of a message short enough to fit a fixed buffer. Only the handshake uses it.
fn sha1(message: &[u8]) -> [u8; 20] {
    const BLOCK_SIZE: usize = 64;
    assert!(message.len() < 2 * BLOCK_SIZE - 9, "the accept input is always 60 bytes");
    let mut padded = [0u8; 2 * BLOCK_SIZE];
    padded[..message.len()].copy_from_slice(message);
    padded[message.len()] = 0x80;
    let blocks = if message.len() + 9 <= BLOCK_SIZE { 1 } else { 2 };
    let bits = (message.len() as u64) * 8;
    padded[blocks * BLOCK_SIZE - 8..blocks * BLOCK_SIZE].copy_from_slice(&bits.to_be_bytes());

    let mut state: [u32; 5] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476, 0xc3d2_e1f0];
    for block in padded[..blocks * BLOCK_SIZE].chunks_exact(BLOCK_SIZE) {
        sha1_block(&mut state, block);
    }
    let mut digest = [0u8; 20];
    for (target, word) in digest.chunks_exact_mut(4).zip(state) {
        target.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

// The letters are FIPS 180's own names for the working variables.
#[allow(clippy::many_single_char_names)]
fn sha1_block(state: &mut [u32; 5], block: &[u8]) {
    assert_eq!(block.len(), 64);
    let mut schedule = [0u32; 80];
    for (word, bytes) in schedule.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    }
    for index in 16..80 {
        schedule[index] = (schedule[index - 3]
            ^ schedule[index - 8]
            ^ schedule[index - 14]
            ^ schedule[index - 16])
            .rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
    for (index, word) in schedule.iter().enumerate() {
        let (mixed, constant) = match index {
            0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
            20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
            _ => (b ^ c ^ d, 0xca62_c1d6),
        };
        let next = a
            .rotate_left(5)
            .wrapping_add(mixed)
            .wrapping_add(e)
            .wrapping_add(constant)
            .wrapping_add(*word);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = next;
    }
    for (slot, value) in state.iter_mut().zip([a, b, c, d, e]) {
        *slot = slot.wrapping_add(value);
    }
}

fn base64_encode(source: &[u8], target: &mut [u8]) -> usize {
    let size = source.len().div_ceil(3) * 4;
    assert!(target.len() >= size);
    for (index, group) in source.chunks(3).enumerate() {
        let first = group[0];
        let second = group.get(1).copied().unwrap_or(0);
        let third = group.get(2).copied().unwrap_or(0);
        let out = &mut target[index * 4..index * 4 + 4];
        out[0] = BASE64[usize::from(first >> 2)];
        out[1] = BASE64[usize::from(((first & 0x03) << 4) | (second >> 4))];
        out[2] = if group.len() > 1 {
            BASE64[usize::from(((second & 0x0f) << 2) | (third >> 6))]
        } else {
            b'='
        };
        out[3] = if group.len() > 2 { BASE64[usize::from(third & 0x3f)] } else { b'=' };
    }
    size
}

/// Encodes a handshake key.
#[must_use]
pub fn encode_key(key: &[u8; 16]) -> [u8; KEY_ENCODED_SIZE] {
    let mut text = [0u8; KEY_ENCODED_SIZE];
    let size = base64_encode(key, &mut text);
    assert_eq!(size, KEY_ENCODED_SIZE);
    text
}

/// What the server must answer to this key.
#[must_use]
pub fn accept_value(key_encoded: &[u8; KEY_ENCODED_SIZE]) -> [u8; ACCEPT_ENCODED_SIZE] {
    let mut input = [0u8; KEY_ENCODED_SIZE + GUID.len()];
    input[..KEY_ENCODED_SIZE].copy_from_slice(key_encoded);
    input[KEY_ENCODED_SIZE..].copy_from_slice(GUID);
    let digest = sha1(&input);
    let mut text = [0u8; ACCEPT_ENCODED_SIZE];
    let size = base64_encode(&digest, &mut text);
    assert_eq!(size, ACCEPT_ENCODED_SIZE);
    text
}

/// Writes the upgrade request. No extensions are offered, so frames are never compressed.
pub fn write_handshake(
    target: &mut [u8],
    host: &str,
    path: &str,
    key_encoded: &[u8; KEY_ENCODED_SIZE],
) -> Result<usize, WsError> {
    assert!(path.starts_with('/'));
    let mut size = 0usize;
    for part in [
        b"GET ".as_slice(),
        path.as_bytes(),
        b" HTTP/1.1\r\nHost: ",
        host.as_bytes(),
        b"\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n",
        b"Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: ",
        key_encoded,
        b"\r\nUser-Agent: hypertiger\r\n\r\n",
    ] {
        let window = target.get_mut(size..size + part.len()).ok_or(WsError::BufferTooSmall)?;
        window.copy_from_slice(part);
        size += part.len();
    }
    Ok(size)
}

/// Reads the upgrade response. `Ok(None)` means more bytes are needed, and `Ok(Some(n))` means the
/// head took `n` bytes and any after it are already frames.
pub fn read_handshake(
    buffer: &[u8],
    accept_expected: &[u8; ACCEPT_ENCODED_SIZE],
) -> Result<Option<usize>, WsError> {
    let mut end = None;
    let mut index = 0usize;
    while index + 4 <= buffer.len() {
        if &buffer[index..index + 4] == b"\r\n\r\n" {
            end = Some(index + 4);
            break;
        }
        index += 1;
    }
    let Some(end) = end else {
        return if buffer.len() > crate::http::HEAD_SIZE_MAX {
            Err(WsError::TooLarge)
        } else {
            Ok(None)
        };
    };
    if !buffer.starts_with(b"HTTP/1.1 101") {
        return Err(WsError::HandshakeRefused);
    }
    let mut accepted = false;
    for line in buffer[..end].split(|&byte| byte == b'\n').skip(1) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(colon) = line.iter().position(|&byte| byte == b':') {
            let name = &line[..colon];
            let value = line[colon + 1..].trim_ascii();
            if name.eq_ignore_ascii_case(b"sec-websocket-accept") {
                accepted = value == accept_expected;
            } else if name.eq_ignore_ascii_case(b"sec-websocket-extensions") && !value.is_empty() {
                // None was offered, so a compressed frame would be unreadable.
                return Err(WsError::HandshakeRefused);
            }
        }
    }
    if accepted { Ok(Some(end)) } else { Err(WsError::HandshakeRefused) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl Opcode {
    const fn parse(bits: u8) -> Option<Opcode> {
        match bits {
            0x0 => Some(Opcode::Continuation),
            0x1 => Some(Opcode::Text),
            0x2 => Some(Opcode::Binary),
            0x8 => Some(Opcode::Close),
            0x9 => Some(Opcode::Ping),
            0xa => Some(Opcode::Pong),
            _ => None,
        }
    }

    const fn is_control(self) -> bool {
        matches!(self, Opcode::Close | Opcode::Ping | Opcode::Pong)
    }

    const fn bits(self) -> u8 {
        match self {
            Opcode::Continuation => 0x0,
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            Opcode::Close => 0x8,
            Opcode::Ping => 0x9,
            Opcode::Pong => 0xa,
        }
    }
}

/// One frame from the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: Opcode,
    pub payload_start: usize,
    pub payload_size: usize,
    /// Bytes this frame used.
    pub consumed: usize,
}

/// Reads one frame from the start of `buffer`. `Ok(None)` means more bytes are needed.
pub fn read_frame(buffer: &[u8], payload_size_max: usize) -> Result<Option<Frame>, WsError> {
    if buffer.len() < 2 {
        return Ok(None);
    }
    let first = buffer[0];
    let second = buffer[1];
    let fin = first & 0x80 != 0;
    // Reserved bits mean an extension, and none was negotiated.
    if first & 0x70 != 0 {
        return Err(WsError::Protocol);
    }
    let opcode = Opcode::parse(first & 0x0f).ok_or(WsError::Protocol)?;
    // A server must not mask, says the RFC, and a client must fail the connection if it does.
    if second & 0x80 != 0 {
        return Err(WsError::Protocol);
    }
    let Some((size, header_size)) = read_length(buffer, second & 0x7f)? else {
        return Ok(None);
    };
    if opcode.is_control() && (!fin || size > CONTROL_PAYLOAD_MAX as u64) {
        return Err(WsError::Protocol);
    }
    if size > payload_size_max as u64 {
        return Err(WsError::TooLarge);
    }
    let payload_size = size as usize;
    let consumed = header_size + payload_size;
    if buffer.len() < consumed {
        return Ok(None);
    }
    Ok(Some(Frame { fin, opcode, payload_start: header_size, payload_size, consumed }))
}

fn read_length(buffer: &[u8], short: u8) -> Result<Option<(u64, usize)>, WsError> {
    match short {
        0..=125 => Ok(Some((u64::from(short), 2))),
        126 => {
            let Some(bytes) = buffer.get(2..4) else {
                return Ok(None);
            };
            let size = u64::from(u16::from_be_bytes([bytes[0], bytes[1]]));
            // The RFC requires the shortest form, and anything else is a broken peer.
            if size < 126 { Err(WsError::Protocol) } else { Ok(Some((size, 4))) }
        }
        _ => {
            let Some(bytes) = buffer.get(2..10) else {
                return Ok(None);
            };
            let mut wide = [0u8; 8];
            wide.copy_from_slice(bytes);
            let size = u64::from_be_bytes(wide);
            if size <= 0xffff || size >> 63 != 0 {
                return Err(WsError::Protocol);
            }
            Ok(Some((size, 10)))
        }
    }
}

/// Writes a client frame's header and masks the payload in place.
///
/// The payload must already sit at `target[CLIENT_HEADER_SIZE_MAX..][..payload_size]`, which lets
/// the engine serialize straight into the buffer with no copy. The header is written just before
/// it, and the frame is the returned range.
pub fn seal_frame(
    target: &mut [u8],
    payload_size: usize,
    opcode: Opcode,
    mask: [u8; 4],
) -> core::ops::Range<usize> {
    assert!(target.len() >= CLIENT_HEADER_SIZE_MAX + payload_size);
    assert!(!opcode.is_control() || payload_size <= CONTROL_PAYLOAD_MAX);
    let mut header = [0u8; CLIENT_HEADER_SIZE_MAX];
    header[0] = 0x80 | opcode.bits();
    let length_size = if payload_size <= 125 {
        header[1] = 0x80 | payload_size as u8;
        0
    } else if payload_size <= 0xffff {
        header[1] = 0x80 | 0x7e;
        header[2..4].copy_from_slice(&(payload_size as u16).to_be_bytes());
        2
    } else {
        header[1] = 0x80 | 0x7f;
        header[2..10].copy_from_slice(&(payload_size as u64).to_be_bytes());
        8
    };
    header[2 + length_size..6 + length_size].copy_from_slice(&mask);
    let header_size = 6 + length_size;
    assert!(header_size <= CLIENT_HEADER_SIZE_MAX);

    let start = CLIENT_HEADER_SIZE_MAX - header_size;
    target[start..CLIENT_HEADER_SIZE_MAX].copy_from_slice(&header[..header_size]);
    let payload = &mut target[CLIENT_HEADER_SIZE_MAX..CLIENT_HEADER_SIZE_MAX + payload_size];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    start..CLIENT_HEADER_SIZE_MAX + payload_size
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;
    use std::vec;
    use std::vec::Vec;

    /// Reads a client frame the way a server would, to check `seal_frame` against the RFC.
    fn unseal(frame: &[u8]) -> (u8, Vec<u8>) {
        assert!(frame[1] & 0x80 != 0, "client frames are always masked");
        let short = frame[1] & 0x7f;
        let (size, at) = match short {
            126 => (usize::from(u16::from_be_bytes([frame[2], frame[3]])), 4),
            127 => {
                let mut wide = [0u8; 8];
                wide.copy_from_slice(&frame[2..10]);
                (u64::from_be_bytes(wide) as usize, 10)
            }
            _ => (usize::from(short), 2),
        };
        let mask = &frame[at..at + 4];
        let payload =
            frame[at + 4..at + 4 + size].iter().enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
        (frame[0], payload)
    }

    #[test]
    fn sha1_matches_the_published_vectors() {
        let hex = |digest: [u8; 20]| -> std::string::String {
            let mut text = std::string::String::new();
            for byte in digest {
                write!(text, "{byte:02x}").unwrap();
            }
            text
        };
        assert_eq!(hex(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    /// RFC 6455 section 1.3 gives this exact key and answer.
    #[test]
    fn accept_matches_the_rfc_example() {
        let key: &[u8; KEY_ENCODED_SIZE] = b"dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(&accept_value(key), b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(&encode_key(b"the sample nonce"), key);
    }

    #[test]
    fn handshake_requires_the_right_answer() {
        let key: &[u8; KEY_ENCODED_SIZE] = b"dGhlIHNhbXBsZSBub25jZQ==";
        let accept = accept_value(key);
        let good = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\nfirst frame";
        let head = good.len() - b"first frame".len();
        assert_eq!(read_handshake(good, &accept), Ok(Some(head)));
        for size in 0..head {
            assert_eq!(read_handshake(&good[..size], &accept), Ok(None));
        }
        let wrong = b"HTTP/1.1 101 Switching\r\nSec-WebSocket-Accept: AAAA\r\n\r\n";
        assert_eq!(read_handshake(wrong, &accept), Err(WsError::HandshakeRefused));
        let missing = b"HTTP/1.1 101 Switching\r\n\r\n";
        assert_eq!(read_handshake(missing, &accept), Err(WsError::HandshakeRefused));
        let status =
            b"HTTP/1.1 403 Forbidden\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
        assert_eq!(read_handshake(status, &accept), Err(WsError::HandshakeRefused));
        let deflate = b"HTTP/1.1 101 S\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n";
        assert_eq!(read_handshake(deflate, &accept), Err(WsError::HandshakeRefused));
    }

    #[test]
    fn writes_an_upgrade_request() {
        let mut target = [0u8; 512];
        let key = encode_key(&[7; 16]);
        let size = write_handshake(&mut target, "h.example", "/ws", &key).unwrap();
        let text = core::str::from_utf8(&target[..size]).unwrap();
        assert!(text.starts_with("GET /ws HTTP/1.1\r\nHost: h.example\r\n"));
        assert!(text.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(!text.contains("Extensions"));
        assert!(text.ends_with("\r\n\r\n"));
        assert_eq!(write_handshake(&mut [0u8; 4], "h", "/", &key), Err(WsError::BufferTooSmall));
    }

    #[test]
    fn seal_round_trips_at_each_length_form() {
        for size in [0usize, 1, 125, 126, 127, 65_535, 65_536, 70_000] {
            let mut target = vec![0u8; CLIENT_HEADER_SIZE_MAX + size];
            let payload: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
            target[CLIENT_HEADER_SIZE_MAX..].copy_from_slice(&payload);
            let range = seal_frame(&mut target, size, Opcode::Text, [1, 2, 3, 4]);
            let (first, read) = unseal(&target[range]);
            assert_eq!(first, 0x81, "size {size}");
            assert_eq!(read, payload, "size {size}");
        }
    }

    #[test]
    fn reads_server_frames_at_each_length_form() {
        let frame = read_frame(&[0x81, 2, b'h', b'i'], 1024).unwrap().unwrap();
        assert_eq!(frame.opcode, Opcode::Text);
        assert!(frame.fin);
        assert_eq!((frame.payload_start, frame.payload_size, frame.consumed), (2, 2, 4));

        let mut medium = vec![0x82, 126, 0x01, 0x00];
        medium.extend(vec![0u8; 256]);
        let frame = read_frame(&medium, 1024).unwrap().unwrap();
        assert_eq!((frame.payload_start, frame.payload_size), (4, 256));

        let mut long = vec![0x02, 127, 0, 0, 0, 0, 0, 1, 0, 0];
        long.extend(vec![0u8; 65_536]);
        let frame = read_frame(&long, 70_000).unwrap().unwrap();
        assert!(!frame.fin);
        assert_eq!((frame.payload_start, frame.payload_size), (10, 65_536));
    }

    #[test]
    fn every_prefix_asks_for_more() {
        let mut frame = vec![0x81, 126, 0x00, 0x80];
        frame.extend(vec![b'x'; 128]);
        for size in 0..frame.len() {
            assert_eq!(read_frame(&frame[..size], 1024), Ok(None), "prefix {size}");
        }
        assert!(read_frame(&frame, 1024).unwrap().is_some());
    }

    #[test]
    fn refuses_what_the_rfc_refuses() {
        for bad in [
            &[0xc1u8, 0][..],          // RSV1 with nothing negotiated
            &[0xa1, 0],                // RSV2
            &[0x91, 0],                // RSV3
            &[0x83, 0],                // reserved opcode
            &[0x8b, 0],                // reserved control opcode
            &[0x81, 0x80, 0, 0, 0, 0], // a masked server frame
            &[0x09, 0],                // a fragmented ping
            &[0x89, 126, 0, 126],      // a ping past the control limit
            &[0x81, 126, 0, 5],        // length not in its shortest form
            &[0x81, 127, 0, 0, 0, 0, 0, 0, 0, 5],
            &[0x81, 127, 0x80, 0, 0, 0, 0, 0, 0, 0],
        ] {
            assert_eq!(read_frame(bad, 1 << 20), Err(WsError::Protocol), "{bad:?}");
        }
        assert_eq!(read_frame(&[0x81, 126, 0x10, 0x00], 1024), Err(WsError::TooLarge));
    }
}
