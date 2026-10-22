//! yaml.v3 base64-decodes a scalar whose tag resolves to `!!binary` into any string
//! field it decodes (decode.go `scalar`); serde_yaml_ng reads the base64 text. A
//! scalar with a local tag (`!keep x`, `! x`) is its text in yaml.v3 (`resolve`
//! leaves unknown tags unresolved); serde_yaml_ng wraps it as a tagged value. The
//! tags come from libyaml's own events, so handles, `%TAG` directives and `%XX`
//! escapes resolve as they do in yaml.v3, whose `shortTag` also keeps a literal
//! `!!binary` tag (`!<!!binary>`, `!%21binary`).

use std::collections::HashMap;
use std::ffi::CStr;
use std::mem::MaybeUninit;

use anyhow::{Context, bail, ensure};
use serde_yaml_ng::Value;

/// Go's `base64.StdEncoding` as yaml.v3 applies it: padding required, unused bits
/// ignored, CR and LF skipped. `None` for invalid data.
pub(super) fn go_base64(text: &[u8]) -> Option<Vec<u8>> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    use base64::{Engine as _, alphabet};
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
    );
    let payload: Vec<u8> = text.iter().copied().filter(|b| !matches!(b, b'\r' | b'\n')).collect();
    engine.decode(payload).ok()
}

/// Replaces each `!!binary` scalar in `value`, which serde_yaml_ng parsed from `text`
/// with merges not yet expanded, by its decoded text, and each locally tagged scalar
/// by its text, aliases of them included. Fails on
/// invalid base64 as Go does, and refuses what a string cannot hold: bytes that are
/// not UTF-8, and a binary mapping key.
pub(super) fn decode(text: &str, value: &mut Value) -> anyhow::Result<()> {
    // No tag, no binary scalar.
    if !text.contains('!') {
        return Ok(());
    }
    let mut walk = Walk {
        events: events(text)?.into_iter(),
        anchors: HashMap::new(),
        found: Vec::new(),
    };
    if let Some(root) = walk.events.next() {
        walk.node(root, &mut Vec::new(), false)?;
    }
    for (path, decoded) in walk.found {
        let mut node = &mut *value;
        for &i in &path {
            while let Value::Tagged(tagged) = node {
                node = &mut tagged.value;
            }
            node = match node {
                Value::Mapping(m) => m.values_mut().nth(i),
                Value::Sequence(items) => items.get_mut(i),
                _ => None,
            }
            .context("config YAML changed while it was read")?;
        }
        *node = Value::String(decoded);
    }
    Ok(())
}

#[derive(PartialEq)]
enum Tag {
    Binary,
    /// A local tag, which serde_yaml_ng keeps as `Value::Tagged`.
    Local,
    Other,
}

/// The document's node events, in order, without stream and document markers.
enum Event {
    Scalar {
        anchor: Option<Vec<u8>>,
        tag: Tag,
        value: Vec<u8>,
    },
    Alias(Vec<u8>),
    Start {
        anchor: Option<Vec<u8>>,
        mapping: bool,
    },
    End,
}

