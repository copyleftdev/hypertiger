//! HTTP/1.1, sans-IO: a request head and a response parser, over buffers the caller owns.
//!
//! Only what this API needs. No compression is ever requested, so none is handled, and a body with
//! no declared length is refused, because a connection closing is indistinguishable from a body
//! being cut short, and a cut-short reply must never look complete.

/// Status, headers and framing together must fit here.
pub const HEAD_SIZE_MAX: usize = 16 * 1024;
const CHUNK_SIZE_LINE_MAX: usize = 32;

/// Why a response cannot be read. The connection cannot be reused after any of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    Malformed,
    HeadTooLarge,
    /// The body is larger than the caller's buffer.
    BodyTooLarge,
    /// Neither `Content-Length` nor chunked framing, so its end could not be known.
    NoLength,
    /// The request head did not fit.
    BufferTooSmall,
}

/// A response read to its end. The body is at `body_start..body_start + body_size`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body_start: usize,
    pub body_size: usize,
    /// Bytes of the buffer this response used; anything after belongs to the next.
    pub consumed: usize,
    /// The server said it will close, so the connection is not worth keeping.
    pub connection_close: bool,
}

/// Writes a POST request's head. The body follows, in the caller's next write.
pub fn write_post_head(
    target: &mut [u8],
    host: &str,
    path: &str,
    body_size: usize,
) -> Result<usize, HttpError> {
    assert!(path.starts_with('/'));
    assert_ne!(host, "");
    let mut writer = Writer { target, size: 0 };
    writer.push(b"POST ")?;
    writer.push(path.as_bytes())?;
    writer.push(b" HTTP/1.1\r\nHost: ")?;
    writer.push(host.as_bytes())?;
    writer.push(b"\r\nContent-Type: application/json\r\nContent-Length: ")?;
    writer.push_decimal(body_size)?;
    writer.push(b"\r\nConnection: keep-alive\r\nUser-Agent: hypertiger\r\n\r\n")?;
    Ok(writer.size)
}

struct Writer<'b> {
    target: &'b mut [u8],
    size: usize,
}

impl Writer<'_> {
    fn push(&mut self, bytes: &[u8]) -> Result<(), HttpError> {
        let window = self
            .target
            .get_mut(self.size..self.size + bytes.len())
            .ok_or(HttpError::BufferTooSmall)?;
        window.copy_from_slice(bytes);
        self.size += bytes.len();
        Ok(())
    }

    fn push_decimal(&mut self, value: usize) -> Result<(), HttpError> {
        let mut digits = [0u8; 20];
        let mut position = digits.len();
        let mut remaining = value;
        for _ in 0..digits.len() {
            position -= 1;
            digits[position] = b'0' + (remaining % 10) as u8;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }
        assert_eq!(remaining, 0);
        self.push(&digits[position..])
    }
}

/// Finds the blank line ending a head, returning the offset just past it.
fn find_head_end(buffer: &[u8]) -> Option<usize> {
    let mut index = 0usize;
    while index + 4 <= buffer.len() {
        if &buffer[index..index + 4] == b"\r\n\r\n" {
            return Some(index + 4);
        }
        index += 1;
    }
    None
}

#[derive(Clone, Copy)]
enum Framing {
    Length(usize),
    Chunked,
}

struct Head {
    status: u16,
    framing: Framing,
    connection_close: bool,
}

/// Reads a response from the start of `buffer`. `Ok(None)` means more bytes are needed.
///
/// `buffer` is mutable because chunked bodies are unframed in place. Calling this again on a buffer
/// that returned `Some` would read the unframed bytes as framing, so a complete response is final.
pub fn parse_response(buffer: &mut [u8], filled: usize) -> Result<Option<Response>, HttpError> {
    assert!(filled <= buffer.len());
    let Some(head_end) = find_head_end(&buffer[..filled]) else {
        return if filled > HEAD_SIZE_MAX { Err(HttpError::HeadTooLarge) } else { Ok(None) };
    };
    let head = parse_head(&buffer[..head_end])?;
    let body = match head.framing {
        Framing::Length(size) => {
            if size > buffer.len() - head_end {
                return Err(HttpError::BodyTooLarge);
            }
            if filled - head_end < size {
                return Ok(None);
            }
            Some((size, head_end + size))
        }
        Framing::Chunked => unchunk(buffer, head_end, filled)?,
    };
    Ok(body.map(|(body_size, consumed)| {
        assert!(consumed <= filled);
        assert!(head_end + body_size <= consumed);
        Response {
            status: head.status,
            body_start: head_end,
            body_size,
            consumed,
            connection_close: head.connection_close,
        }
    }))
}

