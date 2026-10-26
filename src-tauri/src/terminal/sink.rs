//! Where one session's output goes.
//!
//! Output used to be a global `terminal-output` event: every chunk base64
//! encoded, wrapped in JSON, spliced into a generated script and evaluated in
//! every webview, whichever tab it was for. A session now owns an
//! [`OutputSink`], and a view that shows it hands over a per-session
//! `tauri::ipc::Channel` of raw bytes when it spawns or takes the session over
//! (`cmd_terminal_spawn`, `cmd_terminal_attach`, `cmd_workbench_launch_terminal`).
//!
//! What a channel does with a message is the framework's (`ipc/channel.rs`):
//! a message of 1024 bytes or more is queued and fetched by the page, so it
//! costs one small script plus one fetch and carries no base64; a shorter one
//! is evaluated as a JSON array of numbers. Messages are numbered, and the page
//! delivers them in order whatever order their fetches finish in.
//!
//! The sink is replaceable (a page that was reloaded, or a task terminal taken
//! over by the next view) and the reader always sends to the current one.
use super::TerminalOutputPayload;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use std::sync::{Arc, Mutex, PoisonError};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter};

/// A view's channel for one session's output: raw bytes, then one marker.
pub type OutputChannel = Channel<InvokeResponseBody>;

/// The message a channel carries after a session's last byte, before its exit
/// event. A page delivers channel messages in order, but an exit is an event
/// and overtakes bytes still being fetched; the marker tells the page every
/// byte has arrived, so it can hold the exit until then.
pub const END_OF_OUTPUT: &str = "\"end\"";

type SendFn = dyn Fn(&str, Frame<'_>) -> Result<(), String> + Send + Sync;

enum Frame<'a> {
    Bytes(&'a [u8]),
    End,
}

/// Where a session's reader sends what the program printed.
#[derive(Clone)]
pub struct OutputSink {
    send: Arc<SendFn>,
}

impl OutputSink {
    /// The page's channel, which is how a view receives output.
    pub fn channel(channel: OutputChannel) -> Self {
        Self {
            send: Arc::new(move |_, frame| {
                let body = match frame {
                    Frame::Bytes(data) => InvokeResponseBody::Raw(data.to_vec()),
                    Frame::End => InvokeResponseBody::Json(END_OF_OUTPUT.into()),
                };
                channel.send(body).map_err(|error| error.to_string())
            }),
        }
    }

    /// Re-emits each chunk as a `terminal-output` event
    /// `{id, data_b64, bytes}`, for a listener in this process: the tests and
    /// benches that count what a session sends. No view is attached through
    /// this; the end marker is not an event.
    #[doc(hidden)]
    pub fn events<R: tauri::Runtime>(app: &AppHandle<R>) -> Self {
        let app = app.clone();
        Self {
            send: Arc::new(move |id, frame| match frame {
                Frame::Bytes(data) => app
                    .emit(
                        "terminal-output",
                        TerminalOutputPayload {
                            id: id.to_owned(),
                            data_b64: BASE64_STANDARD.encode(data),
                            bytes: data.len(),
                        },
                    )
                    .map_err(|error| error.to_string()),
                Frame::End => Ok(()),
            }),
        }
    }

    /// A sink over any transport: `send(session, Some(bytes))` for output and
    /// `send(session, None)` for the end marker. The terminal host's sink,
    /// which frames output to whichever app is connected.
    #[cfg(unix)]
    pub(crate) fn from_fn(
        send: impl Fn(&str, Option<&[u8]>) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            send: Arc::new(move |id, frame| match frame {
                Frame::Bytes(data) => send(id, Some(data)),
                Frame::End => send(id, None),
            }),
        }
    }

    /// Delivers `data`, one message. `Err` is the transport's: the page is gone
    /// or its event loop refused, which the reader answers by shedding.
    pub(super) fn bytes(&self, session_id: &str, data: &[u8]) -> Result<(), String> {
        (self.send)(session_id, Frame::Bytes(data))
    }

    /// Says the session will send nothing more.
    pub(super) fn end(&self, session_id: &str) -> Result<(), String> {
        (self.send)(session_id, Frame::End)
    }
}

/// A session's current sink.
pub(super) struct OutputSlot(Mutex<OutputSink>);

impl OutputSlot {
    pub(super) fn new(sink: OutputSink) -> Self {
        Self(Mutex::new(sink))
    }

    /// The sink to send to now. Cloned out so a send never runs under the lock.
    pub(super) fn current(&self) -> OutputSink {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A view takes the session over: later output goes to `sink`.
    pub(super) fn replace(&self, sink: OutputSink) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = sink;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sink over a real `Channel` whose messages land in a list.
    fn recording() -> (OutputSink, Arc<Mutex<Vec<InvokeResponseBody>>>) {
        let log: Arc<Mutex<Vec<InvokeResponseBody>>> = Arc::default();
        let into = log.clone();
        let sink = OutputSink::channel(Channel::new(move |body| {
            into.lock().unwrap().push(body);
            Ok(())
        }));
        (sink, log)
    }

    #[test]
    fn a_channel_carries_bytes_raw_and_the_end_as_one_marker() {
        let (sink, log) = recording();
        sink.bytes("term-1", b"\x1b[31mred\xff").unwrap();
        sink.end("term-1").unwrap();
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2);
        match &log[0] {
            InvokeResponseBody::Raw(bytes) => assert_eq!(bytes, b"\x1b[31mred\xff"),
            other => panic!("bytes were not raw: {other:?}"),
        }
        match &log[1] {
            InvokeResponseBody::Json(json) => assert_eq!(json, END_OF_OUTPUT),
            other => panic!("the end was not the marker: {other:?}"),
        }
        assert_eq!(
            serde_json::from_str::<String>(END_OF_OUTPUT).unwrap(),
            "end"
        );
    }

    #[test]
    fn a_view_that_takes_over_receives_what_follows_and_the_old_one_nothing() {
        let (first, first_log) = recording();
        let (second, second_log) = recording();
        let slot = OutputSlot::new(first);
        slot.current().bytes("s", b"a").unwrap();
        slot.replace(second);
        slot.current().bytes("s", b"b").unwrap();
        assert_eq!(first_log.lock().unwrap().len(), 1);
        assert_eq!(second_log.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_failing_channel_reports_instead_of_pretending_to_have_delivered() {
        let sink = OutputSink::channel(Channel::new(|_| Err(tauri::Error::WebviewNotFound)));
        assert!(sink.bytes("s", b"x").is_err());
    }
}
