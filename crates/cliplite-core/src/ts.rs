//! MPEG-TS keyframe chunker.
//!
//! Splits a raw MPEG-TS byte stream (as produced by FFmpeg's mpegts muxer) into
//! chunks that each begin at a video keyframe. A chunk is a contiguous slice of
//! whole 188-byte packets, so `psi()` followed by consecutive chunks forms a
//! valid standalone transport stream.
use std::sync::Arc;

pub const TS_PACKET: usize = 188;
const SYNC: u8 = 0x47;
const PTS_MOD: u64 = 1 << 33;

#[derive(Clone, Debug)]
pub struct Chunk {
    pub epoch: u32,
    pub start_pts_90k: u64,
    pub end_pts_90k: u64,
    pub data: Arc<[u8]>,
}

impl Chunk {
    /// Duration in seconds, wrap-aware for the 33-bit 90 kHz PTS clock.
    pub fn duration_s(&self) -> f64 {
        let ticks = self.end_pts_90k.wrapping_sub(self.start_pts_90k) % PTS_MOD;
        ticks as f64 / 90_000.0
    }
}

pub struct TsChunker {
    epoch: u32,
    pending: Vec<u8>,
    pmt_pid: Option<u16>,
    video_pid: Option<u16>,
    pat: Option<[u8; TS_PACKET]>,
    pmt: Option<[u8; TS_PACKET]>,
    current: Option<(u64, Vec<u8>)>,
}

impl TsChunker {
    pub fn new(epoch: u32) -> Self {
        Self {
            epoch,
            pending: Vec::new(),
            pmt_pid: None,
            video_pid: None,
            pat: None,
            pmt: None,
            current: None,
        }
    }

    /// Feed raw bytes (any split); returns chunks completed by this call.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Chunk> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut pos = 0;
        loop {
            // Resync: skip to the next sync byte.
            match self.pending[pos..].iter().position(|&b| b == SYNC) {
                Some(skip) => pos += skip,
                None => {
                    pos = self.pending.len();
                    break;
                }
            }
            // A packet is trusted only once the next packet's sync byte is visible.
            if self.pending.len() - pos <= TS_PACKET {
                break;
            }
            if self.pending[pos + TS_PACKET] != SYNC {
                pos += 1; // false sync, keep scanning
                continue;
            }
            let mut pkt = [0u8; TS_PACKET];
            pkt.copy_from_slice(&self.pending[pos..pos + TS_PACKET]);
            pos += TS_PACKET;
            if let Some(chunk) = self.packet(&pkt) {
                out.push(chunk);
            }
        }
        self.pending.drain(..pos);
        out
    }

    /// Latest PAT+PMT packets (376 bytes), once seen.
    pub fn psi(&self) -> Option<Arc<[u8]>> {
        let (pat, pmt) = (self.pat.as_ref()?, self.pmt.as_ref()?);
        let mut v = Vec::with_capacity(2 * TS_PACKET);
        v.extend_from_slice(pat);
        v.extend_from_slice(pmt);
        Some(v.into())
    }

    fn packet(&mut self, pkt: &[u8; TS_PACKET]) -> Option<Chunk> {
        let pid = (u16::from(pkt[1] & 0x1F) << 8) | u16::from(pkt[2]);
        let pusi = pkt[1] & 0x40 != 0;

        if pid == 0 && pusi {
            if let Some(pmt_pid) = parse_pat(pkt) {
                self.pmt_pid = Some(pmt_pid);
                self.pat = Some(*pkt);
            }
        } else if Some(pid) == self.pmt_pid && pusi {
            if let Some(video) = parse_pmt(pkt) {
                self.video_pid = Some(video);
                self.pmt = Some(*pkt);
            }
        }

        let mut finished = None;
        if Some(pid) == self.video_pid && pusi && has_rai(pkt) {
            if let Some(pts) = parse_pes_pts(pkt) {
                if let Some((start, data)) = self.current.take() {
                    finished = Some(Chunk {
                        epoch: self.epoch,
                        start_pts_90k: start,
                        end_pts_90k: pts,
                        data: data.into(),
                    });
                }
                self.current = Some((pts, Vec::new()));
            }
        }
        if let Some((_, data)) = self.current.as_mut() {
            data.extend_from_slice(pkt);
        }
        finished
    }
}

/// Offset of the payload within a packet, or None if it has no payload.
fn payload_offset(pkt: &[u8; TS_PACKET]) -> Option<usize> {
    let afc = (pkt[3] >> 4) & 3;
    if afc & 1 == 0 {
        return None;
    }
    let off = if afc & 2 != 0 { 5 + pkt[4] as usize } else { 4 };
    (off < TS_PACKET).then_some(off)
}

/// Section bytes of a PSI packet (after the pointer field).
fn section(pkt: &[u8; TS_PACKET]) -> Option<&[u8]> {
    let off = payload_offset(pkt)?;
    let start = off + 1 + *pkt.get(off)? as usize;
    pkt.get(start..)
}

