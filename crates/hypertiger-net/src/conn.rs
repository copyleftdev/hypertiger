//! One TLS connection, with an HTTP client and a WebSocket client on top.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use hypertiger::http::{HttpError, Response, parse_response, write_post_head};
use hypertiger::ws::{self, CLIENT_HEADER_SIZE_MAX, Frame, Opcode, WsError};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore};

use crate::{Host, now_ms, random_u64};

/// How long a TCP and TLS setup may take. Set-up is the control plane, and a slow one is a failure.
pub const CONNECT_TIMEOUT_MS: u64 = 5_000;
/// How much is read from the socket on one pass, so that a firehose cannot starve the caller.
pub const READ_PASS_SIZE_MAX: usize = 256 * 1024;

#[derive(Debug)]
pub enum NetError {
    Io(std::io::Error),
    Tls(rustls::Error),
    Http(HttpError),
    Ws(WsError),
    Dns,
    /// The peer ended the connection.
    Closed,
    Timeout,
    BufferTooSmall,
}

impl From<std::io::Error> for NetError {
    fn from(error: std::io::Error) -> NetError {
        NetError::Io(error)
    }
}

impl From<rustls::Error> for NetError {
    fn from(error: rustls::Error) -> NetError {
        NetError::Tls(error)
    }
}

impl std::fmt::Display for NetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for NetError {}

fn tls_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
}

/// A byte stream the WebSocket client runs over. `Connection` is the real one; a test supplies one
/// that can stall, tear and fail on demand, which no real socket does when asked.
pub trait Stream {
    /// Moves bytes both ways, as far as possible without blocking.
    fn pump(&mut self) -> Result<(), NetError>;
    fn write_plain(&mut self, bytes: &[u8]) -> Result<(), NetError>;
    /// Reads whatever has arrived. `Ok(0)` means nothing yet.
    fn read_plain(&mut self, target: &mut [u8]) -> Result<usize, NetError>;
}

/// A non-blocking TLS stream. Plaintext goes in with `write_plain` and comes out with `read_plain`,
/// and `pump` moves ciphertext whenever it is called.
#[derive(Debug)]
pub struct Connection {
    socket: TcpStream,
    tls: ClientConnection,
    /// The peer ended the TCP stream. What it sent first is still worth reading, so this is
    /// reported only once the plaintext has run out.
    peer_closed: bool,
}

