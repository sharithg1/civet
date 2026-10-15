//! Absolute-numbered, bounded history store.

use std::collections::VecDeque;

use civet_proto::{ScrollbackLine, ScrollbackPage, ScrollbackQuery, ScrollbackRequest};

#[derive(Debug, Clone)]
struct StoredLine {
    line: ScrollbackLine,
    /// Terminal width when the line scrolled off (0 = unknown). A soft-wrapped
    /// line can only be replayed without a newline when the client has the
    /// same width.
    cols: u16,
}

/// Absolute-numbered history lines, VT-encoded (compact: SGR changes only
/// where attributes change). Evicts oldest beyond `limit`.
///
/// Line numbers start at 0 for the first line that ever scrolled off and are
/// never reused, even after eviction.
#[derive(Debug, Clone)]
pub struct ScrollbackStore {
    lines: VecDeque<StoredLine>,
    /// Absolute number of `lines[0]`.
    first: u64,
    limit: usize,
}

impl ScrollbackStore {
    /// Empty store keeping at most `limit` lines.
    pub fn new(limit: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(limit.min(1024)),
            first: 0,
            limit,
        }
    }

    /// Maximum retained lines.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Append a line (width unknown).
    pub fn push(&mut self, line: ScrollbackLine) {
        self.push_with_cols(line, 0);
    }

    pub(crate) fn push_with_cols(&mut self, line: ScrollbackLine, cols: u16) {
        if self.limit == 0 {
            self.first += 1;
            return;
        }
        if self.lines.len() == self.limit {
            self.lines.pop_front();
            self.first += 1;
        }
        self.lines.push_back(StoredLine { line, cols });
    }

    /// Oldest retained absolute line number.
    pub fn oldest(&self) -> u64 {
        self.first
    }

    /// One past the newest (== history_lines).
    pub fn end(&self) -> u64 {
        self.first + self.lines.len() as u64
    }

    /// Number of retained lines.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// True if nothing is retained.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Line by absolute number, if retained.
    pub fn get(&self, line: u64) -> Option<&ScrollbackLine> {
        self.stored(line).map(|s| &s.line)
    }

    fn stored(&self, line: u64) -> Option<&StoredLine> {
        let idx = line.checked_sub(self.first)?;
        self.lines.get(usize::try_from(idx).ok()?)
    }

    /// Retained lines in `start..end` (clamped), with their absolute numbers.
    pub fn range(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, &ScrollbackLine)> {
        let s = start.clamp(self.first, self.end());
        let e = end.clamp(s, self.end());
        let a = (s - self.first) as usize;
        let b = (e - self.first) as usize;
        self.lines
            .range(a..b)
            .enumerate()
            .map(move |(i, l)| (s + i as u64, &l.line))
    }

    /// Oldest line `s` in `start..end` such that the encoded lines `s..end`
    /// (VT bytes plus `per_line` bytes of framing each) fit in `budget`.
    /// Returns `end` if not even the newest line fits.
    pub(crate) fn budget_start(&self, start: u64, end: u64, per_line: usize, budget: usize) -> u64 {
        let s = start.clamp(self.first, self.end());
        let e = end.clamp(s, self.end());
        let a = (s - self.first) as usize;
        let b = (e - self.first) as usize;
        let mut used = 0usize;
        let mut first = e;
        for l in self.lines.range(a..b).rev() {
            used = used.saturating_add(l.line.vt.len() + per_line);
            if used > budget {
                break;
            }
            first -= 1;
        }
        first
    }

    pub(crate) fn range_with_cols(
        &self,
        start: u64,
        end: u64,
    ) -> impl Iterator<Item = (&ScrollbackLine, u16)> {
        let s = start.clamp(self.first, self.end());
        let e = end.clamp(s, self.end());
        let a = (s - self.first) as usize;
        let b = (e - self.first) as usize;
        self.lines.range(a..b).map(|l| (&l.line, l.cols))
    }

    /// Serve a scrollback request (clamped to what is retained).
    ///
    /// * `Before { end, limit }`: up to `limit` lines immediately before `end`
    ///   (or before the newest line when `end` is None). If `end` is older
    ///   than everything retained the page is empty and starts at
    ///   `oldest_available`.
    /// * `Range { start, end }`: the retained part of `start..end`.
    pub fn page(&self, req: &ScrollbackRequest) -> ScrollbackPage {
        let (s, e) = match req.query {
            ScrollbackQuery::Before { end, limit } => {
                let e = end.unwrap_or(self.end()).clamp(self.first, self.end());
                let s = e.saturating_sub(u64::from(limit)).max(self.first);
                (s, e)
            }
            ScrollbackQuery::Range { start, end } => {
                let s = start.clamp(self.first, self.end());
                (s, end.clamp(s, self.end()))
            }
        };
        ScrollbackPage {
            req_id: req.req_id,
            pane: req.pane,
            start: s,
            lines: self.range(s, e).map(|(_, l)| l.clone()).collect(),
            oldest_available: self.first,
            end_available: self.end(),
        }
    }
}
