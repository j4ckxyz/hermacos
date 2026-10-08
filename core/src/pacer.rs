//! Smooths a bursty token stream into steady, readable motion.
//!
//! Tokens arrive in clumps: nothing for 300 ms, then forty characters at once. Painting them as
//! they land looks jittery. The pacer keeps the full text but reveals it at a rate that tracks
//! the backlog: slow and even when the model is slow, faster when text has piled up, so the
//! display never falls far behind and never lurches.
//!
//! It also reports `fade`: the opacity of the newest characters, by age. The shell applies it
//! to the last glyphs on screen, so text bleeds in instead of popping.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::markdown::{self, MdDocument};

/// How long a character takes to reach full opacity after it is revealed.
const FADE_SECS: f64 = 0.28;
/// At most this many trailing characters are faded individually.
const FADE_MAX_CHARS: usize = 48;
/// Seconds over which a backlog should be worked off.
const CATCH_UP_SECS: f64 = 0.55;
const MIN_RATE: f64 = 45.0;
const MAX_RATE: f64 = 4_000.0;
/// Once the stream has ended, show whatever is left within this long.
const FINISH_SECS: f64 = 0.28;

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct StreamFrame {
    /// Re-parsed document when the visible text changed since the last tick.
    pub document: Option<MdDocument>,
    pub revealed_chars: u32,
    /// Opacity (0..1) of the newest characters that are still fading in, oldest first; the
    /// last entry belongs to the last visible character. Empty once everything is opaque.
    pub fade: Vec<f32>,
    /// Everything is visible and fully faded in; the shell can stop ticking.
    pub settled: bool,
}

/// Characters `start..end` became visible at time `at`.
struct Reveal {
    start: usize,
    end: usize,
    at: f64,
}

#[derive(Default)]
struct State {
    text: String,
    total_chars: usize,
    revealed_chars: usize,
    revealed_bytes: usize,
    head: f64,
    rate: f64,
    clock: f64,
    /// Recent reveal batches, oldest first; only those still fading are kept.
    reveals: VecDeque<Reveal>,
    finished: bool,
    /// Show without fading (the user skipped the animation).
    instant: bool,
    dirty: bool,
}

#[derive(uniffi::Object)]
pub struct StreamPacer {
    state: Mutex<State>,
}

