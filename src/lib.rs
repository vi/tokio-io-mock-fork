#![cfg_attr(docsrs_alt, feature(doc_cfg))]
//! A mock type implementing [`AsyncRead`] and [`AsyncWrite`].
//!
//! (fork of [`tokio_test::io`](https://docs.rs/tokio-test/latest/tokio_test/io/index.html)
//!
//!
//! # Overview
//!
//! Provides a type that implements [`AsyncRead`] + [`AsyncWrite`] that can be configured
//! to handle an arbitrary sequence of read and write operations. This is useful
//! for writing unit tests for networking services as using an actual network
//! type is fairly non deterministic.
//!
//! # Usage
//!
//! Attempting to write data that the mock isn't expecting will result in a
//! panic.
//!
//! [`AsyncRead`]: tokio::io::AsyncRead
//! [`AsyncWrite`]: tokio::io::AsyncWrite

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio::time::{self, Duration, Instant, Sleep};
use tokio_stream::wrappers::UnboundedReceiverStream;

use futures_core::Stream;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{self, ready, Poll, Waker};
use std::{cmp, io};

#[cfg(feature = "text-scenarios")]
mod text_scenario;

#[cfg(feature = "text-scenarios")]
#[cfg_attr(docsrs_alt, doc(cfg(feature = "text-scenarios")))]
pub use text_scenario::ParseError;

#[cfg(feature = "panicless-mode")]
#[cfg_attr(docsrs_alt, doc(cfg(feature = "panicless-mode")))]
/// Details of an error detected by mock when in panicless mode.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MockOutcomeError {
    UnexpectedWrite,
    WriteInsteadOfShutdown,
    ShutdownInsteadOfWrite,
    WrittenByteMismatch { expected: u8, actual: u8 },
    RemainingUnreadData,
    RemainingUnwrittenData,
    Other,
}

#[cfg(feature = "panicless-mode")]
impl std::fmt::Display for MockOutcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MockOutcomeError::UnexpectedWrite => f.write_str("unexpected write"),
            MockOutcomeError::WrittenByteMismatch { expected, actual } => {
                write!(f, "mismatching byte, expected {expected}, got {actual}")
            }
            MockOutcomeError::RemainingUnreadData => f.write_str("data remains to be read"),
            MockOutcomeError::RemainingUnwrittenData => f.write_str("data remains to be written"),
            MockOutcomeError::Other => f.write_str("other error"),
            MockOutcomeError::WriteInsteadOfShutdown => {
                f.write_str("write where shutdown was expected")
            }
            MockOutcomeError::ShutdownInsteadOfWrite => {
                f.write_str("shutdown where a write was expected")
            }
        }
    }
}

#[cfg(feature = "panicless-mode")]
#[cfg_attr(docsrs_alt, doc(cfg(feature = "panicless-mode")))]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
/// Final report of a a mock when used in panicless mode
pub struct MockOutcome {
    /// `Ok` if the mock was dropped without an encountered error, `Err` otherwise
    pub outcome: Result<(), MockOutcomeError>,

    pub total_read_bytes: u64,
    pub total_written_bytes: u64,
}
#[cfg(feature = "panicless-mode")]
impl std::fmt::Display for MockOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.outcome {
            Ok(()) => write!(
                f,
                "success after reading {} and writing {} bytes",
                self.total_read_bytes, self.total_written_bytes
            ),
            Err(e) => write!(
                f,
                "error ({e}) after reading {} and writing {} bytes",
                self.total_read_bytes, self.total_written_bytes
            ),
        }
    }
}

/// An I/O object that follows a predefined script.
///
/// This value is created by `Builder` and implements `AsyncRead` + `AsyncWrite`. It
/// follows the scenario described by the builder and panics otherwise.
#[derive(Debug)]
pub struct Mock {
    inner: Inner,
}

/// A handle to send additional actions to the related `Mock`.
#[derive(Debug)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Action>,
}

/// Builds `Mock` instances.
#[derive(Debug, Clone, Default)]
pub struct Builder {
    // Sequence of actions for the Mock to take
    actions: VecDeque<Action>,
    name: String,
    shutdown_checking_enabled: bool,
}

#[derive(Debug, Clone)]
enum Action {
    Read(Vec<u8>),
    Write(Vec<u8>),
    Wait(Duration),
    // Wrapped in Arc so that Builder can be cloned and Send.
    // Mock is not cloned as does not need to check Rc for ref counts.
    ReadError(Option<Arc<io::Error>>),
    WriteError(Option<Arc<io::Error>>),
    WriteShutdown(bool),
    ReadZeroes(usize),
    WriteZeroes(usize),
    IgnoreWritten(usize),
    ReadEof(bool),
    StopChecking,
}

