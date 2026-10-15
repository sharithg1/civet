//! Conservative scanner that splits PTY output into chunks which cannot push
//! more lines into alacritty's (fixed-size) scrollback than it holds, and
//! that ends a chunk right before any sequence that would clear or hide
//! alacritty's scrollback (RIS, ED, DECSET such as `?1049h`). After each
//! chunk the terminal moves alacritty's history into the
//! [`crate::ScrollbackStore`], so no line is ever lost or duplicated.
//!
//! The bound is counted in *cells*: a line feed costs `cols`, a printable
//! byte costs 1 (a UTF-8 continuation byte overcounts, which is fine), a
//! sequence that can scroll the whole screen costs `rows * cols`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum St {
    #[default]
    Ground,
    Esc,
    Csi,
    /// OSC / DCS / SOS / PM / APC payload.
    Str,
    /// ESC seen inside a string (maybe ST).
    StrEsc,
}

#[derive(Debug, Default)]
pub(crate) struct ScrollScanner {
    st: St,
    /// First CSI parameter (for REP).
    param0: u32,
    /// Index of the CSI parameter being parsed.
    param_idx: u32,
}

/// What processing one byte implies.
struct Effect {
    /// The chunk should end before this byte (history must be drained
    /// first), if the chunk can have pushed anything.
    split_before: bool,
    /// Upper bound of cells this byte can scroll into history.
    cost: u64,
}

impl ScrollScanner {
    /// Length of the next chunk of `bytes` (> 0 for non-empty input).
    /// `limit` is the bound in cells (`max_lines * cols`).
    pub(crate) fn next_chunk(&mut self, bytes: &[u8], cols: u64, rows: u64, limit: u64) -> usize {
        let mut bound: u64 = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let eff = self.effect(b, cols, rows);
            if i > 0 && bound > 0 && (eff.split_before || bound + eff.cost > limit) {
                return i;
            }
            self.consume(b);
            bound += eff.cost;
        }
        bytes.len()
    }

    fn effect(&self, b: u8, cols: u64, rows: u64) -> Effect {
        let none = Effect {
            split_before: false,
            cost: 0,
        };
        let screen = rows * cols;
        if matches!(b, 0x0a..=0x0c) && self.st != St::Str {
            // C0 controls execute in every state but string payloads.
            return Effect {
                split_before: false,
                cost: cols,
            };
        }
        match self.st {
            St::Ground => match b {
                0x20..=0x7e | 0x80..=0xff => Effect {
                    split_before: false,
                    cost: 1,
                },
                _ => none,
            },
            St::Esc | St::StrEsc => match b {
                b'D' | b'E' => Effect {
                    split_before: false,
                    cost: cols,
                },
                b'c' => Effect {
                    split_before: true,
                    cost: 0,
                },
                _ => none,
            },
            St::Csi => match b {
                b'J' | b'h' => Effect {
                    split_before: true,
                    cost: screen,
                },
                b'S' | b'M' => Effect {
                    split_before: false,
                    cost: screen,
                },
                b'b' => Effect {
                    split_before: false,
                    cost: u64::from(self.param0.max(1)) + cols,
                },
                _ => none,
            },
            St::Str => none,
        }
    }

    fn consume(&mut self, b: u8) {
        self.st = match self.st {
            St::Ground => {
                if b == 0x1b {
                    St::Esc
                } else {
                    St::Ground
                }
            }
            St::Esc | St::StrEsc => {
                if self.st == St::StrEsc && b == b'\\' {
                    St::Ground
                } else {
                    match b {
                        b'[' => {
                            self.param0 = 0;
                            self.param_idx = 0;
                            St::Csi
                        }
                        b']' | b'P' | b'X' | b'^' | b'_' => St::Str,
                        0x1b => St::Esc,
                        0x18 | 0x1a => St::Ground,
                        0x20..=0x2f | 0x00..=0x17 | 0x19 | 0x1c..=0x1f => St::Esc,
                        _ => St::Ground,
                    }
                }
            }
            St::Csi => match b {
                b'0'..=b'9' => {
                    if self.param_idx == 0 {
                        self.param0 = (self.param0 * 10 + u32::from(b - b'0')).min(65_535);
                    }
                    St::Csi
                }
                b';' | b':' => {
                    self.param_idx += 1;
                    St::Csi
                }
                0x40..=0x7e => St::Ground,
                0x1b => St::Esc,
                0x18 | 0x1a => St::Ground,
                _ => St::Csi,
            },
            St::Str => match b {
                0x07 | 0x18 | 0x1a => St::Ground,
                0x1b => St::StrEsc,
                _ => St::Str,
            },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(input: &[u8], cols: u64, rows: u64, limit: u64) -> Vec<Vec<u8>> {
        let mut s = ScrollScanner::default();
        let mut rest = input;
        let mut out = Vec::new();
        while !rest.is_empty() {
            let n = s.next_chunk(rest, cols, rows, limit);
            assert!(n > 0);
            out.push(rest[..n].to_vec());
            rest = &rest[n..];
        }
        out
    }

    #[test]
    fn splits_before_ris_and_ed() {
        let c = chunks(b"a\nb\x1bc\nx\x1b[3J", 80, 24, 1 << 30);
        assert_eq!(
            c,
            vec![b"a\nb\x1b".to_vec(), b"c\nx\x1b[3".to_vec(), b"J".to_vec()]
        );
    }

    #[test]
    fn no_split_when_nothing_pushed() {
        let c = chunks(b"\x1b[?25l\x1b[H\x1b[2J", 80, 24, 1 << 30);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn line_budget() {
        let input = b"\n".repeat(100);
        let c = chunks(&input, 10, 5, 10 * 10);
        assert!(c.iter().all(|c| c.len() <= 10));
        assert_eq!(c.iter().map(Vec::len).sum::<usize>(), 100);
    }

    #[test]
    fn osc_payload_is_not_counted() {
        let mut input = b"\x1b]2;".to_vec();
        input.extend(b"J".repeat(50));
        input.extend(b"\x07");
        let c = chunks(&input, 10, 5, 1 << 30);
        assert_eq!(c.len(), 1);
    }
}
