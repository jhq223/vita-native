//! Read the profile's scalar MP4 metadata without retaining sample tables.
use super::{Info, timing};
use crate::app::memory::{MemoryBudget, MemoryReservation};
use std::{
    format,
    io::{Read, Seek, SeekFrom},
    prelude::v1::*,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy)]
pub(super) struct Atom {
    pub kind: [u8; 4],
    pub start: u64,
    pub end: u64,
}

pub(super) struct Metadata {
    pub info: Info,
    pub audio_frame_samples: usize,
    pub sps: Vec<u8>,
    pub sps_storage: Box<dyn MemoryReservation>,
}

pub(super) struct Reader<'a, R: ?Sized> {
    source: &'a mut R,
    stopped: &'a AtomicBool,
}
impl<R: Read + Seek + ?Sized> Reader<'_, R> {
    pub(super) fn bytes<const N: usize>(
        &mut self,
        atom: Atom,
        offset: u64,
    ) -> Result<[u8; N], String> {
        let mut bytes = [0; N];
        self.read_into(atom, offset, &mut bytes)?;
        Ok(bytes)
    }
    pub(super) fn read_into(
        &mut self,
        atom: Atom,
        offset: u64,
        output: &mut [u8],
    ) -> Result<(), String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("movie metadata read cancelled".into());
        }
        let at = atom
            .start
            .checked_add(offset)
            .filter(|&at| {
                at.checked_add(output.len() as u64)
                    .is_some_and(|end| end <= atom.end)
            })
            .ok_or_else(|| {
                format!(
                    "MP4 {} field at {offset} exceeds its box",
                    String::from_utf8_lossy(&atom.kind)
                )
            })?;
        let current = self.source.stream_position().map_err(|e| e.to_string())?;
        if let Ok(delta) = i64::try_from(i128::from(at) - i128::from(current)) {
            self.source
                .seek_relative(delta)
                .map_err(|e| e.to_string())?;
        } else {
            self.source
                .seek(SeekFrom::Start(at))
                .map_err(|e| e.to_string())?;
        }
        self.source
            .read_exact(output)
            .map_err(|e| format!("MP4 {} at {at}: {e}", String::from_utf8_lossy(&atom.kind)))
    }
    fn child(&mut self, parent: Atom, at: u64) -> Result<Atom, String> {
        let raw = Atom {
            kind: *b"box ",
            start: at,
            end: parent.end,
        };
        let header = self.bytes::<8>(raw, 0)?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap());
        let (size, header_size) = match size {
            0 => (parent.end - at, 8),
            1 => (u64::from_be_bytes(self.bytes(raw, 8)?), 16),
            n => (u64::from(n), 8),
        };
        let end = at
            .checked_add(size)
            .filter(|&end| size >= header_size && end <= parent.end)
            .ok_or("MP4 child box has an invalid extent")?;
        Ok(Atom {
            kind: header[4..].try_into().unwrap(),
            start: at + header_size,
            end,
        })
    }
    pub(super) fn find(&mut self, parent: Atom, kind: &[u8; 4], skip: u64) -> Result<Atom, String> {
        let mut at = parent
            .start
            .checked_add(skip)
            .filter(|&at| at <= parent.end)
            .ok_or("truncated MP4 container")?;
        while at < parent.end {
            let atom = self.child(parent, at)?;
            if &atom.kind == kind {
                return Ok(atom);
            }
            at = atom.end;
        }
        Err(format!(
            "MP4 {} is missing {}",
            String::from_utf8_lossy(&parent.kind),
            String::from_utf8_lossy(kind)
        ))
    }
    pub(super) fn optional(
        &mut self,
        parent: Atom,
        kind: &[u8; 4],
    ) -> Result<Option<Atom>, String> {
        let mut at = parent.start;
        while at < parent.end {
            let atom = self.child(parent, at)?;
            if &atom.kind == kind {
                return Ok(Some(atom));
            }
            at = atom.end;
        }
        Ok(None)
    }
    fn description(&mut self, samples: Atom) -> Result<Atom, String> {
        let descriptions = self.find(samples, b"stsd", 0)?;
        if u32::from_be_bytes(self.bytes(descriptions, 4)?) != 1 {
            return Err(
                "Vita movie profile requires exactly one sample description per track".into(),
            );
        }
        self.child(descriptions, descriptions.start + 8)
    }
    fn descriptor(&mut self, parent: Atom, offset: u64, expected: u8) -> Result<Atom, String> {
        if self.bytes::<1>(parent, offset)?[0] != expected {
            return Err(format!("MP4 esds is missing descriptor 0x{expected:02x}"));
        }
        let mut size = 0_u32;
        for index in 0..4 {
            let byte = self.bytes::<1>(parent, offset + 1 + index)?[0];
            size = (size << 7) | u32::from(byte & 0x7f);
            if byte & 0x80 == 0 {
                let start = parent.start + offset + 2 + index;
                let end = start
                    .checked_add(u64::from(size))
                    .filter(|&end| end <= parent.end)
                    .ok_or("MP4 esds descriptor exceeds its box")?;
                return Ok(Atom {
                    kind: *b"esds",
                    start,
                    end,
                });
            }
        }
        Err("MP4 esds descriptor length exceeds four bytes".into())
    }
    fn aac(&mut self, entry: Atom) -> Result<(u16, u32, usize), String> {
        let esds = self.find(entry, b"esds", 28)?;
        let es = self.descriptor(esds, 4, 3)?;
        let flags = self.bytes::<1>(es, 2)?[0];
        let mut offset = 3_u64;
        if flags & 0x80 != 0 {
            offset += 2;
        }
        if flags & 0x40 != 0 {
            offset += 1 + u64::from(self.bytes::<1>(es, offset)?[0]);
        }
        if flags & 0x20 != 0 {
            offset += 2;
        }
        let config = self.descriptor(es, offset, 4)?;
        if self.bytes::<1>(config, 0)?[0] != 0x40 {
            return Err("Vita movie profile requires MPEG-4 AAC audio".into());
        }
        let specific = self.descriptor(config, 13, 5)?;
        let bits = u16::from_be_bytes(self.bytes(specific, 0)?);
        if bits >> 11 != 2 {
            return Err("Vita movie profile requires AAC-LC (object type 2) audio".into());
        }
        let frequency = usize::from((bits >> 7) & 15);
        let channels = (bits >> 3) & 15;
        const RATES: [u32; 13] = [
            96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
        ];
        let rate = *RATES
            .get(frequency)
            .ok_or("Vita AAC profile requires an indexed sample rate")?;
        if !matches!(channels, 1 | 2) {
            return Err(format!(
                "Vita AAC channel configuration {channels} requires mono or stereo"
            ));
        }
        Ok((channels, rate, if bits & 4 == 0 { 1024 } else { 960 }))
    }
}