struct Inner {
    actions: VecDeque<Action>,
    waiting: Option<Instant>,
    sleep: Option<Pin<Box<Sleep>>>,
    read_wait: Option<Waker>,
    rx: UnboundedReceiverStream<Action>,
    name: String,
    checks_enabled: bool,
    read_bytes: u64,
    written_bytes: u64,
    shutdown_checking_enabled: bool,
    #[cfg(feature = "panicless-mode")]
    panicless_tx: Option<tokio::sync::oneshot::Sender<MockOutcome>>,
}

impl Builder {
    /// Return a new, empty `Builder`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sequence a `read` operation.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `buf`.
    pub fn read(&mut self, buf: &[u8]) -> &mut Self {
        self.actions.push_back(Action::Read(buf.into()));
        self
    }

    /// Sequence a `read` operation, resuting in a specified number of zero bytes.
    ///
    /// Same as [`Self::read`] with aa zero buffer, but with less memory consumption.
    ///
    /// The next operation in the mock's script will be to expect a `read` call.
    pub fn read_zeroes(&mut self, nbytes: usize) -> &mut Self {
        self.actions.push_back(Action::ReadZeroes(nbytes));
        self
    }

    /// Sequence a read operation that will return 0, i.e. end of file.
    ///
    /// The next operation in the mock's script will be to expect a `read` call.
    pub fn eof(&mut self) -> &mut Self {
        self.actions.push_back(Action::ReadEof(false));
        self
    }

