//! Bounded reordering and loss concealment decisions; caller supplies the 20 ms clock.
use crate::EncodedFrame;
use std::collections::BTreeMap;
pub enum Playout {
    Wait,
    Packet(Vec<u8>),
    Conceal,
}
pub struct Jitter {
    packets: BTreeMap<u64, Vec<u8>>,
    expected: Option<u64>,
    started: bool,
    target: usize,
    stable: usize,
    pub dropped: u64,
    pub concealed: u64,
}
impl Default for Jitter {
    fn default() -> Self {
        Self {
            packets: BTreeMap::new(),
            expected: None,
            started: false,
            target: 3,
            stable: 0,
            dropped: 0,
            concealed: 0,
        }
    }
}
impl Jitter {
    pub fn push(&mut self, frame: EncodedFrame) {
        if self
            .expected
            .is_some_and(|seq| self.started && frame.sequence < seq)
        {
            self.dropped += 1;
            return;
        }
        if self.packets.contains_key(&frame.sequence) {
            self.dropped += 1;
            return;
        }
        self.packets.insert(frame.sequence, frame.opus);
        if self.packets.len() > 25 {
            // Prefer fresh audio to unbounded latency.
            while self.packets.len() > self.target {
                self.packets.pop_first();
                self.dropped += 1;
            }
            self.expected = self.packets.first_key_value().map(|(seq, _)| *seq);
        }
    }
    pub fn pop(&mut self) -> Playout {
        if !self.started {
            if self.packets.len() < self.target {
                return Playout::Wait;
            }
            self.expected = self.packets.first_key_value().map(|(seq, _)| *seq);
            self.started = true;
        }
        let seq = self.expected.unwrap_or(0);
        self.expected = Some(seq.saturating_add(1));
        if let Some(data) = self.packets.remove(&seq) {
            self.stable += 1;
            if self.stable >= 250 {
                self.target = self.target.saturating_sub(1).max(3);
                self.stable = 0;
            }
            Playout::Packet(data)
        } else {
            self.concealed += 1;
            self.stable = 0;
            self.target = (self.target + 1).min(10);
            if self.packets.is_empty()
                || self
                    .packets
                    .first_key_value()
                    .is_some_and(|(next, _)| next.saturating_sub(seq) > 25)
            {
                self.started = false;
                self.expected = None;
            }
            Playout::Conceal
        }
    }
    pub fn buffer_ms(&self) -> u32 {
        self.packets.len() as u32 * 20
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn f(seq: u64) -> EncodedFrame {
        EncodedFrame {
            sequence: seq,
            opus: vec![seq as u8],
        }
    }
    #[test]
    fn reorders_duplicates_and_conceals_a_gap() {
        let mut j = Jitter::default();
        j.push(f(1));
        assert!(matches!(j.pop(), Playout::Wait));
        j.push(f(3));
        j.push(f(2));
        j.push(f(2));
        for i in 1..=3 {
            assert!(matches!(j.pop(),Playout::Packet(p) if p==vec![i]));
        }
        assert!(matches!(j.pop(), Playout::Conceal));
        assert!(matches!(j.pop(), Playout::Wait));
        assert_eq!(j.dropped, 1);
        assert_eq!(j.concealed, 1);
    }
    #[test]
    fn latency_and_forward_jump_are_bounded() {
        let mut j = Jitter::default();
        for i in 0..1000 {
            j.push(f(i));
        }
        assert!(j.buffer_ms() <= 500);
        assert!(j.dropped > 0);
        for _ in 0..26 {
            j.pop();
        }
        j.push(f(10_000));
        j.push(f(10_001));
        j.push(f(10_002));
        for _ in 0..30 {
            j.pop();
        }
        assert!(j.buffer_ms() <= 500);
    }
}
