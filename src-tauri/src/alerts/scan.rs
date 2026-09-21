//! The one parser that turns raw PTY bytes into attention signals.
//!
//! Agent CLIs do not have a private channel to their terminal: everything they
//! can say about "I need you" travels in the same byte stream as their drawing.
//! Four conventions carry it, and which one arrives depends on what the CLI
//! decided the terminal supports:
//!
//! * `BEL` (`0x07`) — Claude Code's `terminal_bell` channel and Codex's `bel`
//!   notification method, which is also Codex's fallback when its OSC 9 probe
//!   fails.
//! * `OSC 9` — `ESC ] 9 ; <message> ST`, iTerm2's notification command. Claude
//!   Code's `iterm2` channel and Codex's `osc9` method both emit it.
//! * `OSC 777` — `ESC ] 777 ; notify ; <title> ; <body> ST`, the rxvt
//!   convention that Ghostty and kitty also accept.
//! * `OSC 99` — kitty's desktop notification protocol, which Claude Code's
//!   `kitty` channel emits. Chunked and optionally base64.
//!
//! Three properties matter more than breadth of coverage here:
//!
//! 1. **A read boundary is not a token boundary.** A PTY read splits an escape
//!    sequence wherever it likes, so this is a resumable state machine over the
//!    whole session rather than a search over one buffer.
//! 2. **A payload that did not fit is not a payload.** Every accumulator is
//!    bounded, and an overflowed one is *discarded* at its terminator rather
//!    than delivered short — a notification body cut in half is a lie about
//!    what the agent said.
//! 3. **A `BEL` is only a bell in the ground state.** `BEL` is also the legal
//!    terminator of an OSC string, so a parser that scanned for the byte alone
//!    would ring once for every window title an agent sets.
//!
//! The conspicuous non-signal is `OSC 9 ; 4` — ConEmu's progress report, which
//! Windows Terminal and Claude Code's own progress bar both emit, many times a
//! second, for the whole length of a task. It shares an identifier with the
//! notification form and it is the single loudest thing this module must not
//! mistake for a message.

/// What a CLI asked the terminal to tell the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    /// Which convention carried it. Reported so the UI and the logs can say
    /// what the agent actually did, not what we inferred it meant.
    pub channel: Channel,
    /// The heading the CLI supplied, when it supplied one.
    pub title: Option<String>,
    /// The message the CLI supplied, when it supplied one. A bare `BEL`
    /// carries neither, and that absence is deliberately not filled in here.
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Bell,
    Osc9,
    Osc777,
    Osc99,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bell => "bell",
            Self::Osc9 => "osc9",
            Self::Osc777 => "osc777",
            Self::Osc99 => "osc99",
        }
    }
}

/// Largest OSC payload assembled before the sequence is abandoned.
///
/// Four kibibytes is far past any real notification and far short of a memory
/// problem. Past it the scanner keeps tracking the terminator — it must, or
/// the rest of the stream would be parsed as if it were still inside the
/// string — but drops what it has and emits nothing.
const MAX_OSC_BYTES: usize = 4096;

/// Largest identifier accepted before an OSC is treated as malformed.
const MAX_IDENT_BYTES: usize = 8;

/// Largest assembled kitty notification, across all of its chunks.
const MAX_CHUNKED_BYTES: usize = 4096;

/// How many chunks one kitty notification may be split into.
const MAX_CHUNKS: usize = 64;

/// How many part-assembled kitty notifications are tracked at once. A CLI that
/// starts notifications and never finishes them cannot grow this map.
const MAX_PENDING_CHUNKED: usize = 4;

/// Longest text this module will hand to an OS banner, in characters.
const MAX_TEXT_CHARS: usize = 240;

/// Signals returned from a single [`Scanner::feed`].
///
/// A read of nothing but `BEL` would otherwise produce thousands of signals
/// for a hub that is going to coalesce them into one anyway. The overflow is
/// counted rather than dropped silently.
const MAX_SIGNALS_PER_FEED: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    /// Saw `ESC` in the ground state.
    Escape,
    /// Inside `CSI …`, consuming until a final byte.
    Csi,
    /// Inside an OSC identifier, before the first `;`.
    OscIdent,
    /// Inside an OSC payload, after the first `;`.
    OscBody,
    /// Inside a DCS/SOS/PM/APC string, which is consumed and discarded.
    Ignored,
    /// Saw `ESC` inside a string. `\` terminates it; anything else was a
    /// literal ESC and the string named by `resume` continues.
    StringEscape,
}