#[uniffi::export]
impl StreamPacer {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(State { dirty: true, ..State::default() }) })
    }

    /// More text arrived from the stream.
    pub fn append(&self, delta: String) {
        let mut s = self.state.lock().unwrap();
        s.total_chars += delta.chars().count();
        s.text.push_str(&delta);
        s.finished = false;
        s.instant = false;
    }

    /// Replace the text (the turn's authoritative final answer). What is already on screen
    /// stays revealed as long as the new text starts the same way.
    pub fn set_text(&self, text: String) {
        let mut s = self.state.lock().unwrap();
        if s.text == text {
            return;
        }
        let keep = text.starts_with(&s.text[..s.revealed_bytes]);
        s.total_chars = text.chars().count();
        s.text = text;
        if !keep {
            s.revealed_chars = 0;
            s.revealed_bytes = 0;
            s.head = 0.0;
            s.reveals.clear();
        }
        s.head = s.head.min(s.total_chars as f64);
        s.dirty = true;
    }

    /// No more text is coming: flush what is left quickly and let the tail finish fading.
    pub fn finish(&self) {
        let mut s = self.state.lock().unwrap();
        s.finished = true;
        s.dirty = true;
    }

    /// Skip the animation and show everything now.
    pub fn reveal_all(&self) {
        let mut s = self.state.lock().unwrap();
        s.finished = true;
        s.head = s.total_chars as f64;
        s.instant = true;
        s.reveals.clear();
        s.dirty = true;
    }

    pub fn text(&self) -> String {
        self.state.lock().unwrap().text.clone()
    }

    /// Advance by `dt` seconds and report what should be on screen.
    pub fn tick(&self, dt: f64) -> StreamFrame {
        let mut s = self.state.lock().unwrap();
        let dt = dt.clamp(0.0, 0.1);
        let total = s.total_chars as f64;
        let backlog = (total - s.head).max(0.0);
        let target = if s.finished {
            (backlog / FINISH_SECS).max(MIN_RATE * 3.0)
        } else {
            (backlog / CATCH_UP_SECS).clamp(MIN_RATE, MAX_RATE)
        };
        // Ease toward the target so bursts change speed gradually.
        let blend = 1.0 - (-dt * 10.0).exp();
        s.rate += (target - s.rate) * blend;
        s.head = (s.head + s.rate * dt).min(total);
        s.clock += dt;

        let reveal_to = (s.head.floor() as usize).min(s.total_chars);
        let changed = reveal_to != s.revealed_chars || s.dirty;
        if reveal_to > s.revealed_chars {
            let step = reveal_to - s.revealed_chars;
            let from = s.revealed_bytes;
            let advance = s.text[from..].char_indices().nth(step).map(|(i, _)| i).unwrap_or(s.text.len() - from);
            s.revealed_bytes += advance;
            if !s.instant {
                let batch = Reveal { start: s.revealed_chars, end: reveal_to, at: s.clock };
                s.reveals.push_back(batch);
            }
            s.revealed_chars = reveal_to;
        }
        s.dirty = false;

        // Forget batches that are fully opaque or fall outside the per-character window.
        let now = s.clock;
        let floor = s.revealed_chars.saturating_sub(FADE_MAX_CHARS);
        while s.reveals.front().is_some_and(|r| now - r.at >= FADE_SECS || r.end <= floor) {
            s.reveals.pop_front();
        }
        let mut fade = Vec::new();
        for reveal in &s.reveals {
            let opacity = ((now - reveal.at) / FADE_SECS).clamp(0.0, 1.0) as f32;
            fade.extend(std::iter::repeat_n(opacity, reveal.end - reveal.start.max(floor)));
        }

        let all_visible = s.revealed_chars == s.total_chars;
        let document = changed.then(|| {
            let still_streaming = !(s.finished && all_visible);
            markdown::parse(&s.text[..s.revealed_bytes], still_streaming)
        });
        StreamFrame {
            document,
            revealed_chars: s.revealed_chars as u32,
            settled: s.finished && all_visible && fade.is_empty(),
            fade,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pacer: &StreamPacer, seconds: f64) -> StreamFrame {
        let mut frame = pacer.tick(0.0);
        for _ in 0..(seconds * 60.0) as usize {
            frame = pacer.tick(1.0 / 60.0);
        }
        frame
    }

    #[test]
    fn reveals_gradually_then_settles() {
        let pacer = StreamPacer::new();
        pacer.append("word ".repeat(80));
        let early = run(&pacer, 0.1);
        assert!(early.revealed_chars > 0 && early.revealed_chars < 400, "{}", early.revealed_chars);
        assert!(!early.settled);
        pacer.finish();
        let done = run(&pacer, 1.0);
        assert_eq!(done.revealed_chars, 400);
        assert!(done.settled);
        assert!(done.fade.is_empty());
    }

    #[test]
    fn tail_keeps_fading_in_while_waiting_for_tokens() {
        let pacer = StreamPacer::new();
        pacer.append("hello".into());
        let frame = run(&pacer, 1.0);
        assert_eq!(frame.revealed_chars, 5);
        assert!(frame.fade.is_empty(), "the tail finished fading while idle");
        assert!(!frame.settled, "not settled until the stream finishes");
        assert!(pacer.tick(1.0 / 60.0).document.is_none(), "idle ticks don't re-parse");
    }

    #[test]
    fn multibyte_text_is_cut_on_character_boundaries() {
        let pacer = StreamPacer::new();
        pacer.append("héllo wörld — ✓ 日本語 🎉 done".into());
        for _ in 0..200 {
            pacer.tick(1.0 / 120.0);
        }
        pacer.finish();
        assert_eq!(run(&pacer, 1.0).revealed_chars, 26);
    }

    #[test]
    fn newest_characters_fade_in_by_age() {
        let pacer = StreamPacer::new();
        pacer.append("abcdefghij".repeat(4));
        let frame = run(&pacer, 0.15);
        assert!(!frame.fade.is_empty());
        assert!(frame.fade.len() <= frame.revealed_chars as usize);
        assert!(frame.fade.windows(2).all(|w| w[0] >= w[1]), "older characters are more opaque: {:?}", frame.fade);
        assert!(frame.fade.iter().all(|o| (0.0..1.0).contains(o)));
    }

    #[test]
    fn final_text_replaces_without_restarting() {
        let pacer = StreamPacer::new();
        pacer.append("Hello wor".into());
        run(&pacer, 1.0);
        pacer.set_text("Hello world, final.".into());
        assert_eq!(pacer.tick(0.0).revealed_chars, 9);
        pacer.reveal_all();
        let frame = pacer.tick(1.0 / 60.0);
        assert!(frame.settled);
        assert_eq!(frame.revealed_chars, 19);
    }
}