pub(super) fn read<R: Read + Seek + ?Sized>(
    source: &mut R,
    length: u64,
    stopped: &AtomicBool,
    budget: &dyn MemoryBudget,
) -> Result<Metadata, String> {
    let mut reader = Reader { source, stopped };
    let root = Atom {
        kind: *b"file",
        start: 0,
        end: length,
    };
    if reader.optional(root, b"moof")?.is_some() {
        return Err("Vita movie profile requires non-fragmented MP4".into());
    }
    let movie = reader.find(root, b"moov", 0)?;
    if reader.optional(movie, b"mvex")?.is_some() {
        return Err("Vita movie profile requires non-fragmented MP4".into());
    }
    let mut at = movie.start;
    let mut video = None;
    let mut audio = None;
    while at < movie.end {
        let track = reader.child(movie, at)?;
        at = track.end;
        if track.kind != *b"trak" {
            continue;
        }
        let media = reader.find(track, b"mdia", 0)?;
        let handler = reader.find(media, b"hdlr", 0)?;
        let kind = reader.bytes::<4>(handler, 8)?;
        if !matches!(&kind, b"vide" | b"soun") {
            return Err(format!(
                "Vita movie profile does not admit {} tracks",
                String::from_utf8_lossy(&kind)
            ));
        }
        let minf = reader.find(media, b"minf", 0)?;
        let samples = reader.find(minf, b"stbl", 0)?;
        let entry = reader.description(samples)?;
        if kind == *b"vide" {
            if video.is_some() {
                return Err("Vita movie profile supports one H.264 video track".into());
            }
            if !matches!(&entry.kind, b"avc1" | b"avc3") {
                return Err("Vita movie profile requires H.264 avc1/avc3 video".into());
            }
            let size = reader.bytes::<4>(entry, 24)?;
            let width = u32::from(u16::from_be_bytes(size[..2].try_into().unwrap()));
            let height = u32::from(u16::from_be_bytes(size[2..].try_into().unwrap()));
            if width == 0
                || height == 0
                || width > 960
                || height > 544
                || !width.is_multiple_of(2)
                || !height.is_multiple_of(2)
            {
                return Err(format!(
                    "Vita movie visible dimensions {width}x{height} require positive even dimensions up to 960x544"
                ));
            }
            let avcc = reader.find(entry, b"avcC", 78)?;
            if reader.bytes::<1>(avcc, 5)?[0] & 31 == 0 {
                return Err("MP4 avcC has no sequence parameter set".into());
            }
            let sps_size = u16::from_be_bytes(reader.bytes(avcc, 6)?) as usize;
            if sps_size == 0
                || avcc
                    .start
                    .checked_add(8)
                    .and_then(|start| start.checked_add(sps_size as u64))
                    .is_none_or(|end| end > avcc.end)
            {
                return Err("MP4 avcC sequence parameter set is truncated".into());
            }
            let sps_storage = budget.reserve(sps_size as u64)?;
            let mut sps = Vec::new();
            sps.try_reserve_exact(sps_size).map_err(|e| e.to_string())?;
            sps.resize(sps_size, 0);
            reader.read_into(avcc, 8, &mut sps)?;
            let timeline = timing::read(&mut reader, movie, track, media, samples)?;
            video = Some((width, height, timeline, sps, sps_storage));
        } else {
            if audio.is_some() {
                return Err("Vita movie profile supports at most one AAC audio track".into());
            }
            if entry.kind != *b"mp4a" {
                return Err("Vita movie profile requires AAC audio".into());
            }
            audio = Some(reader.aac(entry)?);
        }
    }
    let (width, height, timeline, sps, sps_storage) =
        video.ok_or("MP4 has no H.264 video track")?;
    Ok(Metadata {
        info: Info {
            width,
            height,
            duration: timeline.duration,
            frame_duration: timeline.frame_duration,
            final_picture_time: timeline.final_picture_time,
            audio_sample_rate: audio.map(|(_, rate, _)| rate),
            audio_channels: audio.map_or(0, |(channels, _, _)| channels),
        },
        audio_frame_samples: audio.map_or(0, |(_, _, samples)| samples),
        sps,
        sps_storage,
    })
}
