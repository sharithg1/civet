//! Binary channel records for `attach_pane` → `onOutput`
//! (docs/INTERFACES.md §6.2, mirrored by `ui/src/terminal/records.ts`).
//!
//! Each channel message holds one or more records back to back; all integers
//! are little-endian:
//!
//! ```text
//! offset size field
//! 0      1    kind      1 = screen, 2 = input-ack, 3 = pane-closed, 4 = predict
//! 1      1    flags     bit0 FULL, bit1 RESIZED, bit2 HISTORY_GAP
//! 2      2    cols
//! 4      2    rows
//! 6      2    reserved (0)
//! 8      8    state
//! 16     8    ackedInputSeq
//! 24     8    historyStart
//! 32     8    historyLines
//! 40     4    payloadLen
//! 44     n    payload
//! ```

/// Fixed header size of every record.
pub const HEADER_LEN: usize = 44;

/// Record kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RecordKind {
    /// Server screen update (possibly wrapped in prediction undo/redo bytes).
    Screen = 1,
    /// Highest input seq the daemon applied (`ackedInputSeq`), no payload.
    InputAck = 2,
    /// The pane is gone, no payload.
    PaneClosed = 3,
    /// Local-echo prediction overlay on top of `state`.
    Predict = 4,
}

impl RecordKind {
    /// Wire value.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Screen,
            2 => Self::InputAck,
            3 => Self::PaneClosed,
            4 => Self::Predict,
            _ => return None,
        })
    }
}

/// `flags` bits.
pub mod flags {
    /// `UpdateKind::Full`.
    pub const FULL: u8 = 1 << 0;
    /// cols/rows differ from the previous record on this channel.
    pub const RESIZED: u8 = 1 << 1;
    /// `historyStart` is past the previous `historyLines`: the lines in
    /// between are only available via `fetch_scrollback`.
    pub const HISTORY_GAP: u8 = 1 << 2;
}

/// Everything in a record except the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    /// Record kind.
    pub kind: RecordKind,
    /// [`flags`] bits.
    pub flags: u8,
    /// Terminal columns.
    pub cols: u16,
    /// Terminal rows.
    pub rows: u16,
    /// Server state number (for predictions: the state they overlay).
    pub state: u64,
    /// Highest input seq the daemon applied.
    pub acked_input_seq: u64,
    /// First absolute history line replayed by the payload.
    pub history_start: u64,
    /// Absolute history line count after this record.
    pub history_lines: u64,
}

impl RecordHeader {
    /// Header of the given kind with every other field zero.
    pub fn new(kind: RecordKind) -> Self {
        Self {
            kind,
            flags: 0,
            cols: 0,
            rows: 0,
            state: 0,
            acked_input_seq: 0,
            history_start: 0,
            history_lines: 0,
        }
    }
}

/// Append one record to `out`.
///
/// # Panics
/// If the payload is 4 GiB or larger (cannot happen: frames are capped at
/// 16 MiB by civet-proto).
pub fn encode_into(out: &mut Vec<u8>, h: &RecordHeader, payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("record payload exceeds u32");
    out.reserve(HEADER_LEN + payload.len());
    out.push(h.kind as u8);
    out.push(h.flags);
    out.extend_from_slice(&h.cols.to_le_bytes());
    out.extend_from_slice(&h.rows.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&h.state.to_le_bytes());
    out.extend_from_slice(&h.acked_input_seq.to_le_bytes());
    out.extend_from_slice(&h.history_start.to_le_bytes());
    out.extend_from_slice(&h.history_lines.to_le_bytes());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
}

/// One record as a fresh buffer.
pub fn encode(h: &RecordHeader, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    encode_into(&mut out, h, payload);
    out
}

/// Why a channel message could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// Fewer than [`HEADER_LEN`] bytes left at `offset`.
    #[error("truncated record header at offset {0}")]
    TruncatedHeader(usize),
    /// The payload runs past the end of the message.
    #[error("truncated record payload at offset {0}")]
    TruncatedPayload(usize),
    /// Unknown kind byte.
    #[error("unknown record kind {kind} at offset {offset}")]
    UnknownKind {
        /// Kind byte.
        kind: u8,
        /// Record offset.
        offset: usize,
    },
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"))
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().expect("8 bytes"))
}