fn events(text: &str) -> anyhow::Result<Vec<Event>> {
    struct Parser(Box<MaybeUninit<unsafe_libyaml::yaml_parser_t>>);
    impl Drop for Parser {
        fn drop(&mut self) {
            // SAFETY: only built after a successful yaml_parser_initialize.
            unsafe { unsafe_libyaml::yaml_parser_delete(self.0.as_mut_ptr()) }
        }
    }
    fn bytes(ptr: *const u8) -> Option<Vec<u8>> {
        // SAFETY: libyaml's anchors and tags are NUL-terminated or null.
        (!ptr.is_null()).then(|| unsafe { CStr::from_ptr(ptr.cast()) }.to_bytes().to_vec())
    }
    let mut out = Vec::new();
    // SAFETY: the parser is boxed, so it does not move while libyaml holds pointers
    // into it; `text` outlives it; each event is deleted once its data is copied.
    unsafe {
        let mut raw = Box::new(MaybeUninit::<unsafe_libyaml::yaml_parser_t>::uninit());
        ensure!(
            !unsafe_libyaml::yaml_parser_initialize(raw.as_mut_ptr()).fail,
            "failed to start the YAML parser"
        );
        let mut parser = Parser(raw);
        let p = parser.0.as_mut_ptr();
        unsafe_libyaml::yaml_parser_set_encoding(p, unsafe_libyaml::YAML_UTF8_ENCODING);
        unsafe_libyaml::yaml_parser_set_input_string(p, text.as_ptr(), text.len() as _);
        loop {
            let mut event = MaybeUninit::<unsafe_libyaml::yaml_event_t>::uninit();
            // serde_yaml_ng parsed the same text, so this does not fail.
            ensure!(
                !unsafe_libyaml::yaml_parser_parse(p, event.as_mut_ptr()).fail,
                "failed to parse config file"
            );
            let event = event.assume_init_mut();
            let kind = event.type_;
            let next = match kind {
                unsafe_libyaml::YAML_SCALAR_EVENT => {
                    let s = &event.data.scalar;
                    let tag = match bytes(s.tag) {
                        Some(t) if t == b"tag:yaml.org,2002:binary" || t == b"!!binary" => Tag::Binary,
                        Some(t) if t.starts_with(b"!") => Tag::Local,
                        _ => Tag::Other,
                    };
                    Some(Event::Scalar {
                        anchor: bytes(s.anchor),
                        tag,
                        value: std::slice::from_raw_parts(s.value, s.length as usize).to_vec(),
                    })
                }
                unsafe_libyaml::YAML_ALIAS_EVENT => bytes(event.data.alias.anchor).map(Event::Alias),
                unsafe_libyaml::YAML_SEQUENCE_START_EVENT => Some(Event::Start {
                    anchor: bytes(event.data.sequence_start.anchor),
                    mapping: false,
                }),
                unsafe_libyaml::YAML_MAPPING_START_EVENT => Some(Event::Start {
                    anchor: bytes(event.data.mapping_start.anchor),
                    mapping: true,
                }),
                unsafe_libyaml::YAML_SEQUENCE_END_EVENT | unsafe_libyaml::YAML_MAPPING_END_EVENT => Some(Event::End),
                _ => None,
            };
            unsafe_libyaml::yaml_event_delete(event);
            out.extend(next);
            if kind == unsafe_libyaml::YAML_STREAM_END_EVENT {
                return Ok(out);
            }
        }
    }
}

struct Walk {
    events: std::vec::IntoIter<Event>,
    /// Each anchor's binary scalars, by their path below it.
    anchors: HashMap<Vec<u8>, Vec<(Vec<usize>, String)>>,
    /// Binary scalars by their path: entry or item positions from the root.
    found: Vec<(Vec<usize>, String)>,
}

impl Walk {
    fn next(&mut self) -> anyhow::Result<Event> {
        self.events.next().context("config YAML ended early")
    }

    fn node(&mut self, event: Event, path: &mut Vec<usize>, key: bool) -> anyhow::Result<()> {
        let start = self.found.len();
        let anchor = match event {
            Event::Scalar { anchor, tag, value } => {
                if tag == Tag::Local && !key {
                    // ponytail: a locally tagged key stays tagged; it names no setting.
                    let text = String::from_utf8(value).ok().context("config YAML is not UTF-8")?;
                    self.found.push((path.clone(), text));
                } else if tag == Tag::Binary {
                    ensure!(!key, "a !!binary mapping key in the config file is not supported");
                    let bytes = go_base64(&value).context("yaml: !!binary value contains invalid base64 data")?;
                    let text = String::from_utf8(bytes)
                        .ok()
                        .context("a !!binary value in the config file is not UTF-8 text")?;
                    self.found.push((path.clone(), text));
                }
                anchor
            }
            Event::Alias(name) => {
                let copies = self.anchors.get(&name).cloned().unwrap_or_default();
                ensure!(
                    !key || copies.is_empty(),
                    "a !!binary mapping key in the config file is not supported"
                );
                for (below, text) in copies {
                    self.found.push(([path.as_slice(), &below].concat(), text));
                }
                None
            }
            Event::Start { anchor, mapping } => {
                let mut i = 0;
                loop {
                    let event = self.next()?;
                    if matches!(event, Event::End) {
                        break;
                    }
                    if mapping {
                        self.node(event, path, true)?;
                        let value = self.next()?;
                        path.push(i);
                        self.node(value, path, key)?;
                    } else {
                        path.push(i);
                        self.node(event, path, key)?;
                    }
                    path.pop();
                    i += 1;
                }
                anchor
            }
            Event::End => bail!("config YAML is malformed"),
        };
        if let Some(name) = anchor {
            let below = self.found[start..]
                .iter()
                .map(|(p, text)| (p[path.len()..].to_vec(), text.clone()))
                .collect();
            self.anchors.insert(name, below);
        }
        Ok(())
    }
}
