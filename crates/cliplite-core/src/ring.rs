//! In-memory replay ring of keyframe-aligned MPEG-TS chunks.
//!
//! Bounded by seconds and bytes; `snapshot` hands out cheap `Arc` clones that
//! the caller can write to disk without holding the ring's lock for I/O.
use crate::ts::Chunk;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingLimits {
    pub max_secs: f64,
    pub max_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingStats {
    pub secs: f64,
    pub bytes: u64,
    pub chunks: usize,
}

/// Chunks grouped by epoch, each group prefixed by that epoch's PSI, oldest first.
pub struct Snapshot {
    pub segments: Vec<(Arc<[u8]>, Vec<Chunk>)>,
    pub secs: f64,
}

pub struct ReplayRing {
    limits: RingLimits,
    chunks: VecDeque<Chunk>,
    psi: HashMap<u32, Arc<[u8]>>,
    secs: f64,
    bytes: u64,
}

impl ReplayRing {
    pub fn new(limits: RingLimits) -> Self {
        Self {
            limits,
            chunks: VecDeque::new(),
            psi: HashMap::new(),
            secs: 0.0,
            bytes: 0,
        }
    }

    pub fn set_limits(&mut self, limits: RingLimits) {
        self.limits = limits;
        self.evict();
    }

    /// Record the PSI (PAT/PMT) bytes for an epoch. May be called before the
    /// epoch's first chunk arrives; entries are pruned once no chunk of that
    /// epoch remains after an eviction.
    pub fn set_psi(&mut self, epoch: u32, psi: Arc<[u8]>) {
        self.psi.insert(epoch, psi);
    }

    pub fn push(&mut self, chunk: Chunk) {
        self.secs += chunk.duration_s();
        self.bytes += chunk.data.len() as u64;
        self.chunks.push_back(chunk);
        self.evict();
    }

    /// Newest chunks totalling at least `secs` (or everything), oldest first,
    /// grouped by epoch. Only `Arc` clones; no payload bytes are copied.
    pub fn snapshot(&self, secs: f64) -> Option<Snapshot> {
        let mut total = 0.0;
        let mut start = self.chunks.len();
        for (i, c) in self.chunks.iter().enumerate().rev() {
            if total >= secs {
                break;
            }
            total += c.duration_s();
            start = i;
        }
        if start == self.chunks.len() {
            return None;
        }
        let mut segments: Vec<(Arc<[u8]>, Vec<Chunk>)> = Vec::new();
        for c in self.chunks.iter().skip(start) {
            match segments.last_mut() {
                Some((_, group)) if group[0].epoch == c.epoch => group.push(c.clone()),
                _ => segments.push((self.psi.get(&c.epoch)?.clone(), vec![c.clone()])),
            }
        }
        Some(Snapshot { segments, secs: total })
    }

    pub fn stats(&self) -> RingStats {
        RingStats {
            secs: self.secs.max(0.0),
            bytes: self.bytes,
            chunks: self.chunks.len(),
        }
    }

    fn evict(&mut self) {
        while self.chunks.len() > 1
            && (self.secs > self.limits.max_secs || self.bytes > self.limits.max_bytes)
        {
            let old = self.chunks.pop_front().expect("len > 1");
            self.secs -= old.duration_s();
            self.bytes -= old.data.len() as u64;
            if !self.chunks.iter().any(|c| c.epoch == old.epoch) {
                self.psi.remove(&old.epoch);
            }
        }
        if self.chunks.is_empty() {
            self.secs = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: RingLimits = RingLimits { max_secs: 60.0, max_bytes: u64::MAX };

    fn ch(epoch: u32, start_s: f64, len_s: f64, bytes: usize) -> Chunk {
        let m = 1u64 << 33;
        let start = (start_s * 90_000.0) as u64 % m;
        let end = ((start_s + len_s) * 90_000.0) as u64 % m;
        Chunk { epoch, start_pts_90k: start, end_pts_90k: end, data: vec![0x47; bytes].into() }
    }

    fn psi(b: u8) -> Arc<[u8]> {
        vec![b; 4].into()
    }

    #[test]
    fn evicts_by_seconds() {
        let mut r = ReplayRing::new(L);
        for i in 0..70 {
            r.push(ch(0, i as f64, 1.0, 100));
        }
        let s = r.stats();
        assert!((s.secs - 60.0).abs() < 1e-6, "{}", s.secs);
        assert_eq!(s.chunks, 60);
        assert_eq!(s.bytes, 6000);
    }

    #[test]
    fn evicts_by_bytes() {
        let mut r = ReplayRing::new(RingLimits { max_secs: 1e9, max_bytes: 10_000 });
        for i in 0..20 {
            r.push(ch(0, i as f64, 1.0, 3_000));
        }
        let s = r.stats();
        assert!(s.bytes <= 10_000);
        assert_eq!(s.chunks, 3);
    }

    #[test]
    fn oversized_single_chunk_is_kept() {
        let mut r = ReplayRing::new(RingLimits { max_secs: 1.0, max_bytes: 10 });
        r.push(ch(0, 0.0, 5.0, 1000));
        assert_eq!(r.stats().chunks, 1);
    }

    #[test]
    fn snapshot_covers_requested_seconds_from_a_keyframe() {
        let mut r = ReplayRing::new(L);
        r.set_psi(0, psi(1));
        for i in 0..30 {
            r.push(ch(0, i as f64, 1.0, 100));
        }
        let s = r.snapshot(10.0).unwrap();
        assert!(s.secs >= 10.0 && s.secs < 11.0, "{}", s.secs);
        assert_eq!(s.segments.len(), 1);
        assert_eq!(s.segments[0].1.len(), 10);
        // Oldest first: the snapshot ends at the newest chunk.
        assert_eq!(s.segments[0].1.last().unwrap().start_pts_90k, 29 * 90_000);
    }

    #[test]
    fn snapshot_larger_than_ring_uses_everything() {
        let mut r = ReplayRing::new(L);
        r.set_psi(0, psi(1));
        for i in 0..5 {
            r.push(ch(0, i as f64, 1.0, 100));
        }
        let s = r.snapshot(100.0).unwrap();
        assert!((s.secs - 5.0).abs() < 1e-6);
    }

    #[test]
    fn snapshot_groups_by_epoch_with_psi() {
        let mut r = ReplayRing::new(L);
        r.set_psi(0, psi(0xA));
        r.set_psi(1, psi(0xB));
        for (e, t) in [(0, 0.0), (0, 1.0), (1, 0.0), (1, 1.0)] {
            r.push(ch(e, t, 1.0, 100));
        }
        let s = r.snapshot(100.0).unwrap();
        assert_eq!(s.segments.len(), 2);
        assert_eq!(s.segments[0].0[0], 0xA);
        assert_eq!(s.segments[0].1.len(), 2);
        assert_eq!(s.segments[1].0[0], 0xB);
        assert_eq!(s.segments[1].1.len(), 2);
        assert!(s.segments[1].1.iter().all(|c| c.epoch == 1));
    }

    #[test]
    fn snapshot_empty_is_none() {
        assert!(ReplayRing::new(L).snapshot(5.0).is_none());
    }

    #[test]
    fn snapshot_missing_psi_is_none() {
        let mut r = ReplayRing::new(L);
        r.set_psi(0, psi(1));
        r.push(ch(0, 0.0, 1.0, 100));
        r.push(ch(1, 0.0, 1.0, 100));
        // Newest epoch has no PSI, so the snapshot cannot be made.
        assert!(r.snapshot(1.0).is_none());
        // Only the epoch-1 chunk is needed for 1 s, and epoch 0 is excluded
        // from a shorter request; but a request reaching epoch 0 still fails.
        r.set_psi(1, psi(2));
        assert!(r.snapshot(2.0).is_some());
    }

    #[test]
    fn snapshot_ignores_missing_psi_of_unincluded_epoch() {
        let mut r = ReplayRing::new(L);
        r.push(ch(0, 0.0, 1.0, 100)); // epoch 0 has no PSI
        r.set_psi(1, psi(2));
        r.push(ch(1, 0.0, 1.0, 100));
        assert!(r.snapshot(1.0).is_some());
        assert!(r.snapshot(2.0).is_none());
    }

    #[test]
    fn ring_handles_pts_wraparound() {
        let mut r = ReplayRing::new(L);
        r.set_psi(0, psi(1));
        let base = ((1u64 << 33) / 90_000) as f64 - 5.0; // 5 s before the wrap
        for i in 0..10 {
            r.push(ch(0, base + i as f64, 1.0, 100));
        }
        let s = r.stats();
        assert!((s.secs - 10.0).abs() < 0.01, "{}", s.secs);
        let snap = r.snapshot(100.0).unwrap();
        assert!((snap.secs - 10.0).abs() < 0.01);
    }

    #[test]
    fn shrinking_limits_evicts_now() {
        let mut r = ReplayRing::new(L);
        for i in 0..60 {
            r.push(ch(0, i as f64, 1.0, 100));
        }
        r.set_limits(RingLimits { max_secs: 30.0, max_bytes: u64::MAX });
        let s = r.stats();
        assert!((s.secs - 30.0).abs() < 1e-6, "{}", s.secs);
        assert_eq!(s.chunks, 30);
        assert_eq!(s.bytes, 3000);
    }

    #[test]
    fn psi_for_fully_evicted_epochs_is_pruned() {
        let mut r = ReplayRing::new(RingLimits { max_secs: 2.0, max_bytes: u64::MAX });
        r.set_psi(0, psi(1));
        r.push(ch(0, 0.0, 1.0, 100));
        r.set_psi(1, psi(2));
        r.push(ch(1, 0.0, 1.0, 100));
        r.push(ch(1, 1.0, 1.0, 100)); // evicts the epoch-0 chunk
        assert!(!r.psi.contains_key(&0));
        assert!(r.psi.contains_key(&1));
        // PSI set for an epoch before its first chunk arrives is retained.
        r.set_psi(2, psi(3));
        r.push(ch(2, 0.0, 1.0, 100));
        assert!(r.psi.contains_key(&2));
    }
}