/// Parse every record in one channel message (the inverse of
/// [`encode_into`]; the UI has its own parser, this one serves tests).
pub fn decode_all(msg: &[u8]) -> Result<Vec<(RecordHeader, &[u8])>, DecodeError> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < msg.len() {
        if msg.len() - off < HEADER_LEN {
            return Err(DecodeError::TruncatedHeader(off));
        }
        let kind = RecordKind::from_u8(msg[off]).ok_or(DecodeError::UnknownKind {
            kind: msg[off],
            offset: off,
        })?;
        let len = u32_at(msg, off + 40) as usize;
        let start = off + HEADER_LEN;
        let end = start
            .checked_add(len)
            .filter(|e| *e <= msg.len())
            .ok_or(DecodeError::TruncatedPayload(off))?;
        out.push((
            RecordHeader {
                kind,
                flags: msg[off + 1],
                cols: u16_at(msg, off + 2),
                rows: u16_at(msg, off + 4),
                state: u64_at(msg, off + 8),
                acked_input_seq: u64_at(msg, off + 16),
                history_start: u64_at(msg, off + 24),
                history_lines: u64_at(msg, off + 32),
            },
            &msg[start..end],
        ));
        off = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared with `ui/src/terminal/records.fixture.test.ts`: both sides must
    /// produce/parse exactly these bytes.
    const FIXTURE: &str = include_str!("../ui/src/terminal/records.fixture.json");

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FixtureRecord {
        kind: u8,
        flags: u8,
        cols: u16,
        rows: u16,
        state: u64,
        acked_input_seq: u64,
        history_start: u64,
        history_lines: u64,
        payload: String,
    }

    #[derive(serde::Deserialize)]
    struct Fixture {
        records: Vec<FixtureRecord>,
        hex: String,
    }

    fn hex_of(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn header_layout_is_pinned() {
        let h = RecordHeader {
            kind: RecordKind::Screen,
            flags: flags::FULL | flags::RESIZED,
            cols: 132,
            rows: 43,
            state: (1 << 32) + 7,
            acked_input_seq: 5,
            history_start: 1000,
            history_lines: 2000,
        };
        let b = encode(&h, b"hi");
        assert_eq!(b.len(), HEADER_LEN + 2);
        assert_eq!(b[0], 1);
        assert_eq!(b[1], 0b011);
        assert_eq!(&b[2..4], &132u16.to_le_bytes());
        assert_eq!(&b[4..6], &43u16.to_le_bytes());
        assert_eq!(&b[6..8], &[0, 0]);
        assert_eq!(&b[8..12], &7u32.to_le_bytes());
        assert_eq!(&b[12..16], &1u32.to_le_bytes());
        assert_eq!(&b[16..24], &5u64.to_le_bytes());
        assert_eq!(&b[24..32], &1000u64.to_le_bytes());
        assert_eq!(&b[32..40], &2000u64.to_le_bytes());
        assert_eq!(&b[40..44], &2u32.to_le_bytes());
        assert_eq!(&b[44..], b"hi");
        let parsed = decode_all(&b).unwrap();
        assert_eq!(parsed, vec![(h, &b"hi"[..])]);
    }

    #[test]
    fn matches_shared_fixture() {
        let fx: Fixture = serde_json::from_str(FIXTURE).unwrap();
        let mut out = Vec::new();
        for r in &fx.records {
            let h = RecordHeader {
                kind: RecordKind::from_u8(r.kind).unwrap(),
                flags: r.flags,
                cols: r.cols,
                rows: r.rows,
                state: r.state,
                acked_input_seq: r.acked_input_seq,
                history_start: r.history_start,
                history_lines: r.history_lines,
            };
            encode_into(&mut out, &h, r.payload.as_bytes());
        }
        assert_eq!(hex_of(&out), fx.hex);
        let parsed = decode_all(&out).unwrap();
        assert_eq!(parsed.len(), fx.records.len());
        for ((h, p), r) in parsed.iter().zip(&fx.records) {
            assert_eq!(h.kind as u8, r.kind);
            assert_eq!(h.state, r.state);
            assert_eq!(*p, r.payload.as_bytes());
        }
    }

    #[test]
    fn decode_rejects_truncation_and_unknown_kinds() {
        let b = encode(&RecordHeader::new(RecordKind::InputAck), b"");
        assert_eq!(decode_all(&b[..10]), Err(DecodeError::TruncatedHeader(0)));
        let b = encode(&RecordHeader::new(RecordKind::Screen), b"hello");
        assert_eq!(
            decode_all(&b[..HEADER_LEN + 2]),
            Err(DecodeError::TruncatedPayload(0))
        );
        let mut b = encode(&RecordHeader::new(RecordKind::Screen), b"");
        b[0] = 9;
        assert!(matches!(
            decode_all(&b),
            Err(DecodeError::UnknownKind { kind: 9, .. })
        ));
        assert!(decode_all(&[]).unwrap().is_empty());
    }
}
