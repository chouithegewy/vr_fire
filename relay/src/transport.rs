//! WebSocket transport for the relay (F01 backpressure): explicit Tungstenite limits,
//! deadline-aware socket I/O, write-failure classification and a bounded handshake.
//!
//! `Guarded` wraps the socket. Before every WebSocket operation the worker calls `arm()`,
//! which starts one total write deadline for that operation: partial writes and flushes all
//! count against it, and each OS write timeout is capped to the time remaining. Reads poll
//! (a timeout is a normal "nothing yet"), except during the handshake, whose deadline is
//! absolute. Any write failure is remembered so a failed automatic Pong/Close reply inside
//! `ws.read()` is recognised as a write failure, which is always terminal.

use crate::limits::Limits;
use crate::outbox::CloseReason;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::{HandshakeError, WebSocket};

/// Monotonic time source (fake in tests).
pub trait Clock {
    fn now(&self) -> Instant;
}

#[derive(Clone, Copy, Default)]
pub struct RealClock;

impl Clock for RealClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A byte stream whose blocking reads and writes can be given timeouts.
pub trait Socket: Read + Write {
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()>;
}

impl Socket for TcpStream {
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, d)
    }
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, d)
    }
}

/// Tungstenite limits from the relay's `Limits`: small frames/messages, eager writes and a
/// bounded write buffer. Client frames must stay masked.
pub fn ws_config(l: &Limits) -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(l.read_buffer)
        .write_buffer_size(0)
        .max_write_buffer_size(l.max_write_buffer)
        .max_message_size(Some(l.max_message))
        .max_frame_size(Some(l.max_frame))
        .accept_unmasked_frames(false)
}

pub struct Guarded<S, C> {
    inner: S,
    clock: C,
    poll: Duration,
    write_limit: Duration,
    /// Absolute read deadline (handshake only).
    read_deadline: Option<Instant>,
    /// Total deadline for the current operation's writes.
    op_deadline: Option<Instant>,
    write_failed: Option<io::ErrorKind>,
    cancelled: Arc<AtomicBool>,
    last_read_timeout: Option<Duration>,
    last_write_timeout: Option<Duration>,
}

impl<S: Socket, C: Clock> Guarded<S, C> {
    pub fn new(inner: S, clock: C, limits: &Limits) -> Self {
        Guarded {
            inner,
            clock,
            poll: limits.read_poll,
            write_limit: limits.write_deadline,
            read_deadline: None,
            op_deadline: None,
            write_failed: None,
            cancelled: Arc::default(),
            last_read_timeout: None,
            last_write_timeout: None,
        }
    }

    /// Start a new operation: its writes (including implicit protocol replies) share one deadline.
    pub fn arm(&mut self) {
        self.op_deadline = Some(self.clock.now() + self.write_limit);
    }

    /// The kind of the first write failure, if any write has failed.
    pub fn write_failed(&self) -> Option<io::ErrorKind> {
        self.write_failed
    }

    /// Setting this makes every later read and write fail.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    fn check_cancel(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "cancelled"));
        }
        Ok(())
    }

    fn fail_write(&mut self, kind: io::ErrorKind) -> io::Error {
        // WouldBlock from a timed-out blocking write is a timeout; either way it's terminal.
        let kind = if kind == io::ErrorKind::WouldBlock { io::ErrorKind::TimedOut } else { kind };
        self.write_failed.get_or_insert(kind);
        kind.into()
    }

    /// Time left before the current write deadline (arming now if nothing armed it).
    fn write_budget(&mut self) -> Duration {
        let now = self.clock.now();
        let deadline = *self.op_deadline.get_or_insert(now + self.write_limit);
        deadline.saturating_duration_since(now)
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }
}

impl<S: Socket, C: Clock> Read for Guarded<S, C> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check_cancel()?;
        let mut timeout = self.poll;
        if let Some(deadline) = self.read_deadline {
            let left = deadline.saturating_duration_since(self.clock.now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "handshake deadline"));
            }
            timeout = timeout.min(left);
        }
        // A zero timeout means "block forever" to the OS.
        let timeout = timeout.max(Duration::from_millis(1));
        if self.last_read_timeout != Some(timeout) {
            self.inner.set_read_timeout(Some(timeout))?;
            self.last_read_timeout = Some(timeout);
        }
        self.inner.read(buf)
    }
}