fn parse_head(head: &[u8]) -> Result<Head, HttpError> {
    let mut lines = head.split(|&byte| byte == b'\n');
    let status_line = lines.next().ok_or(HttpError::Malformed)?;
    let status = parse_status(status_line)?;
    let mut length = None;
    let mut chunked = false;
    let mut connection_close = false;
    for line in lines {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        let colon = line.iter().position(|&byte| byte == b':').ok_or(HttpError::Malformed)?;
        let name = &line[..colon];
        let value = line[colon + 1..].trim_ascii();
        if name.eq_ignore_ascii_case(b"content-length") {
            length = Some(parse_size(value).ok_or(HttpError::Malformed)?);
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            chunked = value.eq_ignore_ascii_case(b"chunked");
            if !chunked {
                return Err(HttpError::Malformed);
            }
        } else if name.eq_ignore_ascii_case(b"connection") {
            connection_close = value.eq_ignore_ascii_case(b"close");
        }
    }
    let framing = match (chunked, length) {
        // Both at once is the classic request-smuggling shape. No honest server sends it.
        (true, Some(_)) => return Err(HttpError::Malformed),
        (true, None) => Framing::Chunked,
        (false, Some(size)) => Framing::Length(size),
        (false, None) => return Err(HttpError::NoLength),
    };
    Ok(Head { status, framing, connection_close })
}

fn parse_status(line: &[u8]) -> Result<u16, HttpError> {
    let rest = line.strip_prefix(b"HTTP/1.").ok_or(HttpError::Malformed)?;
    let code = rest.get(2..5).ok_or(HttpError::Malformed)?;
    if rest.get(1) != Some(&b' ') {
        return Err(HttpError::Malformed);
    }
    let mut status = 0u16;
    for &digit in code {
        if !digit.is_ascii_digit() {
            return Err(HttpError::Malformed);
        }
        status = status * 10 + u16::from(digit - b'0');
    }
    if (100..=599).contains(&status) { Ok(status) } else { Err(HttpError::Malformed) }
}

fn parse_size(text: &[u8]) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let mut value = 0usize;
    for &digit in text {
        if !digit.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(usize::from(digit - b'0'))?;
    }
    Some(value)
}

fn parse_chunk_size(line: &[u8]) -> Option<usize> {
    // A chunk extension after `;` is legal and never used here, so it is dropped.
    let digits = line.split(|&byte| byte == b';').next()?;
    if digits.is_empty() {
        return None;
    }
    let mut value = 0usize;
    for &digit in digits {
        let nibble = char::from(digit).to_digit(16)?;
        value = value.checked_mul(16)?.checked_add(nibble as usize)?;
    }
    Some(value)
}

/// Checks that a whole chunked body has arrived, then slides its chunks together. Nothing moves
/// until the terminator is seen, so a partial read can be retried from the same bytes.
fn unchunk(
    buffer: &mut [u8],
    start: usize,
    filled: usize,
) -> Result<Option<(usize, usize)>, HttpError> {
    let Some(end) = chunked_end(&buffer[start..filled])? else {
        return Ok(None);
    };
    let mut read = start;
    let mut write = start;
    // A chunk is at least its size line, so the framed length bounds the number of passes.
    for _ in 0..end {
        let line_size = line_length(&buffer[read..filled]).ok_or(HttpError::Malformed)?;
        let size = parse_chunk_size(&buffer[read..read + line_size]).ok_or(HttpError::Malformed)?;
        read += line_size + 2;
        if size == 0 {
            return Ok(Some((write - start, start + end)));
        }
        buffer.copy_within(read..read + size, write);
        write += size;
        read += size + 2;
    }
    Err(HttpError::Malformed)
}

/// The length of the framed body, if all of it has arrived.
fn chunked_end(framed: &[u8]) -> Result<Option<usize>, HttpError> {
    let mut cursor = 0usize;
    for _ in 0..=framed.len() {
        let Some(line_size) = line_length(&framed[cursor..]) else {
            if framed.len() - cursor > CHUNK_SIZE_LINE_MAX {
                return Err(HttpError::Malformed);
            }
            return Ok(None);
        };
        let size = parse_chunk_size(&framed[cursor..cursor + line_size]);
        let size = size.ok_or(HttpError::Malformed)?;
        cursor += line_size + 2;
        if size == 0 {
            // Trailers are never requested, so the terminator is exactly this CRLF.
            return match framed.get(cursor..cursor + 2) {
                None => Ok(None),
                Some(b"\r\n") => Ok(Some(cursor + 2)),
                Some(_) => Err(HttpError::Malformed),
            };
        }
        let next = cursor.checked_add(size).ok_or(HttpError::Malformed)?;
        let after = next.checked_add(2).ok_or(HttpError::Malformed)?;
        match framed.get(next..after) {
            None => return Ok(None),
            Some(b"\r\n") => cursor = next + 2,
            Some(_) => return Err(HttpError::Malformed),
        }
    }
    Err(HttpError::Malformed)
}