/// First program's PMT PID from a PAT packet.
fn parse_pat(pkt: &[u8; TS_PACKET]) -> Option<u16> {
    let s = section(pkt)?;
    if s.len() < 8 || s[0] != 0x00 {
        return None;
    }
    let len = (usize::from(s[1] & 0x0F) << 8) | usize::from(s[2]);
    let end = (3 + len).checked_sub(4)?.min(s.len());
    let mut i = 8;
    while i + 4 <= end {
        let program = (u16::from(s[i]) << 8) | u16::from(s[i + 1]);
        if program != 0 {
            return Some((u16::from(s[i + 2] & 0x1F) << 8) | u16::from(s[i + 3]));
        }
        i += 4;
    }
    None
}

/// First H.264 (0x1B) or HEVC (0x24) elementary stream PID from a PMT packet.
fn parse_pmt(pkt: &[u8; TS_PACKET]) -> Option<u16> {
    let s = section(pkt)?;
    if s.len() < 12 || s[0] != 0x02 {
        return None;
    }
    let len = (usize::from(s[1] & 0x0F) << 8) | usize::from(s[2]);
    let end = (3 + len).checked_sub(4)?.min(s.len());
    let mut i = 12 + ((usize::from(s[10] & 0x0F) << 8) | usize::from(s[11]));
    while i + 5 <= end {
        let stream_type = s[i];
        let pid = (u16::from(s[i + 1] & 0x1F) << 8) | u16::from(s[i + 2]);
        if stream_type == 0x1B || stream_type == 0x24 {
            return Some(pid);
        }
        i += 5 + ((usize::from(s[i + 3] & 0x0F) << 8) | usize::from(s[i + 4]));
    }
    None
}

/// Adaptation field present with random_access_indicator set.
fn has_rai(pkt: &[u8; TS_PACKET]) -> bool {
    pkt[3] & 0x20 != 0 && pkt[4] > 0 && pkt[5] & 0x40 != 0
}

/// 33-bit PTS from the PES header at the start of this packet's payload.
fn parse_pes_pts(pkt: &[u8; TS_PACKET]) -> Option<u64> {
    let off = payload_offset(pkt)?;
    let p = pkt.get(off..off + 14)?;
    if p[..3] != [0, 0, 1] || p[7] & 0x80 == 0 {
        return None;
    }
    let b = &p[9..14];
    Some(
        (u64::from(b[0] & 0x0E) << 29)
            | (u64::from(b[1]) << 22)
            | (u64::from(b[2] & 0xFE) << 14)
            | (u64::from(b[3]) << 7)
            | (u64::from(b[4]) >> 1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample_2s.ts");

    fn chunk_bytes(chunks: &[Chunk]) -> Vec<Vec<u8>> {
        chunks.iter().map(|c| c.data.to_vec()).collect()
    }

    #[test]
    fn fixture_splits_into_one_second_keyframe_chunks() {
        let bytes = std::fs::read(FIXTURE).unwrap();
        let mut c = TsChunker::new(0);
        let chunks: Vec<Chunk> = bytes.chunks(1000).flat_map(|b| c.push(b)).collect();
        assert_eq!(chunks.len(), 1); // 2 keyframes -> 1 closed chunk (second still open)
        assert!((chunks[0].duration_s() - 1.0).abs() < 0.05);
        assert_eq!(chunks[0].data.len() % TS_PACKET, 0);
        assert_eq!(chunks[0].data[0], 0x47);
        assert_eq!(c.psi().unwrap().len(), 2 * TS_PACKET);
    }

    #[test]
    fn split_points_do_not_matter() {
        let bytes = std::fs::read(FIXTURE).unwrap();
        let mut whole = TsChunker::new(0);
        let a = whole.push(&bytes);
        let mut single = TsChunker::new(0);
        let b: Vec<Chunk> = bytes.chunks(1).flat_map(|p| single.push(p)).collect();
        assert!(!a.is_empty());
        assert_eq!(chunk_bytes(&a), chunk_bytes(&b));
        assert_eq!(whole.psi().unwrap(), single.psi().unwrap());
    }

    #[test]
    fn resyncs_after_garbage() {
        let bytes = std::fs::read(FIXTURE).unwrap();
        let mut clean = TsChunker::new(0);
        let expected = clean.push(&bytes);
        let mut dirty_bytes = vec![0x11u8; 37];
        dirty_bytes.extend_from_slice(&bytes);
        let mut dirty = TsChunker::new(0);
        let got = dirty.push(&dirty_bytes);
        assert!(!expected.is_empty());
        assert_eq!(chunk_bytes(&expected), chunk_bytes(&got));
    }

    #[test]
    fn duration_wraps_at_33_bits() {
        let c = Chunk {
            epoch: 0,
            start_pts_90k: (1u64 << 33) - 45_000,
            end_pts_90k: 45_000,
            data: vec![].into(),
        };
        assert!((c.duration_s() - 1.0).abs() < 1e-9);
    }
}