#[derive(Debug, Clone)]
struct Chunked {
    id: String,
    title: Vec<u8>,
    body: Vec<u8>,
    chunks: usize,
    /// True once any part overflowed. A chunked notification that lost a
    /// middle chunk is discarded whole rather than delivered with a hole.
    spoiled: bool,
}

/// One session's resumable view of its own output stream.
#[derive(Debug)]
pub struct Scanner {
    state: State,
    /// The string state a `StringEscape` returns to. Kept explicitly because
    /// "which string am I in" is not recoverable from the accumulators, and
    /// guessing it is how an ignored DCS payload becomes a notification.
    resume: State,
    ident: Vec<u8>,
    payload: Vec<u8>,
    overflowed: bool,
    pending: Vec<Chunked>,
    /// Signals this scanner produced but did not return, for the caller to
    /// report rather than discover from a count that does not add up.
    dropped: u64,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            resume: State::Ground,
            ident: Vec::new(),
            payload: Vec::new(),
            overflowed: false,
            pending: Vec::new(),
            dropped: 0,
        }
    }

    /// Signals this scanner produced but could not return within a feed's cap.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Consumes one PTY read and returns whatever completed inside it.
    ///
    /// Bytes that complete nothing leave state behind for the next call; that
    /// is the whole point of the type.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Signal> {
        let mut out = Vec::new();
        for &byte in bytes {
            if let Some(signal) = self.step(byte) {
                if out.len() < MAX_SIGNALS_PER_FEED {
                    out.push(signal);
                } else {
                    self.dropped += 1;
                }
            }
        }
        out
    }

    fn step(&mut self, byte: u8) -> Option<Signal> {
        match self.state {
            State::Ground => match byte {
                0x07 => {
                    return Some(Signal {
                        channel: Channel::Bell,
                        title: None,
                        body: None,
                    })
                }
                0x1b => self.state = State::Escape,
                _ => {}
            },
            State::Escape => match byte {
                b']' => {
                    self.state = State::OscIdent;
                    self.ident.clear();
                    self.payload.clear();
                    self.overflowed = false;
                }
                b'[' => self.state = State::Csi,
                // DCS, SOS, PM, APC: string sequences whose payload may contain
                // anything, including bytes that would otherwise read as a bell.
                b'P' | b'X' | b'^' | b'_' => self.state = State::Ignored,
                // A second ESC restarts the introducer rather than dropping it.
                0x1b => {}
                _ => self.state = State::Ground,
            },
            State::Csi => {
                // Parameter and intermediate bytes are 0x20-0x3F; a final byte
                // in 0x40-0x7E ends the sequence.
                if (0x40..=0x7e).contains(&byte) {
                    self.state = State::Ground;
                }
            }
            State::OscIdent => match byte {
                b';' => self.state = State::OscBody,
                0x07 => {
                    self.state = State::Ground;
                    return self.complete();
                }
                0x1b => {
                    self.resume = State::OscIdent;
                    self.state = State::StringEscape;
                }
                _ => {
                    if self.ident.len() < MAX_IDENT_BYTES {
                        self.ident.push(byte);
                    } else {
                        self.overflowed = true;
                    }
                }
            },
            State::OscBody => match byte {
                0x07 => {
                    self.state = State::Ground;
                    return self.complete();
                }
                0x1b => {
                    self.resume = State::OscBody;
                    self.state = State::StringEscape;
                }
                _ => {
                    if self.payload.len() < MAX_OSC_BYTES {
                        self.payload.push(byte);
                    } else {
                        self.overflowed = true;
                    }
                }
            },
            State::Ignored => match byte {
                0x07 => self.state = State::Ground,
                0x1b => {
                    self.resume = State::Ignored;
                    self.state = State::StringEscape;
                }
                _ => {}
            },
            State::StringEscape => {
                if byte == b'\\' {
                    // ST. Whether it completes anything depends on which
                    // string it closed, which `resume` is the only record of.
                    let osc = matches!(self.resume, State::OscIdent | State::OscBody);
                    self.state = State::Ground;
                    self.resume = State::Ground;
                    if osc {
                        return self.complete();
                    }
                    return None;
                }
                // A literal ESC inside the string. Put it back where it came
                // from; the string has not ended.
                match self.resume {
                    State::OscBody => {
                        self.state = State::OscBody;
                        if self.payload.len() + 2 <= MAX_OSC_BYTES {
                            self.payload.push(0x1b);
                            self.payload.push(byte);
                        } else {
                            self.overflowed = true;
                        }
                    }
                    State::OscIdent => {
                        // An identifier is digits. An ESC in the middle of one
                        // means we no longer know what this sequence is, so it
                        // is marked unreadable rather than parsed as if the
                        // ESC had been a separator.
                        self.state = State::OscBody;
                        self.overflowed = true;
                    }
                    _ => self.state = State::Ignored,
                }
                self.resume = State::Ground;
            }
        }
        None
    }

    fn complete(&mut self) -> Option<Signal> {
        let ident = std::mem::take(&mut self.ident);
        let payload = std::mem::take(&mut self.payload);
        let overflowed = std::mem::replace(&mut self.overflowed, false);
        if overflowed {
            // The sequence was longer, or less well-formed, than we are willing
            // to hold. We tracked its terminator so the stream stays in sync,
            // but we do not know what it said and will not guess.
            return None;
        }
        match ident.as_slice() {
            b"9" => osc9(&payload),
            b"777" => osc777(&payload),
            b"99" => self.osc99(&payload),
            _ => None,
        }
    }

    /// kitty's protocol, reduced to the part that carries a notification.
    ///
    /// `ESC ] 99 ; <metadata> ; <payload> ST`, where metadata is a
    /// colon-separated `key=value` list. The keys that change what the payload
    /// means are `p` (`title` or `body`), `e` (`1` for base64), `d` (`0` when
    /// more chunks follow) and `i` (the identity chunks are joined by).
    fn osc99(&mut self, payload: &[u8]) -> Option<Signal> {
        let (meta, data) = match payload.iter().position(|&b| b == b';') {
            Some(index) => (&payload[..index], &payload[index + 1..]),
            None => (payload, &[][..]),
        };
        let meta = std::str::from_utf8(meta).ok()?;
        let mut id = String::new();
        let mut part = "title";
        let mut base64 = false;
        let mut done = true;
        for field in meta.split(':') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key {
                "i" if value.len() <= 64 => id = value.to_owned(),
                "p" => part = value,
                "e" => base64 = value == "1",
                "d" => done = value != "0",
                _ => {}
            }
        }
        if !matches!(part, "title" | "body") {
            // `p=alive`, `p=close` and the query forms are protocol traffic,
            // not text. Ignoring them is not the same as failing to parse.
            return None;
        }
        let decoded = if base64 {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .ok()?
        } else {
            data.to_vec()
        };

        let slot = match self.pending.iter().position(|c| c.id == id) {
            Some(index) => index,
            None => {
                if done {
                    // The common case: one self-contained chunk. It never
                    // touches the pending table at all.
                    let text = text_of(&decoded)?;
                    return Some(if part == "body" {
                        Signal {
                            channel: Channel::Osc99,
                            title: None,
                            body: Some(text),
                        }
                    } else {
                        Signal {
                            channel: Channel::Osc99,
                            title: Some(text),
                            body: None,
                        }
                    });
                }
                if self.pending.len() >= MAX_PENDING_CHUNKED {
                    // Refuse the new identity rather than evicting an old one:
                    // evicting would deliver a notification missing a chunk.
                    return None;
                }
                self.pending.push(Chunked {
                    id: id.clone(),
                    title: Vec::new(),
                    body: Vec::new(),
                    chunks: 0,
                    spoiled: false,
                });
                self.pending.len() - 1
            }
        };

        {
            let entry = &mut self.pending[slot];
            entry.chunks += 1;
            if entry.chunks > MAX_CHUNKS {
                entry.spoiled = true;
            }
            let target = if part == "body" {
                &mut entry.body
            } else {
                &mut entry.title
            };
            if target.len() + decoded.len() > MAX_CHUNKED_BYTES {
                entry.spoiled = true;
            } else {
                target.extend_from_slice(&decoded);
            }
        }
        if !done {
            return None;
        }
        let entry = self.pending.remove(slot);
        if entry.spoiled {
            return None;
        }
        let title = text_of(&entry.title);
        let body = text_of(&entry.body);
        if title.is_none() && body.is_none() {
            return None;
        }
        Some(Signal {
            channel: Channel::Osc99,
            title,
            body,
        })
    }
}

