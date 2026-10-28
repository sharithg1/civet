//! List prices for estimating agent spend (`Request::AgentUsage`). The only
//! price table in Civet: the desktop shows whatever the daemon sends.
//!
//! These are **estimates**: public API list prices as of [`PRICES_AS_OF`],
//! per million tokens. They know nothing of subscriptions (Claude Max,
//! ChatGPT plans), discounts, batch or regional pricing. Models missing here
//! are counted in tokens but not priced. Update the table (and the date)
//! when prices change.

use civet_proto::ModelPrice;

/// When the table was last checked.
pub(crate) const PRICES_AS_OF: &str = "2026-10-27";

/// Dollars per million tokens → micro-dollars per million tokens.
const fn usd(d: f64) -> u64 {
    (d * 1_000_000.0 + 0.5) as u64
}

/// One model's prices, $/MTok.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Price {
    pub model: &'static str,
    pub input: f64,
    pub output: f64,
    /// Cache reads.
    pub cache_read: f64,
    /// Cache writes cost this much input, 5-minute lifetime (1.25x for
    /// Claude; OpenAI does not charge them).
    pub write_5m: f64,
    /// ... and with the 1-hour lifetime (2x for Claude).
    pub write_1h: f64,
    /// Every rate is multiplied by this in fast mode (Claude's
    /// `usage.speed: "fast"`); 1 where fast mode is not priced here.
    pub fast: u64,
    /// Prompts (input and cache tokens of one request) above this many
    /// tokens cost `long_mult` times as much, every rate.
    pub long_above: Option<u64>,
    pub long_mult: u64,
}

const fn claude(model: &'static str, input: f64, output: f64, cache_read: f64) -> Price {
    Price {
        model,
        input,
        output,
        cache_read,
        write_5m: 1.25,
        write_1h: 2.0,
        fast: 1,
        long_above: None,
        long_mult: 1,
    }
}

const fn openai(model: &'static str, input: f64, output: f64, cache_read: f64) -> Price {
    Price {
        model,
        input,
        output,
        cache_read,
        write_5m: 0.0,
        write_1h: 0.0,
        fast: 1,
        long_above: None,
        long_mult: 1,
    }
}

const fn with_fast(p: Price, fast: u64) -> Price {
    Price { fast, ..p }
}

const fn with_long(p: Price, above: u64, mult: u64) -> Price {
    Price {
        long_above: Some(above),
        long_mult: mult,
        ..p
    }
}

/// The table. Model ids are matched after [`base_model`].
pub(crate) const PRICES: &[Price] = &[
    // Anthropic (Claude Code).
    claude("claude-fable-5-1", 10.0, 50.0, 0.25),
    claude("claude-mythos-5-1", 10.0, 50.0, 0.25),
    claude("claude-fable-5", 10.0, 50.0, 1.0),
    claude("claude-mythos-5", 10.0, 50.0, 1.0),
    with_fast(claude("claude-opus-5-5", 4.0, 20.0, 0.20), 2),
    with_fast(claude("claude-opus-5", 5.0, 25.0, 0.50), 2),
    claude("claude-opus-4-8", 5.0, 25.0, 0.50),
    claude("claude-opus-4-7", 5.0, 25.0, 0.50),
    claude("claude-opus-4-6", 5.0, 25.0, 0.50),
    claude("claude-opus-4-5", 5.0, 25.0, 0.50),
    claude("claude-opus-4-1", 15.0, 75.0, 1.50),
    claude("claude-opus-4", 15.0, 75.0, 1.50),
    claude("claude-sonnet-5-5", 2.0, 10.0, 0.20),
    claude("claude-sonnet-5", 2.0, 10.0, 0.20),
    claude("claude-sonnet-4-6", 3.0, 15.0, 0.30),
    claude("claude-sonnet-4-5", 3.0, 15.0, 0.30),
    claude("claude-sonnet-4", 3.0, 15.0, 0.30),
    with_long(claude("claude-haiku-5-5", 0.10, 0.50, 0.01), 100_000, 5),
    claude("claude-haiku-4-5", 1.0, 5.0, 0.10),
    claude("claude-3-5-haiku", 0.80, 4.0, 0.08),
    // OpenAI (Codex).
    openai("gpt-5", 1.25, 10.0, 0.125),
    openai("gpt-5-codex", 1.25, 10.0, 0.125),
    openai("gpt-5.1", 1.25, 10.0, 0.125),
    openai("gpt-5.1-codex", 1.25, 10.0, 0.125),
    openai("gpt-5-mini", 0.25, 2.0, 0.025),
    openai("gpt-5.1-codex-mini", 0.25, 2.0, 0.025),
    openai("gpt-5-nano", 0.05, 0.40, 0.005),
];