impl<S: Socket, C: Clock> Write for Guarded<S, C> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(kind) = self.write_failed {
            return Err(kind.into());
        }
        if let Err(e) = self.check_cancel() {
            return Err(self.fail_write(e.kind()));
        }
        let left = self.write_budget();
        if left.is_zero() {
            return Err(self.fail_write(io::ErrorKind::TimedOut));
        }
        let left = left.max(Duration::from_millis(1));
        if self.last_write_timeout != Some(left) {
            if let Err(e) = self.inner.set_write_timeout(Some(left)) {
                return Err(self.fail_write(e.kind()));
            }
            self.last_write_timeout = Some(left);
        }
        match self.inner.write(buf) {
            Ok(0) if !buf.is_empty() => Err(self.fail_write(io::ErrorKind::WriteZero)),
            Ok(n) => Ok(n),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(0),
            Err(e) => Err(self.fail_write(e.kind())),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(kind) = self.write_failed {
            return Err(kind.into());
        }
        if self.write_budget().is_zero() {
            return Err(self.fail_write(io::ErrorKind::TimedOut));
        }
        self.inner.flush().map_err(|e| self.fail_write(e.kind()))
    }
}

/// Accept a WebSocket within `limits.handshake_deadline` of now, however slowly bytes arrive.
pub fn handshake<S: Socket, C: Clock>(
    mut stream: Guarded<S, C>,
    limits: &Limits,
) -> Result<WebSocket<Guarded<S, C>>, String> {
    let deadline = stream.clock.now() + limits.handshake_deadline;
    stream.read_deadline = Some(deadline);
    stream.op_deadline = Some(deadline);
    let mut attempt = tungstenite::accept_with_config(stream, Some(ws_config(limits)));
    loop {
        match attempt {
            Ok(mut ws) => {
                let g = ws.get_mut();
                g.read_deadline = None;
                g.op_deadline = None;
                return Ok(ws);
            }
            Err(HandshakeError::Interrupted(mid)) => {
                // Resume the same handshake state; the deadline is absolute.
                if mid.get_ref().get_ref().clock.now() >= deadline {
                    return Err("handshake deadline".into());
                }
                attempt = mid.handshake();
            }
            Err(HandshakeError::Failure(e)) => return Err(format!("handshake failed: {e}")),
        }
    }
}

/// What a worker does after a Tungstenite error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Read poll found nothing; carry on.
    Poll,
    Close(CloseReason),
}

/// Classify an error from `ws.read()`/`send()`/`flush()`. `write_failed` comes from
/// `Guarded::write_failed()`: any write failure is terminal, even inside a read.
pub fn classify(err: &tungstenite::Error, write_failed: Option<io::ErrorKind>) -> Next {
    use tungstenite::Error as E;
    if let Some(kind) = write_failed {
        return Next::Close(if kind == io::ErrorKind::TimedOut { CloseReason::WriteTimeout } else { CloseReason::WriteError });
    }
    match err {
        E::Io(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Next::Poll,
        E::Io(e) if e.kind() == io::ErrorKind::ConnectionAborted => Next::Close(CloseReason::ReadError),
        E::WriteBufferFull(_) => Next::Close(CloseReason::WriteError),
        E::Capacity(_) => Next::Close(CloseReason::TooLarge),
        E::ConnectionClosed | E::AlreadyClosed => Next::Close(CloseReason::ClientClose),
        E::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake) => Next::Close(CloseReason::ClientClose),
        _ => Next::Close(CloseReason::ReadError),
    }
}