impl Connection {
    /// Connects and finishes the TLS handshake. This blocks for up to `CONNECT_TIMEOUT_MS`, and DNS
    /// can take longer, because the standard library offers no timeout for it.
    pub fn connect(host: Host) -> Result<Connection, NetError> {
        let mut addresses = (host.name, 443u16).to_socket_addrs()?;
        let address = addresses.next().ok_or(NetError::Dns)?;
        let timeout = Duration::from_millis(CONNECT_TIMEOUT_MS);
        let socket = TcpStream::connect_timeout(&address, timeout)?;
        // Each order is one small write, so waiting to coalesce them only adds latency.
        socket.set_nodelay(true)?;
        socket.set_nonblocking(true)?;

        let name = ServerName::try_from(host.name).map_err(|_| NetError::Dns)?;
        let tls = ClientConnection::new(tls_config(), name)?;
        let mut connection = Connection { socket, tls, peer_closed: false };
        let deadline_ms = now_ms() + CONNECT_TIMEOUT_MS;
        // Bounded by the deadline.
        while connection.tls.is_handshaking() {
            connection.pump()?;
            if now_ms() >= deadline_ms {
                return Err(NetError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(connection)
    }
}

impl Stream for Connection {
    fn pump(&mut self) -> Result<(), NetError> {
        let mut read_total = 0usize;
        while read_total < READ_PASS_SIZE_MAX && self.tls.wants_read() && !self.peer_closed {
            match self.tls.read_tls(&mut self.socket) {
                Ok(0) => self.peer_closed = true,
                Ok(size) => {
                    read_total += size;
                    self.tls.process_new_packets()?;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        while self.tls.wants_write() {
            match self.tls.write_tls(&mut self.socket) {
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn write_plain(&mut self, bytes: &[u8]) -> Result<(), NetError> {
        self.tls.writer().write_all(bytes)?;
        self.pump()
    }

    fn read_plain(&mut self, target: &mut [u8]) -> Result<usize, NetError> {
        // An empty target would read as "nothing yet" forever.
        assert_ne!(target.len(), 0);
        match self.tls.reader().read(target) {
            Ok(0) => Err(NetError::Closed),
            Ok(size) => Ok(size),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                if self.peer_closed {
                    Err(NetError::Closed)
                } else {
                    Ok(0)
                }
            }
            Err(error) => Err(error.into()),
        }
    }
}

/// A blocking request and response, for startup and the control plane. The trading path does not
/// use it: that goes over the WebSocket.
#[derive(Debug)]
pub struct HttpClient {
    host: Host,
    connection: Option<Connection>,
}

impl HttpClient {
    #[must_use]
    pub const fn new(host: Host) -> HttpClient {
        HttpClient { host, connection: None }
    }

    /// Posts `body` to `path`, reading the response into `buffer`, and returns the body's range.
    pub fn post(
        &mut self,
        path: &str,
        body: &[u8],
        buffer: &mut [u8],
        timeout_ms: u64,
    ) -> Result<Response, NetError> {
        let mut head = [0u8; 512];
        let head_size =
            write_post_head(&mut head, self.host.name, path, body.len()).map_err(NetError::Http)?;
        if self.connection.is_none() {
            self.connection = Some(Connection::connect(self.host)?);
        }
        let result = self.exchange(&head[..head_size], body, buffer, timeout_ms);
        let keep = matches!(&result, Ok(response) if !response.connection_close);
        if !keep {
            self.connection = None;
        }
        result
    }

    fn exchange(
        &mut self,
        head: &[u8],
        body: &[u8],
        buffer: &mut [u8],
        timeout_ms: u64,
    ) -> Result<Response, NetError> {
        let Some(connection) = self.connection.as_mut() else {
            return Err(NetError::Closed);
        };
        connection.write_plain(head)?;
        connection.write_plain(body)?;
        let deadline_ms = now_ms() + timeout_ms;
        let mut filled = 0usize;
        // Bounded by the deadline.
        loop {
            connection.pump()?;
            if filled == buffer.len() {
                return Err(NetError::BufferTooSmall);
            }
            let size = connection.read_plain(&mut buffer[filled..])?;
            filled += size;
            if size > 0 {
                if let Some(response) = parse_response(buffer, filled).map_err(NetError::Http)? {
                    return Ok(response);
                }
            } else if now_ms() >= deadline_ms {
                return Err(NetError::Timeout);
            } else {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }
}

/// What came off a WebSocket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WsEvent {
    /// A complete message is in the buffer at this range.
    Message { start: usize, size: usize },
    /// Nothing complete yet.
    Idle,
}

/// A WebSocket client over TLS. It keeps its own read buffer, sized at startup and never grown, and
/// the largest message the exchange can send has to fit that.
#[derive(Debug)]
pub struct WsClient<S: Stream = Connection> {
    connection: S,
    buffer: Vec<u8>,
    filled: usize,
    /// Consumed from the front of `buffer` by the previous message, reclaimed on the next read.
    consumed: usize,
    /// Where a fragmented message is being reassembled, in place: frames are slid together.
    fragment_size: usize,
    fragmenting: bool,
}

impl WsClient<Connection> {
    /// Connects and upgrades. `buffer_size` must hold the largest message expected.
    pub fn connect(host: Host, path: &str, buffer_size: usize) -> Result<Self, NetError> {
        let connection = Connection::connect(host)?;
        WsClient::upgrade(connection, host, path, buffer_size, CONNECT_TIMEOUT_MS)
    }
}

impl<S: Stream> WsClient<S> {
    /// Upgrades an open stream, waiting up to `timeout_ms` for the server's answer. This blocks, as
    /// connecting does: it belongs to the control plane.
    pub fn upgrade(
        mut connection: S,
        host: Host,
        path: &str,
        buffer_size: usize,
        timeout_ms: u64,
    ) -> Result<Self, NetError> {
        assert!(buffer_size > hypertiger::http::HEAD_SIZE_MAX);
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&random_u64().to_be_bytes());
        key[8..].copy_from_slice(&random_u64().to_be_bytes());
        let key_encoded = ws::encode_key(&key);
        let accept = ws::accept_value(&key_encoded);
        let mut request = [0u8; 512];
        let size = ws::write_handshake(&mut request, host.name, path, &key_encoded)
            .map_err(NetError::Ws)?;
        connection.write_plain(&request[..size])?;

        let mut client = WsClient {
            connection,
            buffer: vec![0; buffer_size],
            filled: 0,
            consumed: 0,
            fragment_size: 0,
            fragmenting: false,
        };
        client.finish_handshake(&accept, timeout_ms)?;
        Ok(client)
    }

    fn finish_handshake(
        &mut self,
        accept: &[u8; ws::ACCEPT_ENCODED_SIZE],
        timeout_ms: u64,
    ) -> Result<(), NetError> {
        let deadline_ms = now_ms() + timeout_ms;
        // Bounded by the deadline.
        loop {
            self.fill()?;
            let found = ws::read_handshake(&self.buffer[..self.filled], accept);
            if let Some(head_size) = found.map_err(NetError::Ws)? {
                self.buffer.copy_within(head_size..self.filled, 0);
                self.filled -= head_size;
                return Ok(());
            }
            if now_ms() >= deadline_ms {
                return Err(NetError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn fill(&mut self) -> Result<(), NetError> {
        self.connection.pump()?;
        if self.filled < self.buffer.len() {
            let size = self.connection.read_plain(&mut self.buffer[self.filled..])?;
            self.filled += size;
        }
        Ok(())
    }

    /// Sends one text message. `frame` has the payload at `CLIENT_HEADER_SIZE_MAX`, placed there by
    /// the caller, so that nothing is copied on the way out.
    pub fn send_text(&mut self, frame: &mut [u8], payload_size: usize) -> Result<(), NetError> {
        assert!(frame.len() >= CLIENT_HEADER_SIZE_MAX + payload_size);
        let mask = random_u64().to_be_bytes();
        let range =
            ws::seal_frame(frame, payload_size, Opcode::Text, [mask[0], mask[1], mask[2], mask[3]]);
        self.connection.write_plain(&frame[range])
    }

    fn send_pong(&mut self, payload_start: usize, payload_size: usize) -> Result<(), NetError> {
        let mut frame = [0u8; CLIENT_HEADER_SIZE_MAX + ws::CONTROL_PAYLOAD_MAX];
        assert!(payload_size <= ws::CONTROL_PAYLOAD_MAX);
        frame[CLIENT_HEADER_SIZE_MAX..CLIENT_HEADER_SIZE_MAX + payload_size]
            .copy_from_slice(&self.buffer[payload_start..payload_start + payload_size]);
        let mask = random_u64().to_be_bytes();
        let range = ws::seal_frame(
            &mut frame,
            payload_size,
            Opcode::Pong,
            [mask[0], mask[1], mask[2], mask[3]],
        );
        self.connection.write_plain(&frame[range])
    }

    /// Reads the next complete message, handling control frames. Bounded by the bytes in hand.
    pub fn poll(&mut self) -> Result<WsEvent, NetError> {
        if self.consumed > 0 {
            self.buffer.copy_within(self.consumed..self.filled, 0);
            self.filled -= self.consumed;
            self.consumed = 0;
        }
        self.fill()?;
        // Each pass removes one frame, and a frame takes at least two bytes.
        for _ in 0..=self.filled / 2 {
            let window = &self.buffer[self.fragment_size..self.filled];
            let capacity = self.buffer.len() - self.fragment_size;
            let Some(frame) = ws::read_frame(window, capacity).map_err(NetError::Ws)? else {
                // A frame that needs more bytes than the buffer has room for would wait forever. It
                // has to end the connection: only a reconnect can get past it.
                if self.filled == self.buffer.len() {
                    return Err(NetError::Ws(WsError::TooLarge));
                }
                return Ok(WsEvent::Idle);
            };
            if let Some(event) = self.on_frame(&frame)? {
                return Ok(event);
            }
        }
        Ok(WsEvent::Idle)
    }

    fn on_frame(&mut self, frame: &Frame) -> Result<Option<WsEvent>, NetError> {
        let frame_start = self.fragment_size;
        let payload_start = frame_start + frame.payload_start;
        match frame.opcode {
            Opcode::Close => Err(NetError::Closed),
            Opcode::Ping => {
                self.send_pong(payload_start, frame.payload_size)?;
                self.remove_frame(frame_start, frame.consumed);
                Ok(None)
            }
            Opcode::Pong => {
                self.remove_frame(frame_start, frame.consumed);
                Ok(None)
            }
            Opcode::Text | Opcode::Binary | Opcode::Continuation => {
                self.on_data(frame, payload_start)
            }
        }
    }

    fn on_data(
        &mut self,
        frame: &Frame,
        payload_start: usize,
    ) -> Result<Option<WsEvent>, NetError> {
        let is_continuation = frame.opcode == Opcode::Continuation;
        if is_continuation != self.fragmenting {
            return Err(NetError::Ws(WsError::Protocol));
        }
        // Slide the payload down against what has been assembled so far, over its own header.
        let frame_start = self.fragment_size;
        self.buffer.copy_within(payload_start..payload_start + frame.payload_size, frame_start);
        self.fragment_size += frame.payload_size;
        let after = frame_start + frame.consumed;
        self.buffer.copy_within(after..self.filled, self.fragment_size);
        self.filled -= frame.consumed - frame.payload_size;
        if frame.fin {
            let size = self.fragment_size;
            self.fragment_size = 0;
            self.fragmenting = false;
            self.consumed = size;
            Ok(Some(WsEvent::Message { start: 0, size }))
        } else {
            self.fragmenting = true;
            Ok(None)
        }
    }

    fn remove_frame(&mut self, frame_start: usize, frame_size: usize) {
        assert!(frame_start + frame_size <= self.filled);
        self.buffer.copy_within(frame_start + frame_size..self.filled, frame_start);
        self.filled -= frame_size;
    }

    /// The bytes of the message `poll` just reported.
    #[must_use]
    pub fn message(&self, start: usize, size: usize) -> &[u8] {
        &self.buffer[start..start + size]
    }
}