    /// Sequence a `read` operation that produces an error.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `error`.
    pub fn read_error(&mut self, error: io::Error) -> &mut Self {
        let error = Some(error.into());
        self.actions.push_back(Action::ReadError(error));
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call.
    pub fn write(&mut self, buf: &[u8]) -> &mut Self {
        self.actions.push_back(Action::Write(buf.into()));
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call. The written bytes will be asserted to be equal to zero.
    pub fn write_zeroes(&mut self, nbytes: usize) -> &mut Self {
        self.actions.push_back(Action::WriteZeroes(nbytes));
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call. The written bytes will not be checked.
    ///
    /// Note that while content of the bytes are ignored,
    /// specified number of bytes still must be written to the mock to avoid the failure.
    pub fn write_ignore(&mut self, nbytes: usize) -> &mut Self {
        self.actions.push_back(Action::IgnoreWritten(nbytes));
        self
    }

    /// Sequence a `write` operation that produces an error.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call that provides `error`.
    pub fn write_error(&mut self, error: io::Error) -> &mut Self {
        let error = Some(error.into());
        self.actions.push_back(Action::WriteError(error));
        self
    }

    /// Check that [`AsyncWrite::poll_shutdown`]s happen where needed.
    ///
    /// It is disabled by default for compatibility with `tokio_test::io::Mock`.
    pub fn enable_shutdown_checking(&mut self) -> &mut Self {
        self.shutdown_checking_enabled = true;
        self
    }

    /// Sequence a `shutdown` operation that corresponds to an expected [`AsyncWrite::poll_shutdown`] call.
    ///
    /// Automatically does [`Self::enable_shutdown_checking`].
    pub fn write_shutdown(&mut self) -> &mut Self {
        self.shutdown_checking_enabled = true;
        self.actions.push_back(Action::WriteShutdown(false));
        self
    }

    /// Sequence a special event that makes this Mock stop asserting any operation (i.e. allow everything).
    ///
    /// Reaching this means the test is already succeeded and possible
    /// further operations are likely irrelevent.
    ///
    /// More reads can still be sequenced after this.
    pub fn stop_checking(&mut self) -> &mut Self {
        self.actions.push_back(Action::StopChecking);
        self
    }

    /// Sequence a wait.
    ///
    /// The next operation in the mock's script will be to wait without doing so
    /// for `duration` amount of time.
    pub fn wait(&mut self, duration: Duration) -> &mut Self {
        let duration = cmp::max(duration, Duration::from_millis(1));
        self.actions.push_back(Action::Wait(duration));
        self
    }

    /// Set name of the mock IO object to include in panic messages and debug output
    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = name.into();
        self
    }

    /// Sequence multiple operations using a special text scenario.
    ///
    /// Text scenario commands consist of a command character, subcommand
    /// character (if needed), content (if needed) and `|` terminator/separator.
    ///
    /// Parsing is not very strict.
    ///
    /// Commands:
    ///
    /// * `R` - sequence specified bytes to be read
    /// * `W` - sequence specified bytes to be written
    /// * `ZR` - sequence specified number of zero bytes to be read
    /// * `ZW` - sequence specified number of zero bytes to be written
    /// * `I` - sequence specified number of written bytes to be ignored
    /// * `X` - sequence a EOF event
    /// * `Q` - sequence stop_checking event
    /// * `ER` - sequence a read error
    /// * `EW` - sequence a write error
    /// * `D` - sequence a write shutdown
    /// * `T` - sequence a sleep for a specified number of milliseconds
    /// * `N` - set name of this mock stream object
    ///
    /// Most characters after `R` or `W` or `N` become content of the buffer.
    /// Exceptions: `|` character that marks end of command, leading (but not trailing) whitespace
    /// and escape sequences starting with `\`, like `\n` or `\xFF`.
    ///
    /// Numbers for `ZR`, `ZW`, `I` or `T` can be prefixed with `x` or `0x` to use hex instead of dec.
    ///
    /// Examples:
    ///
    /// * `R hello|W world` is equivalent to `.read(b"hello").write(b"world")`
    /// * `R \x55\x00| ZR x5500| R \x00\x03| ZR 3` is equivalent to `.read(b"\x55\x00").read_zeroes(0x5500).read(b"\x00\x03").read_zeroes(3)`
    /// * `R cat; echo qqq\n| R 1234|W 1234| R 56|W 56| X | W qqq|`
    /// * `N calc|R 2+2|W 4|R 3+$RANDOM|Q`
    /// * `R PING|W PONG|T5000|R PING|W PONG|T5000|ER`
    ///
    /// Mnemonics/explanations:
    ///
    /// * `T` - timeout, timer
    /// * `ZR` instead of `RZ` because of `Z` would be interpreted as a content of `R`
    /// * `Q` - quiet mode, quit checking, quash all assertions
    /// * `X` - eXit reading, eXtend writer span when reading is already finished. `E` is already busy for injected errors.
    /// * `D` - shutDown, `^D`` to send EOF in console
    #[cfg(feature = "text-scenarios")]
    #[cfg_attr(docsrs_alt, doc(cfg(feature = "text-scenarios")))]
    pub fn text_scenario(&mut self, scenario: &str) -> Result<&mut Self, ParseError> {
        let (items, name) = text_scenario::parse_text_scenario(scenario)?;
        if let Some(name) = name {
            self.name = name;
        }
        self.actions.extend(items);
        Ok(self)
    }

    /// Build a `Mock` value according to the defined script.
    pub fn build(&mut self) -> Mock {
        let (mock, _) = self.build_with_handle();
        mock
    }

    /// Build a `Mock` value paired with a handle
    pub fn build_with_handle(&mut self) -> (Mock, Handle) {
        let (inner, handle) = Inner::new(
            self.actions.clone(),
            self.name.clone(),
            self.shutdown_checking_enabled,
        );

        let mock = Mock { inner };

        (mock, handle)
    }

    /// Build a `Mock` value that should not normally panic.
    ///
    /// Instead of panicking when a mismatch is detected, it should signal the outcome to the channel.
    #[cfg(feature = "panicless-mode")]
    #[cfg_attr(docsrs_alt, doc(cfg(feature = "panicless-mode")))]
    pub fn build_panicless(
        &mut self,
    ) -> (Mock, Handle, tokio::sync::oneshot::Receiver<MockOutcome>) {
        let (mut inner, handle) = Inner::new(
            self.actions.clone(),
            self.name.clone(),
            self.shutdown_checking_enabled,
        );

        let (tx, rx) = tokio::sync::oneshot::channel();

        inner.panicless_tx = Some(tx);

        let mock = Mock { inner };

        (mock, handle, rx)
    }
}

impl Handle {
    /// Sequence a `read` operation.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `buf`.
    pub fn read(&mut self, buf: &[u8]) -> &mut Self {
        self.tx.send(Action::Read(buf.into())).unwrap();
        self
    }

    /// Sequence a `read` operation, resuting in a specified number of zero bytes.
    ///
    /// Same as [`Self::read`] with aa zero buffer, but with less memory consumption.
    ///
    /// The next operation in the mock's script will be to expect a `read` call.
    pub fn read_zeroes(&mut self, nbytes: usize) -> &mut Self {
        self.tx.send(Action::ReadZeroes(nbytes)).unwrap();
        self
    }

    /// Sequence a read operation that will return 0, i.e. end of file.
    ///
    /// The next operation in the mock's script will be to expect a `read` call.
    pub fn eof(&mut self) -> &mut Self {
        self.tx.send(Action::ReadEof(false)).unwrap();
        self
    }

    /// Sequence a `read` operation error.
    ///
    /// The next operation in the mock's script will be to expect a `read` call
    /// and return `error`.
    pub fn read_error(&mut self, error: io::Error) -> &mut Self {
        let error = Some(error.into());
        self.tx.send(Action::ReadError(error)).unwrap();
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call.
    pub fn write(&mut self, buf: &[u8]) -> &mut Self {
        self.tx.send(Action::Write(buf.into())).unwrap();
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call. The written bytes will be asserted to be equal to zero.
    pub fn write_zeroes(&mut self, nbytes: usize) -> &mut Self {
        self.tx.send(Action::WriteZeroes(nbytes)).unwrap();
        self
    }

    /// Sequence a `write` operation.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call. The written bytes will not be checked.
    ///
    /// Note that while content of the bytes are ignored,
    /// specified number of bytes still must be written to the mock to avoid the failure.
    pub fn write_ignore(&mut self, nbytes: usize) -> &mut Self {
        self.tx.send(Action::IgnoreWritten(nbytes)).unwrap();
        self
    }

    /// Sequence a `write` operation error.
    ///
    /// The next operation in the mock's script will be to expect a `write`
    /// call error.
    pub fn write_error(&mut self, error: io::Error) -> &mut Self {
        let error = Some(error.into());
        self.tx.send(Action::WriteError(error)).unwrap();
        self
    }

    /// Sequence a `shutdown` operation that corresponds to an expected [`AsyncWrite::poll_shutdown`] call.
    ///
    /// Note that unlike with [`Builder::write_shutdown`] you need to do [`Builder::enable_shutdown_checking`].explicitly.
    pub fn write_shutdown(&mut self) -> &mut Self {
        self.tx.send(Action::WriteShutdown(false)).unwrap();
        self
    }

    /// Sequence a special event that makes this Mock stop asserting any operation (i.e. allow everything).
    ///
    /// Reaching this means the test is already succeeded and possible
    /// further operations are likely irrelevent.
    ///
    /// More reads can still be sequenced after this.
    pub fn stop_checking(&mut self) -> &mut Self {
        self.tx.send(Action::StopChecking).unwrap();
        self
    }

    /// Sequence multiple operations using a special text scenario.
    ///
    /// See [`Builder::text_scenario`] for the description of the text content.
    ///
    /// Note that `N` (set name) command is ignored.
    #[cfg(feature = "text-scenarios")]
    #[cfg_attr(docsrs_alt, doc(cfg(feature = "text-scenarios")))]
    pub fn text_scenario(&mut self, scenario: &str) -> Result<&mut Self, ParseError> {
        let (items, name) = text_scenario::parse_text_scenario(scenario)?;
        if let Some(_name) = name {
            // ignoring the name in this context
        }
        for x in items {
            self.tx.send(x).unwrap();
        }
        Ok(self)
    }
}

impl Inner {
    fn new(
        actions: VecDeque<Action>,
        name: String,
        shutdown_checking_enabled: bool,
    ) -> (Inner, Handle) {
        let (tx, rx) = mpsc::unbounded_channel();

        let rx = UnboundedReceiverStream::new(rx);

        let inner = Inner {
            actions,
            sleep: None,
            read_wait: None,
            rx,
            waiting: None,
            name,
            checks_enabled: true,
            read_bytes: 0,
            written_bytes: 0,
            shutdown_checking_enabled,
            #[cfg(feature = "panicless-mode")]
            panicless_tx: None,
        };

        let handle = Handle { tx };

        (inner, handle)
    }

    fn poll_action(&mut self, cx: &mut task::Context<'_>) -> Poll<Option<Action>> {
        Pin::new(&mut self.rx).poll_next(cx)
    }

    fn read(&mut self, dst: &mut ReadBuf<'_>) -> io::Result<()> {
        match self.action() {
            Some(&mut Action::ReadZeroes(ref mut nbytes)) => {
                let n = cmp::min(dst.remaining(), *nbytes);
                let nfilled = dst.filled().len();
                dst.initialize_unfilled_to(nfilled + n)[nfilled..].fill(0);
                dst.set_filled(nfilled + n);
                *nbytes -= n;
                self.read_bytes += n as u64;
                Ok(())
            }
            Some(&mut Action::ReadEof(ref mut observed)) => {
                *observed = true;
                Ok(())
            }
            Some(&mut Action::Read(ref mut data)) => {
                // Figure out how much to copy
                let n = cmp::min(dst.remaining(), data.len());

                // Copy the data into the `dst` slice
                dst.put_slice(&data[..n]);

                // Drain the data from the source
                data.drain(..n);

                self.read_bytes += n as u64;

                Ok(())
            }
            Some(&mut Action::ReadError(ref mut err)) => {
                // As the
                let err = err.take().expect("Should have been removed from actions.");
                let err = Arc::try_unwrap(err).expect("There are no other references.");
                Err(err)
            }
            Some(_) => {
                // Either waiting or expecting a write
                Err(io::ErrorKind::WouldBlock.into())
            }
            None => Ok(()),
        }
    }

    fn write(&mut self, mut src: &[u8]) -> io::Result<usize> {
        let mut ret = 0;

        if self.actions.is_empty() {
            return Err(io::ErrorKind::BrokenPipe.into());
        }

        if let Some(&mut Action::Wait(..)) = self.action() {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        if let Some(&mut Action::WriteError(ref mut err)) = self.action() {
            let err = err.take().expect("Should have been removed from actions.");
            let err = Arc::try_unwrap(err).expect("There are no other references.");
            return Err(err);
        }

        let mut checks_enabled = self.checks_enabled;

        let n_remaining_actions = self.actions.len();
        for i in 0..n_remaining_actions {
            let action = &mut self.actions[i];
            let ignore_written = matches!(action, Action::IgnoreWritten { .. });
            match action {
                Action::Write(ref mut expect) => {
                    let n = cmp::min(src.len(), expect.len());

                    #[allow(unused_mut)]
                    let mut already_checked = false;

                    #[cfg(feature = "panicless-mode")]
                    if checks_enabled && self.panicless_tx.is_some() {
                        for i in 0..n {
                            let expected = expect[i];
                            let actual = src[i];
                            if expected != actual {
                                let _ = self.panicless_tx.take().unwrap().send(MockOutcome {
                                    outcome: Err(MockOutcomeError::WrittenByteMismatch {
                                        expected,
                                        actual,
                                    }),
                                    total_read_bytes: self.read_bytes,
                                    total_written_bytes: self.written_bytes,
                                });
                                self.checks_enabled = false;
                            }
                            self.written_bytes += 1;
                        }
                        already_checked = true;
                    }

                    if self.checks_enabled && !already_checked {
                        assert_eq!(
                            &src[..n],
                            &expect[..n],
                            "name={} r={} w={} remaining actions: {}",
                            self.name,
                            self.read_bytes,
                            self.written_bytes,
                            n_remaining_actions - i
                        );
                        self.written_bytes += n as u64;
                    }
                    if !already_checked {
                        self.written_bytes += n as u64;
                    }

                    // Drop data that was matched
                    expect.drain(..n);
                    src = &src[n..];

                    ret += n;

                    if src.is_empty() {
                        return Ok(ret);
                    }
                }
                Action::WriteZeroes(ref mut nbytes) | Action::IgnoreWritten(ref mut nbytes) => {
                    let n = cmp::min(src.len(), *nbytes);

                    #[allow(unused_mut)]
                    let mut already_checked = false;

                    #[cfg(feature = "panicless-mode")]
                    if checks_enabled && !ignore_written && self.panicless_tx.is_some() {
                        for i in 0..n {
                            let expected = 0;
                            let actual = src[i];
                            if expected != actual {
                                // lifetimes block refactoring here
                                let _ = self.panicless_tx.take().unwrap().send(MockOutcome {
                                    outcome: Err(MockOutcomeError::WrittenByteMismatch {
                                        expected,
                                        actual,
                                    }),
                                    total_read_bytes: self.read_bytes,
                                    total_written_bytes: self.written_bytes,
                                });
                                self.checks_enabled = false;
                            }
                            self.written_bytes += 1;
                        }
                        already_checked = true;
                    }

                    if checks_enabled && !ignore_written && !already_checked {
                        for (j, x) in src[..n].iter().enumerate() {
                            self.written_bytes += 1;
                            assert_eq!(
                                *x,
                                0,
                                "byte_index={j} r={} w={} name={} remaining actions: {}",
                                self.read_bytes,
                                self.written_bytes,
                                self.name,
                                n_remaining_actions - i
                            );
                        }
                    } else {
                        if !already_checked {
                            self.written_bytes += n as u64;
                        }
                    }

                    // Drop data that was matched
                    *nbytes -= n;
                    src = &src[n..];

                    ret += n;

                    if src.is_empty() {
                        return Ok(ret);
                    }
                }
                Action::WriteShutdown(ref mut observed) => {
                    #[cfg(feature = "panicless-mode")]
                    if checks_enabled && self.panicless_tx.is_some() {
                        let _ = self.panicless_tx.take().unwrap().send(MockOutcome {
                            outcome: Err(MockOutcomeError::WriteInsteadOfShutdown),
                            total_read_bytes: self.read_bytes,
                            total_written_bytes: self.written_bytes,
                        });
                        self.checks_enabled = false;
                        *observed=true;
                        return Err(io::ErrorKind::InvalidData.into())
                    }

                    if checks_enabled  {
                            panic!(
                                "unexpected write (a shutdown was expected) r={} w={} name={} remaining actions: {}",
                                self.read_bytes,
                                self.written_bytes,
                                self.name,
                                n_remaining_actions - i
                            );
                        
                    } else {
                        *observed=true;
                        return Err(io::ErrorKind::InvalidData.into())
                    }
                }
                Action::StopChecking => checks_enabled = false,
                Action::Wait(..) | Action::WriteError(..) => {
                    break;
                }
                _ => {}
            }

            // TODO: remove write
        }

        Ok(ret)
    }

    fn shutdown(&mut self) -> io::Result<()> {
        if self.actions.is_empty() {
            return Ok(());
        }

        if let Some(&mut Action::Wait(..)) = self.action() {
            return Err(io::ErrorKind::WouldBlock.into());
        }

        let mut checks_enabled = self.checks_enabled;

        let n_remaining_actions = self.actions.len();
        for i in 0..n_remaining_actions {
            let action = &mut self.actions[i];
            match action {
                Action::Write(..)
                | Action::IgnoreWritten(..)
                | Action::WriteError(..)
                | Action::WriteZeroes(..) => {
                    #[cfg(feature = "panicless-mode")]
                    if self.checks_enabled && self.panicless_tx.is_some() {
                        let _ = self.panicless_tx.take().unwrap().send(MockOutcome {
                            outcome: Err(MockOutcomeError::ShutdownInsteadOfWrite),
                            total_read_bytes: self.read_bytes,
                            total_written_bytes: self.written_bytes,
                        });
                        self.checks_enabled = false;
                        return Err(io::ErrorKind::InvalidData.into());
                    }

                    if checks_enabled {
                        panic!(
                            "Unexpected shutdown (there are more pending write actions) name={} r={} w={} remaining actions: {}",
                            self.name,
                            self.read_bytes,
                            self.written_bytes,
                            n_remaining_actions - i
                        );
                    }
                }
                Action::WriteShutdown(ref mut observed) => {
                    *observed = true;
                    return Ok(());
                }
                Action::StopChecking => checks_enabled = false,
                Action::Wait(..) => {
                    break;
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn remaining_wait(&mut self) -> Option<Duration> {
        match self.action() {
            Some(&mut Action::Wait(dur)) => Some(dur),
            _ => None,
        }
    }

    fn action(&mut self) -> Option<&mut Action> {
        loop {
            if self.actions.is_empty() {
                return None;
            }

            match self.actions[0] {
                Action::Read(ref mut data) => {
                    if !data.is_empty() {
                        break;
                    }
                }
                Action::ReadZeroes(n) => {
                    if n > 0 {
                        break;
                    }
                }
                Action::ReadEof(ref observed) => {
                    if !observed {
                        break;
                    }
                }
                Action::Write(ref mut data) => {
                    if !data.is_empty() {
                        break;
                    }
                }
                Action::WriteZeroes(n) => {
                    if n > 0 {
                        break;
                    }
                }
                Action::IgnoreWritten(n) => {
                    if n > 0 {
                        break;
                    }
                }
                Action::Wait(ref mut dur) => {
                    if let Some(until) = self.waiting {
                        let now = Instant::now();

                        if now < until {
                            break;
                        } else {
                            self.waiting = None;
                        }
                    } else {
                        self.waiting = Some(Instant::now() + *dur);
                        break;
                    }
                }
                Action::ReadError(ref mut error) | Action::WriteError(ref mut error) => {
                    if error.is_some() {
                        break;
                    }
                }
                Action::WriteShutdown(ref observed) => {
                    if !*observed {
                        break;
                    }
                }
                Action::StopChecking => {
                    self.checks_enabled = false;
                    break;
                }
            }

            let _action = self.actions.pop_front();
        }

        self.actions.front_mut()
    }
}

// ===== impl Inner =====

impl Mock {
    fn maybe_wakeup_reader(&mut self) {
        match self.inner.action() {
            Some(&mut Action::Read(_)) | Some(&mut Action::ReadError(_)) | None => {
                if let Some(waker) = self.inner.read_wait.take() {
                    waker.wake();
                }
            }
            _ => {}
        }
    }
}

impl AsyncRead for Mock {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if let Some(ref mut sleep) = self.inner.sleep {
                ready!(Pin::new(sleep).poll(cx));
            }

            // If a sleep is set, it has already fired
            self.inner.sleep = None;

            // Capture 'filled' to monitor if it changed
            let filled = buf.filled().len();

            match self.inner.read(buf) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(rem) = self.inner.remaining_wait() {
                        let until = Instant::now() + rem;
                        self.inner.sleep = Some(Box::pin(time::sleep_until(until)));
                    } else {
                        self.inner.read_wait = Some(cx.waker().clone());
                        return Poll::Pending;
                    }
                }
                Ok(()) => {
                    if buf.filled().len() == filled {
                        match ready!(self.inner.poll_action(cx)) {
                            Some(action) => {
                                self.inner.actions.push_back(action);
                                continue;
                            }
                            None => {
                                return Poll::Ready(Ok(()));
                            }
                        }
                    } else {
                        return Poll::Ready(Ok(()));
                    }
                }
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }
}

impl AsyncWrite for Mock {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            if let Some(ref mut sleep) = self.inner.sleep {
                ready!(Pin::new(sleep).poll(cx));
            }

            // If a sleep is set, it has already fired
            self.inner.sleep = None;

            if self.inner.actions.is_empty() {
                match self.inner.poll_action(cx) {
                    Poll::Pending => {
                        // do not propagate pending
                    }
                    Poll::Ready(Some(action)) => {
                        self.inner.actions.push_back(action);
                    }
                    Poll::Ready(None) => {
                        #[cfg(feature = "panicless-mode")]
                        if self.inner.checks_enabled && self.inner.panicless_tx.is_some() {
                            let _ = self.inner.panicless_tx.take().unwrap().send(MockOutcome {
                                outcome: Err(MockOutcomeError::UnexpectedWrite),
                                total_read_bytes: self.inner.read_bytes,
                                total_written_bytes: self.inner.written_bytes,
                            });
                            self.inner.checks_enabled = false;
                        }

                        if self.inner.checks_enabled {
                            panic!("unexpected write {}", self.pmsg());
                        } else {
                            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
                        }
                    }
                }
            }

            match self.inner.write(buf) {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(rem) = self.inner.remaining_wait() {
                        let until = Instant::now() + rem;
                        self.inner.sleep = Some(Box::pin(time::sleep_until(until)));
                    } else {
                        #[cfg(feature = "panicless-mode")]
                        if self.inner.checks_enabled && self.inner.panicless_tx.is_some() {
                            let _ = self.inner.panicless_tx.take().unwrap().send(MockOutcome {
                                outcome: Err(MockOutcomeError::Other),
                                total_read_bytes: self.inner.read_bytes,
                                total_written_bytes: self.inner.written_bytes,
                            });
                            self.inner.checks_enabled = false;
                        }

                        if self.inner.checks_enabled {
                            panic!("unexpected WouldBlock {}", self.pmsg());
                        } else {
                            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
                        }
                    }
                }
                Ok(0) => {
                    // TODO: Is this correct?
                    if self.inner.action().is_some() {
                        return Poll::Pending;
                    }

                    // TODO: Extract
                    match ready!(self.inner.poll_action(cx)) {
                        Some(action) => {
                            self.inner.actions.push_back(action);
                            continue;
                        }
                        None => {
                            #[cfg(feature = "panicless-mode")]
                            if self.inner.checks_enabled && self.inner.panicless_tx.is_some() {
                                let _ = self.inner.panicless_tx.take().unwrap().send(MockOutcome {
                                    outcome: Err(MockOutcomeError::UnexpectedWrite),
                                    total_read_bytes: self.inner.read_bytes,
                                    total_written_bytes: self.inner.written_bytes,
                                });
                                self.inner.checks_enabled = false;
                            }

                            if self.inner.checks_enabled {
                                panic!("unexpected write {}", self.pmsg());
                            } else {
                                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
                            }
                        }
                    }
                }
                ret => {
                    self.maybe_wakeup_reader();
                    return Poll::Ready(ret);
                }
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<io::Result<()>> {
        if !self.inner.shutdown_checking_enabled {
            return Poll::Ready(Ok(()));
        }
        loop {
            if let Some(ref mut sleep) = self.inner.sleep {
                ready!(Pin::new(sleep).poll(cx));
            }

            // If a sleep is set, it has already fired
            self.inner.sleep = None;

            if self.inner.actions.is_empty() {
                match self.inner.poll_action(cx) {
                    Poll::Pending => {
                        // do not propagate pending
                    }
                    Poll::Ready(Some(action)) => {
                        self.inner.actions.push_back(action);
                    }
                    Poll::Ready(None) => {
                        // OK to omit shutdown at the end
                        return Poll::Ready(Ok(()));
                    }
                }
            }

            match self.inner.shutdown() {
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(rem) = self.inner.remaining_wait() {
                        let until = Instant::now() + rem;
                        self.inner.sleep = Some(Box::pin(time::sleep_until(until)));
                    } else {
                        #[cfg(feature = "panicless-mode")]
                        if self.inner.checks_enabled && self.inner.panicless_tx.is_some() {
                            let _ = self.inner.panicless_tx.take().unwrap().send(MockOutcome {
                                outcome: Err(MockOutcomeError::Other),
                                total_read_bytes: self.inner.read_bytes,
                                total_written_bytes: self.inner.written_bytes,
                            });
                            self.inner.checks_enabled = false;
                        }

                        if self.inner.checks_enabled {
                            panic!("unexpected WouldBlock {}", self.pmsg());
                        } else {
                            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
                        }
                    }
                }
                ret => {
                    self.maybe_wakeup_reader();
                    return Poll::Ready(ret);
                }
            }
        }
    }
}

/// Ensures that Mock isn't dropped with data "inside".
impl Drop for Mock {
    fn drop(&mut self) {
        #[cfg(feature = "panicless-mode")]
        if let Some(tx) = self.inner.panicless_tx.take() {
            let mut outcome = Ok(());

            if self.inner.checks_enabled {
                self.inner.actions.iter().for_each(|a| match a {
                    Action::Read(data) if !data.is_empty() => {
                        outcome = Err(MockOutcomeError::RemainingUnreadData)
                    }
                    Action::ReadZeroes(nbytes) if *nbytes > 0 => {
                        outcome = Err(MockOutcomeError::RemainingUnreadData)
                    }
                    Action::ReadEof(observed) if !observed => {
                        outcome = Err(MockOutcomeError::RemainingUnreadData)
                    }
                    Action::Write(data) if !data.is_empty() => {
                        outcome = Err(MockOutcomeError::RemainingUnwrittenData)
                    }
                    Action::WriteZeroes(nbytes) if *nbytes > 0 => {
                        outcome = Err(MockOutcomeError::RemainingUnwrittenData)
                    }
                    Action::IgnoreWritten(nbytes) if *nbytes > 0 => {
                        outcome = Err(MockOutcomeError::RemainingUnwrittenData)
                    }
                    _ => (),
                });
            }

            let _ = tx.send(MockOutcome {
                outcome,
                total_read_bytes: self.inner.read_bytes,
                total_written_bytes: self.inner.written_bytes,
            });
            return;
        }

        // Avoid double panicking, since makes debugging much harder.
        if std::thread::panicking() {
            return;
        }

        if !self.inner.checks_enabled {
            return;
        }

        self.inner.actions.iter().for_each(|a| match a {
            Action::Read(data) => assert!(
                data.is_empty(),
                "There is still data left to read. {}",
                self.pmsg()
            ),
            Action::ReadZeroes(nbytes) => assert!(
                *nbytes == 0,
                "There is still data left to read. {}",
                self.pmsg()
            ),
            Action::ReadEof(observed) => assert!(
                observed,
                "There is still a read EOF event that was not observed {}",
                self.pmsg()
            ),
            Action::Write(data) => assert!(
                data.is_empty(),
                "There is still data left to write. {}",
                self.pmsg()
            ),
            Action::WriteZeroes(nbytes) => assert!(
                *nbytes == 0,
                "There is still data left to write. {}",
                self.pmsg()
            ),
            Action::IgnoreWritten(nbytes) => assert!(
                *nbytes == 0,
                "There is still data left to write (even though content is to be ignored). {}",
                self.pmsg()
            ),
            _ => (),
        });
    }
}
/*
/// Returns `true` if called from the context of a futures-rs Task
fn is_task_ctx() -> bool {
    use std::panic;

    // Save the existing panic hook
    let h = panic::take_hook();

    // Install a new one that does nothing
    panic::set_hook(Box::new(|_| {}));

    // Attempt to call the fn
    let r = panic::catch_unwind(|| task::current()).is_ok();

    // Re-install the old one
    panic::set_hook(h);

    // Return the result
    r
}
*/

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.name.is_empty() {
            write!(f, "Inner {{...}}")
        } else {
            write!(f, "Inner {{name={}, ...}}", self.name)
        }
    }
}

struct PanicMsgSnippet<'a>(&'a Inner);

impl<'a> fmt::Display for PanicMsgSnippet<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.name.is_empty() {
            write!(
                f,
                "({} actions remain, {} bytes was read, {} bytes was written)",
                self.0.actions.len(),
                self.0.read_bytes,
                self.0.written_bytes,
            )
        } else {
            write!(
                f,
                "(name {}, {} actions remain, {} bytes was read, {} bytes was written)",
                self.0.name,
                self.0.actions.len(),
                self.0.read_bytes,
                self.0.written_bytes,
            )
        }
    }
}

impl Mock {
    fn pmsg(&self) -> PanicMsgSnippet<'_> {
        PanicMsgSnippet(&self.inner)
    }
}
