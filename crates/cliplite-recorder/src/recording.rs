//! Recording sink: appends engine chunks to MPEG-TS segment files on disk.
//! `finish` hands the segments to a concat remux that produces the final file.
use anyhow::{bail, Context};
use cliplite_core::ts::Chunk;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Segment files of one recording. A new segment (PSI first, so each file is a
/// standalone transport stream) starts on the first chunk, after a resume, and
/// when the engine's epoch changes (FFmpeg restarted: new timestamps and PSI).
pub struct RecordingSink {
    dir: PathBuf,
    out: PathBuf,
    segments: Vec<PathBuf>,
    current: Option<BufWriter<File>>,
    paused: bool,
    epoch: Option<u32>,
    /// First write error; `finish` reports it instead of a truncated file.
    error: Option<String>,
}

impl RecordingSink {
    /// `temp_dir` is this recording's own scratch directory (created here).
    pub fn new(temp_dir: PathBuf, out: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&temp_dir)
            .with_context(|| format!("create {}", temp_dir.display()))?;
        Ok(Self {
            dir: temp_dir,
            out,
            segments: Vec::new(),
            current: None,
            paused: false,
            epoch: None,
            error: None,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append a chunk (dropped while paused). `psi` is the chunk's epoch PSI.
    pub fn on_chunk(&mut self, chunk: &Chunk, psi: &[u8]) -> std::io::Result<()> {
        if self.paused {
            return Ok(());
        }
        let res = self.append(chunk, psi);
        if let Err(e) = &res {
            self.error.get_or_insert_with(|| e.to_string());
        }
        res
    }

    fn append(&mut self, chunk: &Chunk, psi: &[u8]) -> std::io::Result<()> {
        if self.current.is_none() || self.epoch != Some(chunk.epoch) {
            self.close_segment()?;
            let path = self.dir.join(format!("seg{}.ts", self.segments.len()));
            let mut w = BufWriter::new(File::create(&path)?);
            self.segments.push(path);
            w.write_all(psi)?;
            self.current = Some(w);
            self.epoch = Some(chunk.epoch);
        }
        self.current.as_mut().expect("segment open").write_all(&chunk.data)
    }

    fn close_segment(&mut self) -> std::io::Result<()> {
        match self.current.take() {
            Some(mut w) => w.flush(),
            None => Ok(()),
        }
    }

    pub fn pause(&mut self) {
        self.paused = true;
        if let Err(e) = self.close_segment() {
            self.error.get_or_insert_with(|| e.to_string());
        }
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// Close the open segment; returns (segment files in order, output path).
    pub fn finish(mut self) -> anyhow::Result<(Vec<PathBuf>, PathBuf)> {
        self.close_segment().context("flush recording segment")?;
        if let Some(e) = &self.error {
            bail!("recording write failed: {e}");
        }
        if self.segments.is_empty() {
            bail!("recording is empty");
        }
        Ok((self.segments, self.out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn chunk(epoch: u32, byte: u8) -> Chunk {
        Chunk { epoch, start_pts_90k: 0, end_pts_90k: 90_000, data: Arc::from(vec![byte; 188]) }
    }

    fn sink(name: &str) -> (RecordingSink, PathBuf) {
        let dir = std::env::temp_dir()
            .join("cliplite sink test")
            .join(format!("{name}-{}", cliplite_core::telemetry::new_cid()));
        let out = dir.with_extension("mp4");
        (RecordingSink::new(dir.clone(), out).unwrap(), dir)
    }

    fn read(p: &PathBuf) -> Vec<u8> {
        std::fs::read(p).unwrap()
    }

    #[test]
    fn writes_psi_then_chunks_into_one_segment() {
        let (mut s, dir) = sink("one");
        s.on_chunk(&chunk(0, 1), &[9; 4]).unwrap();
        s.on_chunk(&chunk(0, 2), &[9; 4]).unwrap();
        let (segs, out) = s.finish().unwrap();
        assert_eq!(out, dir.with_extension("mp4"));
        assert_eq!(segs.len(), 1);
        assert!(segs[0].starts_with(&dir));
        let mut want = vec![9; 4];
        want.extend([1; 188]);
        want.extend([2; 188]);
        assert_eq!(read(&segs[0]), want);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_segment_on_resume() {
        let (mut s, dir) = sink("resume");
        s.on_chunk(&chunk(0, 1), &[9; 4]).unwrap();
        s.pause();
        s.on_chunk(&chunk(0, 2), &[9; 4]).unwrap(); // dropped while paused
        s.resume();
        s.on_chunk(&chunk(0, 3), &[9; 4]).unwrap();
        let (segs, _) = s.finish().unwrap();
        assert_eq!(segs.len(), 2);
        let seg = |b: u8| [vec![9; 4], vec![b; 188]].concat();
        assert_eq!(read(&segs[0]), seg(1));
        assert_eq!(read(&segs[1]), seg(3));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_segment_on_epoch_change() {
        let (mut s, dir) = sink("epoch");
        s.on_chunk(&chunk(0, 1), &[8; 4]).unwrap();
        s.on_chunk(&chunk(1, 2), &[9; 4]).unwrap();
        s.on_chunk(&chunk(1, 3), &[9; 4]).unwrap();
        let (segs, _) = s.finish().unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(read(&segs[0]), [vec![8; 4], vec![1; 188]].concat());
        assert_eq!(read(&segs[1]), [vec![9; 4], vec![2; 188], vec![3; 188]].concat());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finish_without_chunks_is_an_error() {
        let (mut s, dir) = sink("empty");
        s.pause();
        s.on_chunk(&chunk(0, 1), &[9; 4]).unwrap();
        assert!(s.finish().is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
