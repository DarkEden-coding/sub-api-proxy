//! `helps.CodexSSEScanner` (7795de19): an upstream Responses event whose data spans several
//! `data:` lines is delivered as one `data:` line.
//!
//! Every event on this path is one JSON value. A data line that is complete JSON on its own,
//! or cannot start JSON (an empty value, `[DONE]`), is delivered as read. A data line that
//! opens an object or array without closing it starts an event that collects the following
//! data lines until their joined value closes its outermost object or array, a blank line
//! ends the event, the input ends, or the collected bytes would pass the line limit. A
//! completed event is delivered compacted onto one line; one that never becomes valid JSON
//! is delivered as its data lines joined with `\n`, so it still fails downstream. Other
//! lines are delivered the moment they are read, so a comment inside a collected event
//! comes before its data.

use std::borrow::Cow;

const DATA: &[u8] = b"data:";

/// Go's `codexSSEDiagnosticLines`: data lines one invalid-event warning describes.
const DIAGNOSTIC_LINES: usize = 8;

/// The scanner's state between physical lines.
pub(crate) struct DataLines {
    max: usize,
    /// The data lines of the event being collected, as read, joined with `\n`.
    held: Vec<u8>,
    /// Their values (the text after `data:` without one leading space), joined with `\n`.
    joined: Vec<u8>,
    count: usize,
    size: usize,
    nesting: Nesting,
    /// The stream already logged its invalid-event warning.
    warned: bool,
}

impl DataLines {
    /// `max` bounds the collected data lines of one event, as Go's line limit does.
    pub(crate) fn new(max: usize) -> Self {
        Self {
            max,
            held: Vec::new(),
            joined: Vec::new(),
            count: 0,
            size: 0,
            nesting: Nesting::default(),
            warned: false,
        }
    }

    /// Data lines held for an unfinished event, and their bytes.
    pub(crate) fn holding(&self) -> (usize, usize) {
        (self.count, self.size)
    }

    /// One physical line, its terminator and a trailing CR removed. Appends the lines it
    /// completes to `out`, in the order Go's `Scan` delivers them.
    pub(crate) fn push<'a>(&mut self, line: &'a [u8], out: &mut Vec<Cow<'a, [u8]>>) {
        if !line.starts_with(DATA) {
            if line.is_empty() && self.count > 0 {
                self.release(out);
            }
            out.push(Cow::Borrowed(line));
            return;
        }
        if self.count == 0 {
            if !incomplete_json(line[DATA.len()..].trim_ascii()) {
                out.push(Cow::Borrowed(line));
                return;
            }
        } else if self.size + line.len() > self.max {
            // Bounded like a single line: stop collecting and deliver this line on its own.
            self.release(out);
            out.push(Cow::Borrowed(line));
            return;
        }
        if self.hold(line) {
            self.release(out);
        }
    }

    /// The input ended: an event still being collected is delivered.
    pub(crate) fn finish(&mut self, out: &mut Vec<Cow<'_, [u8]>>) {
        if self.count > 0 {
            self.release(out);
        }
    }

    /// Adds a data line and reports whether the event's data closed with it.
    fn hold(&mut self, line: &[u8]) -> bool {
        let value = line[DATA.len()..].strip_prefix(b" ").unwrap_or(&line[DATA.len()..]);
        if self.count > 0 {
            self.held.push(b'\n');
            self.joined.push(b'\n');
            self.nesting.byte(b'\n');
        }
        self.held.extend_from_slice(line);
        self.joined.extend_from_slice(value);
        self.count += 1;
        self.size += line.len();
        self.nesting.feed(value)
    }

    fn release(&mut self, out: &mut Vec<Cow<'_, [u8]>>) {
        let held = std::mem::take(&mut self.held);
        let joined = std::mem::take(&mut self.joined);
        let lines = self.count;
        (self.count, self.size, self.nesting) = (0, 0, Nesting::default());
        if lines > 1 && cpa_common::json::std_valid(&joined) {
            let mut line = Vec::with_capacity(DATA.len() + 1 + joined.len());
            line.extend_from_slice(b"data: ");
            line.extend_from_slice(&cpa_common::json::compact(&joined, false));
            out.push(Cow::Owned(line));
            return;
        }
        let data = if lines == 1 { joined.trim_ascii() } else { &joined[..] };
        self.diagnose(&held, lines, data);
        out.push(Cow::Owned(held));
    }

    /// One bounded warning for the stream's first event whose data is not valid JSON. It
    /// describes the framing and never logs data bytes.
    // ponytail: Go also logs `json_error_offset`, the encoding/json syntax error offset;
    // std_valid reports no offset. Port an offset-reporting validator if the field matters.
    fn diagnose(&mut self, held: &[u8], lines: usize, data: &[u8]) {
        if self.warned || data.is_empty() || data == b"[DONE]" || cpa_common::json::std_valid(data) {
            return;
        }
        self.warned = true;
        let shown = lines.min(DIAGNOSTIC_LINES);
        let (mut lengths, mut valid) = (Vec::with_capacity(shown), Vec::with_capacity(shown));
        for line in held.split(|b| *b == b'\n').take(shown) {
            let value = line[DATA.len()..].trim_ascii();
            lengths.push(value.len().to_string());
            valid.push(cpa_common::json::std_valid(value).to_string());
        }
        let more = if lines > shown {
            format!(",+{}", lines - shown)
        } else {
            String::new()
        };
        tracing::warn!(
            data_lines = lines,
            line_bytes = format!("{}{more}", lengths.join(",")),
            lines_valid = format!("{}{more}", valid.join(",")),
            data_bytes = data.len(),
            "codex executor: upstream SSE event data is not valid JSON"
        );
    }
}

/// `codexSSEIncompleteJSON`: the value starts an object or array it does not complete.
fn incomplete_json(value: &[u8]) -> bool {
    !(value.is_empty() || !matches!(value[0], b'{' | b'[') || value == b"[DONE]" || closed(value))
}

/// `codexSSEClosed`: only a value ending in a closing bracket can be complete JSON.
fn closed(value: &[u8]) -> bool {
    let trimmed = value.trim_ascii_end();
    matches!(trimmed.last(), Some(b'}' | b']')) && cpa_common::json::std_valid(value)
}

/// `codexJSONNesting`: the nesting of a JSON text fed in pieces, so a collected event is
/// validated once, when its outermost object or array closes.
#[derive(Default)]
struct Nesting {
    depth: i64,
    in_string: bool,
    escaped: bool,
}

impl Nesting {
    /// Consumes `p` and reports whether the outermost object or array closed within it.
    fn feed(&mut self, p: &[u8]) -> bool {
        p.iter().any(|&c| self.byte(c))
    }

    fn byte(&mut self, c: u8) -> bool {
        if self.in_string {
            if self.escaped {
                self.escaped = false;
            } else if c == b'\\' {
                self.escaped = true;
            } else if c == b'"' {
                self.in_string = false;
            }
            return false;
        }
        match c {
            b'"' => self.in_string = true,
            b'{' | b'[' => self.depth += 1,
            b'}' | b']' => {
                self.depth -= 1;
                return self.depth <= 0;
            }
            _ => {}
        }
        false
    }
}

#[cfg(test)]
#[path = "codex_sse_tests.rs"]
mod tests;