/// iTerm2's `OSC 9`, minus the identifier it shares with ConEmu's control set.
///
/// `9;4;…` is a progress report, emitted continuously by Windows Terminal and
/// by Claude Code's own progress bar; `9;9;…` reports the working directory.
/// Both would otherwise read as notification text. Every other payload is the
/// message form, including one that merely starts with a digit.
fn osc9(payload: &[u8]) -> Option<Signal> {
    let text = text_of(payload)?;
    let control = text
        .split_once(';')
        .map(|(head, _)| head)
        .unwrap_or(text.as_str());
    if matches!(control, "4" | "9") {
        return None;
    }
    Some(Signal {
        channel: Channel::Osc9,
        title: None,
        body: Some(text),
    })
}

/// `OSC 777 ; notify ; <title> ; <body>`.
///
/// `777` also carries other verbs in rxvt's scheme, so the `notify` keyword is
/// required rather than assumed.
fn osc777(payload: &[u8]) -> Option<Signal> {
    let text = std::str::from_utf8(payload).ok()?;
    let rest = text.strip_prefix("notify;")?;
    let (title, body) = match rest.split_once(';') {
        Some((title, body)) => (title, Some(body)),
        None => (rest, None),
    };
    let title = sanitize(title);
    let body = body.map(sanitize).filter(|b| !b.is_empty());
    if title.is_empty() && body.is_none() {
        return None;
    }
    Some(Signal {
        channel: Channel::Osc777,
        title: (!title.is_empty()).then_some(title),
        body,
    })
}