#[cfg(test)]
pub mod testing {
    //! Fake clock and socket for deterministic transport tests.
    use super::*;
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Clone)]
    pub struct FakeClock(pub Rc<Cell<Instant>>);

    impl FakeClock {
        pub fn new() -> Self {
            FakeClock(Rc::new(Cell::new(Instant::now())))
        }
        pub fn advance(&self, d: Duration) {
            self.0.set(self.0.get() + d);
        }
        pub fn since(&self, t0: Instant) -> Duration {
            self.0.get() - t0
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.0.get()
        }
    }

    pub enum WriteMode {
        Ok,
        /// Accept this many bytes per call, taking `cost` each.
        Trickle(usize, Duration),
        /// Every write fails with this error kind.
        Fail(io::ErrorKind),
    }

    /// In-memory socket: reads come from `input` (`read_chunk` bytes at a time, `read_cost`
    /// each); an empty input times out after the current read timeout.
    pub struct FakeSocket {
        pub clock: FakeClock,
        pub input: VecDeque<u8>,
        pub read_chunk: usize,
        pub read_cost: Duration,
        pub output: Vec<u8>,
        pub write_mode: WriteMode,
        read_timeout: Cell<Option<Duration>>,
    }

    impl FakeSocket {
        pub fn new(clock: &FakeClock, input: &[u8]) -> Self {
            FakeSocket {
                clock: clock.clone(),
                input: input.iter().copied().collect(),
                read_chunk: usize::MAX,
                read_cost: Duration::ZERO,
                output: Vec::new(),
                write_mode: WriteMode::Ok,
                read_timeout: Cell::new(None),
            }
        }
    }

    impl Socket for FakeSocket {
        fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
            self.read_timeout.set(d);
            Ok(())
        }
        fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for FakeSocket {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.input.is_empty() {
                self.clock.advance(self.read_timeout.get().unwrap_or(Duration::from_millis(20)));
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.clock.advance(self.read_cost);
            let n = buf.len().min(self.read_chunk).min(self.input.len());
            for b in buf.iter_mut().take(n) {
                *b = self.input.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    impl Write for FakeSocket {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match self.write_mode {
                WriteMode::Ok => {
                    self.output.extend_from_slice(buf);
                    Ok(buf.len())
                }
                WriteMode::Trickle(n, cost) => {
                    self.clock.advance(cost);
                    let n = n.min(buf.len());
                    self.output.extend_from_slice(&buf[..n]);
                    Ok(n)
                }
                WriteMode::Fail(kind) => Err(kind.into()),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub const UPGRADE: &[u8] = b"GET /vr_fire/ws HTTP/1.1\r\nHost: chilos.dev\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";

    /// A masked client frame.
    pub fn client_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![(if fin { 0x80 } else { 0 }) | opcode];
        match payload.len() {
            n if n < 126 => f.push(0x80 | n as u8),
            n if n <= 0xffff => {
                f.push(0x80 | 126);
                f.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                f.push(0x80 | 127);
                f.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        let key = [0x12, 0x34, 0x56, 0x78];
        f.extend_from_slice(&key);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        f
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use tungstenite::Message;
    use tungstenite::protocol::Role;

    fn server(sock: FakeSocket, clock: &FakeClock, l: &Limits) -> WebSocket<Guarded<FakeSocket, FakeClock>> {
        WebSocket::from_raw_socket(Guarded::new(sock, clock.clone(), l), Role::Server, Some(ws_config(l)))
    }

    #[test]
    fn a_writer_making_tiny_progress_still_hits_the_total_deadline() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut sock = FakeSocket::new(&clock, b"");
        sock.write_mode = WriteMode::Trickle(1, Duration::from_millis(7));
        let mut ws = server(sock, &clock, &l);
        let t0 = clock.now();
        ws.get_mut().arm();
        let err = ws.send(Message::text("x".repeat(3000))).unwrap_err();
        let took = clock.since(t0);
        assert!(took >= l.write_deadline && took < l.write_deadline + Duration::from_millis(10), "took {took:?}");
        assert_eq!(ws.get_ref().write_failed(), Some(io::ErrorKind::TimedOut));
        assert_eq!(classify(&err, ws.get_ref().write_failed()), Next::Close(CloseReason::WriteTimeout));
    }

    #[test]
    fn a_healthy_send_completes_and_rearming_resets_the_deadline() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut ws = server(FakeSocket::new(&clock, b""), &clock, &l);
        for _ in 0..3 {
            ws.get_mut().arm();
            ws.send(Message::text("{\"t\":\"s\"}")).unwrap();
            clock.advance(Duration::from_secs(1));
        }
        assert_eq!(ws.get_ref().write_failed(), None);
        assert_eq!(ws.get_ref().get_ref().output.len(), 3 * (2 + 9));
    }

    #[test]
    fn a_trickled_handshake_expires_at_the_absolute_deadline() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut sock = FakeSocket::new(&clock, UPGRADE);
        sock.read_chunk = 1;
        sock.read_cost = Duration::from_millis(100);
        let t0 = clock.now();
        assert!(handshake(Guarded::new(sock, clock.clone(), &l), &l).is_err());
        let took = clock.since(t0);
        assert!(took <= l.handshake_deadline + Duration::from_millis(100), "took {took:?}");
    }

    #[test]
    fn a_silent_handshake_expires_too() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let t0 = clock.now();
        assert!(handshake(Guarded::new(FakeSocket::new(&clock, b""), clock.clone(), &l), &l).is_err());
        assert!(clock.since(t0) <= l.handshake_deadline + Duration::from_millis(30));
    }

    #[test]
    fn a_prompt_handshake_succeeds_and_reads_a_text_message() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut ws = handshake(Guarded::new(FakeSocket::new(&clock, UPGRADE), clock.clone(), &l), &l).unwrap();
        assert!(ws.get_ref().get_ref().output.starts_with(b"HTTP/1.1 101"));
        // Clients send frames only after the 101 response.
        ws.get_mut().get_mut().input.extend(client_frame(true, 0x1, b"{\"t\":\"ping\",\"c\":1}"));
        ws.get_mut().arm();
        assert_eq!(ws.read().unwrap(), Message::text("{\"t\":\"ping\",\"c\":1}"));
        // Nothing more: a poll, not an error that closes the connection.
        ws.get_mut().arm();
        let err = ws.read().unwrap_err();
        assert_eq!(classify(&err, ws.get_ref().write_failed()), Next::Poll);
    }

    #[test]
    fn exactly_max_message_is_accepted_and_one_more_byte_is_rejected() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let ok = client_frame(true, 0x1, &vec![b'a'; l.max_message]);
        let mut ws = server(FakeSocket::new(&clock, &ok), &clock, &l);
        ws.get_mut().arm();
        assert_eq!(ws.read().unwrap().len(), l.max_message);

        let big = client_frame(true, 0x1, &vec![b'a'; l.max_message + 1]);
        let mut ws = server(FakeSocket::new(&clock, &big), &clock, &l);
        ws.get_mut().arm();
        let err = ws.read().unwrap_err();
        assert_eq!(classify(&err, ws.get_ref().write_failed()), Next::Close(CloseReason::TooLarge));
    }

    #[test]
    fn an_oversized_fragmented_message_is_rejected() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut input = client_frame(false, 0x1, &vec![b'a'; 3000]);
        input.extend(client_frame(true, 0x0, &vec![b'a'; 3000]));
        let mut ws = server(FakeSocket::new(&clock, &input), &clock, &l);
        ws.get_mut().arm();
        let err = loop {
            match ws.read() {
                Err(e) if classify(&e, None) != Next::Poll => break e,
                Err(_) => continue,
                Ok(m) => panic!("accepted {} bytes", m.len()),
            }
        };
        assert_eq!(classify(&err, ws.get_ref().write_failed()), Next::Close(CloseReason::TooLarge));
    }

    #[test]
    fn unmasked_client_frames_are_rejected() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let unmasked = [0x81u8, 0x02, b'h', b'i'];
        let mut ws = server(FakeSocket::new(&clock, &unmasked), &clock, &l);
        ws.get_mut().arm();
        let err = ws.read().unwrap_err();
        assert!(matches!(classify(&err, None), Next::Close(_)));
    }

    #[test]
    fn a_failed_automatic_pong_is_a_terminal_write_failure() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut sock = FakeSocket::new(&clock, &client_frame(true, 0x9, b"hb"));
        sock.write_mode = WriteMode::Fail(io::ErrorKind::BrokenPipe);
        let mut ws = server(sock, &clock, &l);
        let err = loop {
            ws.get_mut().arm();
            match ws.read() {
                Ok(_) => continue, // the Ping itself is surfaced; the Pong goes out on the next call
                Err(e) => break e,
            }
        };
        assert_eq!(ws.get_ref().write_failed(), Some(io::ErrorKind::BrokenPipe));
        assert_eq!(classify(&err, ws.get_ref().write_failed()), Next::Close(CloseReason::WriteError));
    }

    #[test]
    fn a_failed_explicit_flush_is_terminal() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut sock = FakeSocket::new(&clock, b"");
        sock.write_mode = WriteMode::Fail(io::ErrorKind::ConnectionReset);
        let mut ws = server(sock, &clock, &l);
        ws.get_mut().arm();
        let err = ws.send(Message::text("{}")).unwrap_err();
        assert_eq!(classify(&err, ws.get_ref().write_failed()), Next::Close(CloseReason::WriteError));
        ws.get_mut().arm();
        let err = ws.flush().unwrap_err();
        assert_eq!(classify(&err, ws.get_ref().write_failed()), Next::Close(CloseReason::WriteError));
    }

    #[test]
    fn cancellation_fails_reads_and_writes() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut ws = server(FakeSocket::new(&clock, &client_frame(true, 0x1, b"hi")), &clock, &l);
        ws.get_ref().cancel_flag().store(true, Ordering::SeqCst);
        ws.get_mut().arm();
        let err = ws.read().unwrap_err();
        assert!(matches!(classify(&err, ws.get_ref().write_failed()), Next::Close(_)));
        ws.get_mut().arm();
        assert!(ws.send(Message::text("x")).is_err());
    }

    #[test]
    fn a_client_close_is_classified() {
        let l = Limits::default();
        let clock = FakeClock::new();
        let mut ws = server(FakeSocket::new(&clock, &client_frame(true, 0x8, &[0x03, 0xe8])), &clock, &l);
        let err = loop {
            ws.get_mut().arm();
            match ws.read() {
                Ok(_) => continue,
                Err(e) if classify(&e, ws.get_ref().write_failed()) == Next::Poll => continue,
                Err(e) => break e,
            }
        };
        assert_eq!(classify(&err, ws.get_ref().write_failed()), Next::Close(CloseReason::ClientClose));
    }
}