fn line_length(bytes: &[u8]) -> Option<usize> {
    let mut index = 0usize;
    while index + 1 < bytes.len() {
        if bytes[index] == b'\r' && bytes[index + 1] == b'\n' {
            return Some(index);
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn parse(text: &[u8]) -> Result<Option<Response>, HttpError> {
        let mut buffer = text.to_vec();
        buffer.resize(4096, 0);
        parse_response(&mut buffer, text.len())
    }

    fn body_of(text: &[u8]) -> Vec<u8> {
        let mut buffer = text.to_vec();
        buffer.resize(4096, 0);
        let response = parse_response(&mut buffer, text.len()).unwrap().unwrap();
        buffer[response.body_start..response.body_start + response.body_size].to_vec()
    }

    #[test]
    fn writes_a_request_head() {
        let mut target = [0u8; 256];
        let size = write_post_head(&mut target, "api.example", "/info", 17).unwrap();
        let text = core::str::from_utf8(&target[..size]).unwrap();
        assert!(text.starts_with("POST /info HTTP/1.1\r\nHost: api.example\r\n"));
        assert!(text.contains("Content-Length: 17\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
        assert!(!text.to_ascii_lowercase().contains("accept-encoding"));
        assert_eq!(write_post_head(&mut [0u8; 8], "h", "/", 0), Err(HttpError::BufferTooSmall));
    }

    #[test]
    fn reads_a_length_framed_response() {
        let text = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let response = parse(text).unwrap().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.consumed, text.len());
        assert!(!response.connection_close);
        assert_eq!(body_of(text), b"ok");
    }

    #[test]
    fn every_prefix_asks_for_more_and_never_completes() {
        let text = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
        for size in 0..text.len() {
            assert_eq!(parse(&text[..size]), Ok(None), "prefix {size}");
        }
        assert!(parse(text).unwrap().is_some());
    }

    #[test]
    fn unframes_chunks_and_keeps_what_follows() {
        let text = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\na;x=1\r\n0123456789\r\n0\r\n\r\nNEXT";
        let mut buffer = text.to_vec();
        buffer.resize(4096, 0);
        let response = parse_response(&mut buffer, text.len()).unwrap().unwrap();
        assert_eq!(&buffer[response.body_start..][..response.body_size], b"abc0123456789");
        assert_eq!(&text[response.consumed..], b"NEXT");
    }

    #[test]
    fn every_prefix_of_a_chunked_body_asks_for_more() {
        let text = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        for size in 0..text.len() {
            assert_eq!(parse(&text[..size]), Ok(None), "prefix {size}");
        }
        assert_eq!(body_of(text), b"abc");
    }

    #[test]
    fn refuses_what_it_cannot_frame() {
        assert_eq!(parse(b"HTTP/1.1 200 OK\r\n\r\n"), Err(HttpError::NoLength));
        assert_eq!(parse(b"nonsense\r\n\r\n"), Err(HttpError::Malformed));
        assert_eq!(parse(b"HTTP/1.1 2x0 OK\r\n\r\n"), Err(HttpError::Malformed));
        assert_eq!(
            parse(b"HTTP/1.1 999 OK\r\nContent-Length: 0\r\n\r\n"),
            Err(HttpError::Malformed)
        );
        assert_eq!(
            parse(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(HttpError::Malformed)
        );
        assert_eq!(
            parse(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n"),
            Err(HttpError::Malformed)
        );
        assert_eq!(
            parse(b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n"),
            Err(HttpError::Malformed)
        );
        assert_eq!(parse(b"HTTP/1.1 200 OK\r\nno colon here\r\n\r\n"), Err(HttpError::Malformed));
        assert_eq!(
            parse(b"HTTP/1.1 200 OK\r\nContent-Length: 99999\r\n\r\n"),
            Err(HttpError::BodyTooLarge)
        );
        let bad = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZ\r\n";
        assert_eq!(parse(bad), Err(HttpError::Malformed));
        let bad = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nabc\r\n";
        assert_eq!(parse(bad), Err(HttpError::Malformed));
    }

    #[test]
    fn a_chunk_size_past_the_address_space_is_refused() {
        let text = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\nx";
        assert_eq!(parse(text), Err(HttpError::Malformed));
        let text = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nfffffffffffffff0\r\nx";
        assert_eq!(parse(text), Err(HttpError::Malformed));
    }

    #[test]
    fn head_size_is_bounded() {
        let mut text = b"HTTP/1.1 200 OK\r\n".to_vec();
        text.resize(HEAD_SIZE_MAX + 2, b'x');
        let filled = text.len();
        assert_eq!(parse_response(&mut text, filled), Err(HttpError::HeadTooLarge));
    }

    #[test]
    fn notes_a_closing_connection() {
        let text = b"HTTP/1.1 500 Oops\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
        let response = parse(text).unwrap().unwrap();
        assert_eq!(response.status, 500);
        assert!(response.connection_close);
    }
}