fn text_of(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let text = sanitize(std::str::from_utf8(bytes).ok()?);
    (!text.is_empty()).then_some(text)
}

/// Strips control characters and collapses runs of whitespace.
///
/// What reaches this point is attacker-adjacent by construction: any program
/// the user runs can write it. A notification body is rendered by the OS, so
/// the bytes that matter are the ones that could move a cursor, start another
/// escape sequence, or turn one line into forty.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(512));
    let mut chars = 0usize;
    let mut space = false;
    for ch in text.chars() {
        if ch.is_control() || ch.is_whitespace() || ch == '\u{2028}' || ch == '\u{2029}' {
            space = true;
            continue;
        }
        if space && chars > 0 {
            out.push(' ');
            chars += 1;
        }
        space = false;
        if chars >= MAX_TEXT_CHARS {
            out.push('…');
            break;
        }
        out.push(ch);
        chars += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(input: &[u8]) -> Vec<Signal> {
        Scanner::new().feed(input)
    }

    fn bodies(signals: &[Signal]) -> Vec<Option<&str>> {
        signals.iter().map(|s| s.body.as_deref()).collect()
    }

    fn bell() -> Signal {
        Signal {
            channel: Channel::Bell,
            title: None,
            body: None,
        }
    }

    #[test]
    fn a_bare_bell_is_the_signal_every_cli_can_send() {
        assert_eq!(scan(b"working\x07done"), vec![bell()]);
    }

    #[test]
    fn a_bell_that_terminates_an_osc_title_is_not_a_bell() {
        // This is the whole reason the parser has states. `ESC ] 0 ; … BEL` is
        // how a shell sets the window title, several times per prompt.
        assert_eq!(scan(b"\x1b]0;~/code\x07$ "), vec![]);
        assert_eq!(scan(b"\x1b]2;claude\x07"), vec![]);
    }

    #[test]
    fn a_bell_inside_a_device_control_string_is_not_a_bell() {
        assert_eq!(scan(b"\x1bP1$r0m\x1b\\after"), vec![]);
        assert_eq!(scan(b"\x1b_Ga=T\x07tail"), vec![]);
        // An APC payload containing a literal ESC that is not ST stays inside
        // the string, so its later BEL still terminates rather than rings.
        assert_eq!(scan(b"\x1b_G\x1bXpayload\x07"), vec![]);
    }

    #[test]
    fn osc9_carries_the_message_an_iterm2_style_cli_sends() {
        assert_eq!(
            bodies(&scan(b"\x1b]9;Claude needs your permission\x07")),
            vec![Some("Claude needs your permission")]
        );
        // ST rather than BEL terminates it just as legally.
        assert_eq!(
            bodies(&scan(b"\x1b]9;Turn complete\x1b\\")),
            vec![Some("Turn complete")]
        );
    }

    #[test]
    fn osc9_progress_reports_are_never_notifications() {
        // Claude Code's progress bar and Windows Terminal both emit 9;4 many
        // times a second. One notification per update would be unusable.
        assert_eq!(scan(b"\x1b]9;4;1;40\x07"), vec![]);
        assert_eq!(scan(b"\x1b]9;4;0;0\x07"), vec![]);
        assert_eq!(scan(b"\x1b]9;9;/Users/me/code\x07"), vec![]);
        // A message that merely begins with a digit is still a message.
        assert_eq!(
            bodies(&scan(b"\x1b]9;3 files need review\x07")),
            vec![Some("3 files need review")]
        );
        assert_eq!(
            bodies(&scan(b"\x1b]9;42\x07")),
            vec![Some("42")],
            "a bare number is a message, not a control word"
        );
    }

    #[test]
    fn osc777_needs_its_verb_and_splits_title_from_body() {
        let signals = scan(b"\x1b]777;notify;Codex;Turn complete\x1b\\");
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].title.as_deref(), Some("Codex"));
        assert_eq!(signals[0].body.as_deref(), Some("Turn complete"));
        // A different 777 verb is not a notification.
        assert_eq!(scan(b"\x1b]777;precmd\x07"), vec![]);
    }

    #[test]
    fn osc99_accepts_one_shot_base64_and_plain_chunks() {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode("Needs input");
        let sequence = format!("\x1b]99;i=1:p=title:e=1;{encoded}\x1b\\");
        let signals = scan(sequence.as_bytes());
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].title.as_deref(), Some("Needs input"));
        assert_eq!(signals[0].channel, Channel::Osc99);

        let mut scanner = Scanner::new();
        assert_eq!(
            scanner.feed(b"\x1b]99;i=a:d=0:p=title;Claude\x1b\\"),
            vec![]
        );
        let done = scanner.feed(b"\x1b]99;i=a:d=1:p=body; is waiting\x1b\\");
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].title.as_deref(), Some("Claude"));
        assert_eq!(done[0].body.as_deref(), Some("is waiting"));
    }

    #[test]
    fn osc99_protocol_traffic_is_not_text() {
        assert_eq!(scan(b"\x1b]99;i=1:p=alive;1\x1b\\"), vec![]);
        assert_eq!(scan(b"\x1b]99;i=1:p=close;\x1b\\"), vec![]);
        // Undecodable base64 is dropped rather than shown as raw base64.
        assert_eq!(
            scan(b"\x1b]99;i=1:e=1:p=title;!!!!not base64\x1b\\"),
            vec![]
        );
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_one_sequence() {
        let whole = b"\x1b]9;Permission needed for Bash\x07";
        for cut in 1..whole.len() {
            let mut scanner = Scanner::new();
            let mut signals = scanner.feed(&whole[..cut]);
            signals.extend(scanner.feed(&whole[cut..]));
            assert_eq!(
                bodies(&signals),
                vec![Some("Permission needed for Bash")],
                "split at {cut} lost the sequence"
            );
        }
    }

    #[test]
    fn every_channel_survives_being_split_one_byte_at_a_time() {
        for whole in [
            &b"\x1b]777;notify;Codex;Done\x1b\\"[..],
            &b"\x1b]99;i=7:p=title;Claude\x1b\\"[..],
            &b"\x1b]0;title\x07\x07"[..],
        ] {
            let mut every = Scanner::new();
            let mut byte_at_a_time = Vec::new();
            for &byte in whole {
                byte_at_a_time.extend(every.feed(&[byte]));
            }
            assert_eq!(
                byte_at_a_time,
                scan(whole),
                "byte-at-a-time disagreed with one read"
            );
        }
    }

    #[test]
    fn an_oversized_payload_is_discarded_not_truncated() {
        let mut sequence = b"\x1b]9;".to_vec();
        sequence.extend_from_slice(&vec![b'x'; MAX_OSC_BYTES * 2]);
        sequence.push(0x07);
        sequence.push(0x07);
        // The giant OSC produced nothing, and the bell *after* it still
        // registered: the parser stayed in sync through the overflow.
        assert_eq!(scan(&sequence), vec![bell()]);
    }

    #[test]
    fn an_unterminated_sequence_cannot_grow_without_bound() {
        let mut scanner = Scanner::new();
        for _ in 0..512 {
            scanner.feed(&[b'x'; 1024]);
        }
        scanner.feed(b"\x1b]9;");
        for _ in 0..512 {
            scanner.feed(&[b'y'; 1024]);
        }
        assert!(scanner.payload.len() <= MAX_OSC_BYTES);
        assert!(scanner.overflowed);
    }

    #[test]
    fn unfinished_chunked_notifications_cannot_accumulate() {
        let mut scanner = Scanner::new();
        for id in 0..64 {
            scanner.feed(format!("\x1b]99;i={id}:d=0:p=title;x\x1b\\").as_bytes());
        }
        assert!(scanner.pending.len() <= MAX_PENDING_CHUNKED);
        let held: usize = scanner
            .pending
            .iter()
            .map(|c| c.title.len() + c.body.len())
            .sum();
        assert!(held <= MAX_PENDING_CHUNKED * MAX_CHUNKED_BYTES);
    }

    #[test]
    fn a_chunked_notification_that_overflowed_is_dropped_whole() {
        let mut scanner = Scanner::new();
        let filler = "y".repeat(2048);
        for _ in 0..4 {
            scanner.feed(format!("\x1b]99;i=z:d=0:p=body;{filler}\x1b\\").as_bytes());
        }
        assert_eq!(scanner.feed(b"\x1b]99;i=z:d=1:p=body;tail\x1b\\"), vec![]);
        assert!(scanner.pending.is_empty(), "spoiled entry was not released");
    }

    #[test]
    fn a_body_cannot_smuggle_control_bytes_into_the_os_banner() {
        let signals = scan(b"\x1b]777;notify;A\tB;line\x1b[31mone\x0c\x1b\\");
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].title.as_deref(), Some("A B"));
        let body = signals[0].body.as_deref().unwrap();
        assert!(!body.contains('\x1b') && !body.contains('\x0c'), "{body}");
    }

    #[test]
    fn sanitize_bounds_the_length_it_returns() {
        let long = "a".repeat(10_000);
        assert!(sanitize(&long).chars().count() <= MAX_TEXT_CHARS + 1);
        let spaced = "a b ".repeat(10_000);
        assert!(sanitize(&spaced).chars().count() <= MAX_TEXT_CHARS + 1);
    }

    #[test]
    fn csi_parameters_are_not_mistaken_for_ground_bytes() {
        // A truecolor SGR carries `;` and digits; none of it is an OSC.
        assert_eq!(scan(b"\x1b[38;2;255;0;0mred\x1b[0m"), vec![]);
    }

    #[test]
    fn signals_returned_per_read_are_capped_and_the_remainder_counted() {
        let mut scanner = Scanner::new();
        let signals = scanner.feed(&[0x07; 100]);
        assert_eq!(signals.len(), MAX_SIGNALS_PER_FEED);
        assert_eq!(scanner.dropped(), 100 - MAX_SIGNALS_PER_FEED as u64);
    }

    #[test]
    fn arbitrary_bytes_never_panic_and_never_retain_more_than_their_bounds() {
        // Not a fuzzer, but the shapes a fuzzer finds first: every byte value,
        // every introducer, in an order no CLI would produce.
        let mut scanner = Scanner::new();
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut chunk = Vec::with_capacity(4096);
        for _ in 0..256 {
            chunk.clear();
            for _ in 0..4096 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                chunk.push((seed >> 33) as u8);
            }
            scanner.feed(&chunk);
            assert!(scanner.payload.len() <= MAX_OSC_BYTES);
            assert!(scanner.ident.len() <= MAX_IDENT_BYTES);
            assert!(scanner.pending.len() <= MAX_PENDING_CHUNKED);
        }
    }
}