/// The id without provider prefixes, version / date suffixes and context
/// tags: `us.anthropic.claude-sonnet-4-5-20250929-v1:0` and
/// `claude-sonnet-4-5@20250929` → `claude-sonnet-4-5`,
/// `claude-opus-4-6[1m]` → `claude-opus-4-6`.
pub(crate) fn base_model(model: &str) -> String {
    let mut m = model.trim().to_ascii_lowercase();
    if let Some(i) = m.rfind("anthropic.") {
        m = m[i + "anthropic.".len()..].to_string();
    }
    if let Some(i) = m.find('/') {
        // `openai/gpt-5`
        m = m[i + 1..].to_string();
    }
    for sep in ['[', '@'] {
        if let Some(i) = m.find(sep) {
            m.truncate(i);
        }
    }
    if let Some(i) = m.rfind("-v")
        && m[i + 2..].chars().all(|c| c.is_ascii_digit() || c == ':')
        && m.len() > i + 2
    {
        m.truncate(i);
    }
    // A trailing date: -20250929 or -2025-09-29.
    for len in [9usize, 11] {
        if m.len() > len && m.is_char_boundary(m.len() - len) {
            let (head, tail) = m.split_at(m.len() - len);
            let digits = tail[1..].chars().filter(char::is_ascii_digit).count();
            if tail.starts_with('-') && digits == 8 && tail[1..].starts_with("20") {
                m = head.to_string();
                break;
            }
        }
    }
    m
}

/// The row for `model`, if any.
pub(crate) fn price_of(model: &str) -> Option<&'static Price> {
    let base = base_model(model);
    PRICES.iter().find(|p| p.model == base)
}

/// What one request cost, in pico-dollars (micro-dollars per MTok times
/// tokens).
pub(crate) struct Request {
    pub input: u64,
    pub output: u64,
    pub write_5m: u64,
    pub write_1h: u64,
    pub cache_read: u64,
    pub fast: bool,
}

impl Price {
    pub(crate) fn cost_pico(&self, r: &Request) -> u64 {
        let mut mult = if r.fast { self.fast } else { 1 };
        let prompt = r.input + r.write_5m + r.write_1h + r.cache_read;
        if self.long_above.is_some_and(|a| prompt > a) {
            mult *= self.long_mult;
        }
        let rate = |per_mtok: f64, tokens: u64| usd(per_mtok).saturating_mul(tokens);
        (rate(self.input, r.input)
            + rate(self.output, r.output)
            + rate(self.input * self.write_5m, r.write_5m)
            + rate(self.input * self.write_1h, r.write_1h)
            + rate(self.cache_read, r.cache_read))
        .saturating_mul(mult)
    }

    /// The row as sent to clients (base rates, no fast or long-prompt
    /// multipliers).
    pub(crate) fn to_proto(self) -> ModelPrice {
        ModelPrice {
            model: self.model.to_string(),
            input: usd(self.input),
            output: usd(self.output),
            cache_write_5m: usd(self.input * self.write_5m),
            cache_write_1h: usd(self.input * self.write_1h),
            cache_read: usd(self.cache_read),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_model_strips_decorations() {
        for (raw, want) in [
            ("claude-opus-5-5", "claude-opus-5-5"),
            ("claude-sonnet-4-5-20250929", "claude-sonnet-4-5"),
            ("claude-sonnet-4-5@20250929", "claude-sonnet-4-5"),
            (
                "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
                "claude-sonnet-4-5",
            ),
            ("claude-opus-4-6[1m]", "claude-opus-4-6"),
            ("Claude-Haiku-4-5", "claude-haiku-4-5"),
            ("gpt-5-codex", "gpt-5-codex"),
            ("openai/gpt-5", "gpt-5"),
            ("gpt-5-2025-08-07", "gpt-5"),
            ("claude-opus-5", "claude-opus-5"),
        ] {
            assert_eq!(base_model(raw), want, "{raw}");
        }
    }

    #[test]
    fn lookup_needs_an_exact_base() {
        assert_eq!(price_of("claude-opus-5-5").unwrap().input, 4.0);
        assert_eq!(price_of("claude-opus-4-20250514").unwrap().input, 15.0);
        assert!(price_of("claude-opus-9").is_none());
        assert!(price_of("gpt-6-astra").is_none());
        assert!(price_of("<synthetic>").is_none());
    }

    #[test]
    fn costs() {
        let opus = price_of("claude-opus-5-5").unwrap();
        let r = Request {
            input: 1_000_000,
            output: 1_000_000,
            write_5m: 1_000_000,
            write_1h: 1_000_000,
            cache_read: 1_000_000,
            fast: false,
        };
        // $4 + $20 + $5 + $8 + $0.20, in pico-dollars.
        assert_eq!(opus.cost_pico(&r), 37_200_000 * 1_000_000);
        let fast = Request { fast: true, ..r };
        assert_eq!(opus.cost_pico(&fast), 2 * 37_200_000 * 1_000_000);

        let haiku = price_of("claude-haiku-5-5").unwrap();
        let small = Request {
            input: 1000,
            output: 1000,
            write_5m: 0,
            write_1h: 0,
            cache_read: 0,
            fast: false,
        };
        assert_eq!(haiku.cost_pico(&small), (100_000 + 500_000) * 1000);
        let long = Request {
            cache_read: 200_000,
            ..small
        };
        assert_eq!(
            haiku.cost_pico(&long),
            5 * ((100_000 + 500_000) * 1000 + 10_000 * 200_000)
        );
    }

    #[test]
    fn every_row_is_unique_and_sane() {
        for (i, p) in PRICES.iter().enumerate() {
            assert_eq!(base_model(p.model), p.model, "{}", p.model);
            assert!(
                PRICES[..i].iter().all(|q| q.model != p.model),
                "{}",
                p.model
            );
            assert!(
                p.output >= p.input && p.cache_read <= p.input,
                "{}",
                p.model
            );
        }
        assert_eq!(
            price_of("claude-opus-5-5")
                .unwrap()
                .to_proto()
                .cache_write_1h,
            8_000_000
        );
    }
}
