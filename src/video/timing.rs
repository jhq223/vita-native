//! Stream timing runs and edit entries without retaining complete MP4 tables.
//! Two 4 KiB run pages avoid alternating 64 KiB source read-ahead for each VFR
//! or composition entry. Work is proportional to the actual timing records.
use super::metadata::{Atom, Reader};
use std::{
    format,
    io::{Read, Seek},
    prelude::v1::*,
};

pub(super) struct Timeline {
    pub duration: f64,
    pub frame_duration: f64,
    pub final_picture_time: f64,
}

struct Runs {
    atom: Atom,
    count: u32,
    index: u32,
    cached: u32,
    signed: bool,
    page: [u8; 4096],
}
impl Runs {
    fn new<R: Read + Seek + ?Sized>(
        reader: &mut Reader<'_, R>,
        atom: Atom,
        composition: bool,
    ) -> Result<Self, String> {
        let version = reader.bytes::<1>(atom, 0)?[0];
        if version > u8::from(composition) {
            return Err(format!(
                "unsupported MP4 {} version {version}",
                String::from_utf8_lossy(&atom.kind)
            ));
        }
        let count = u32::from_be_bytes(reader.bytes(atom, 4)?);
        if u64::from(count)
            .checked_mul(8)
            .and_then(|n| n.checked_add(8))
            .is_none_or(|n| n > atom.end - atom.start)
        {
            return Err(format!(
                "MP4 {} entry count exceeds its timing table",
                String::from_utf8_lossy(&atom.kind)
            ));
        }
        Ok(Self {
            atom,
            count,
            index: 0,
            cached: 0,
            signed: version == 1,
            page: [0; 4096],
        })
    }
    fn next<R: Read + Seek + ?Sized>(
        &mut self,
        reader: &mut Reader<'_, R>,
    ) -> Result<Option<(u64, i64)>, String> {
        if self.index == self.count {
            return Ok(None);
        }
        let in_page = self.index % 512;
        if in_page == 0 {
            self.cached = (self.count - self.index).min(512);
            reader.read_into(
                self.atom,
                8 + u64::from(self.index) * 8,
                &mut self.page[..self.cached as usize * 8],
            )?;
        }
        let at = in_page as usize * 8;
        let count = u64::from(u32::from_be_bytes(
            self.page[at..at + 4].try_into().unwrap(),
        ));
        let raw: [u8; 4] = self.page[at + 4..at + 8].try_into().unwrap();
        let value = if self.signed {
            i64::from(i32::from_be_bytes(raw))
        } else {
            i64::from(u32::from_be_bytes(raw))
        };
        self.index += 1;
        if count == 0 {
            return Err(format!(
                "MP4 {} has a zero-length timing run",
                String::from_utf8_lossy(&self.atom.kind)
            ));
        }
        Ok(Some((count, value)))
    }
}

#[derive(Clone, Copy)]
struct Range {
    start: i128,
    end_numerator: i128,
    scale: i128,
}
struct Summary {
    frames: u64,
    ticks: u64,
    last: Option<i128>,
}

fn scan<R: Read + Seek + ?Sized>(
    reader: &mut Reader<'_, R>,
    stts: Atom,
    ctts: Option<Atom>,
    range: Option<Range>,
) -> Result<Summary, String> {
    let mut decode = Runs::new(reader, stts, false)?;
    let mut composition = ctts.map(|atom| Runs::new(reader, atom, true)).transpose()?;
    let (mut composition_left, mut offset) = (0_u64, 0_i64);
    let mut result = Summary {
        frames: 0,
        ticks: 0,
        last: None,
    };
    while let Some((mut left, delta)) = decode.next(reader)? {
        if delta <= 0 {
            return Err("MP4 stts duration must be positive".into());
        }
        let delta = delta as u64;
        while left != 0 {
            if let Some(composition) = &mut composition
                && composition_left == 0
            {
                let next = composition
                    .next(reader)?
                    .ok_or("MP4 ctts covers fewer samples than stts")?;
                (composition_left, offset) = next;
            }
            let count = if composition.is_some() {
                left.min(composition_left)
            } else {
                left
            };
            let first = i128::from(result.ticks) + i128::from(offset);
            let mut index = Some(i128::from(count - 1));
            if let Some(range) = range {
                let ceiling = range.end_numerator - 1 - first * range.scale;
                index = (ceiling >= 0).then(|| {
                    (ceiling / (i128::from(delta) * range.scale)).min(i128::from(count - 1))
                });
            }
            if let Some(index) = index {
                let pts = first + index * i128::from(delta);
                if range.is_none_or(|range| pts + i128::from(delta) > range.start) {
                    result.last = Some(result.last.map_or(pts, |last| last.max(pts)));
                }
            }
            result.frames = result
                .frames
                .checked_add(count)
                .ok_or("MP4 sample count overflow")?;
            result.ticks = result
                .ticks
                .checked_add(count.checked_mul(delta).ok_or("MP4 timing overflow")?)
                .ok_or("MP4 timing overflow")?;
            left -= count;
            if composition.is_some() {
                composition_left -= count;
            }
        }
    }
    if result.frames == 0 {
        return Err("MP4 video timing table is empty".into());
    }
    if let Some(composition) = &mut composition
        && (composition_left != 0 || composition.next(reader)?.is_some())
    {
        return Err("MP4 ctts covers more samples than stts".into());
    }
    Ok(result)
}

fn clock<R: Read + Seek + ?Sized>(
    reader: &mut Reader<'_, R>,
    header: Atom,
) -> Result<(u32, u64), String> {
    let value = match reader.bytes::<1>(header, 0)?[0] {
        0 => (
            u32::from_be_bytes(reader.bytes(header, 12)?),
            u64::from(u32::from_be_bytes(reader.bytes(header, 16)?)),
        ),
        1 => (
            u32::from_be_bytes(reader.bytes(header, 20)?),
            u64::from_be_bytes(reader.bytes(header, 24)?),
        ),
        _ => {
            return Err(format!(
                "unsupported MP4 {} version",
                String::from_utf8_lossy(&header.kind)
            ));
        }
    };
    if value.0 == 0 || value.1 == 0 {
        return Err("MP4 media timescale and duration must be positive".into());
    }
    Ok(value)
}

pub(super) fn read<R: Read + Seek + ?Sized>(
    reader: &mut Reader<'_, R>,
    movie: Atom,
    track: Atom,
    media: Atom,
    samples: Atom,
) -> Result<Timeline, String> {
    let header = reader.find(media, b"mdhd", 0)?;
    let (rate, ticks) = clock(reader, header)?;
    let stts = reader.find(samples, b"stts", 0)?;
    let ctts = reader.optional(samples, b"ctts")?;
    let summary = scan(reader, stts, ctts, None)?;
    let final_picture_time = if let Some(edits) = reader.optional(track, b"edts")? {
        let edits = reader.find(edits, b"elst", 0)?;
        let movie_header = reader.find(movie, b"mvhd", 0)?;
        let (movie_rate, _) = clock(reader, movie_header)?;
        let version = reader.bytes::<1>(edits, 0)?[0];
        let entry_bytes = match version {
            0 => 12,
            1 => 20,
            _ => return Err("unsupported MP4 elst version".into()),
        };
        let count = u32::from_be_bytes(reader.bytes(edits, 4)?);
        if u64::from(count)
            .checked_mul(entry_bytes)
            .and_then(|n| n.checked_add(8))
            .is_none_or(|n| n > edits.end - edits.start)
        {
            return Err("MP4 elst entry count exceeds its edit table".into());
        }
        let (mut cursor, mut final_time) = (0_u64, None::<f64>);
        for index in 0..count {
            let at = 8 + u64::from(index) * entry_bytes;
            let (duration, start, rate_at) = if version == 0 {
                (
                    u64::from(u32::from_be_bytes(reader.bytes(edits, at)?)),
                    i64::from(i32::from_be_bytes(reader.bytes(edits, at + 4)?)),
                    at + 8,
                )
            } else {
                (
                    u64::from_be_bytes(reader.bytes(edits, at)?),
                    i64::from_be_bytes(reader.bytes(edits, at + 8)?),
                    at + 16,
                )
            };
            if reader.bytes::<4>(edits, rate_at)? != [0, 1, 0, 0] {
                return Err("Vita MP4 edits require a media rate of 1.0".into());
            }
            if start < -1 {
                return Err("MP4 elst contains an invalid negative media time".into());
            }
            if start != -1 && duration != 0 {
                let range = Range {
                    start: i128::from(start),
                    end_numerator: i128::from(start) * i128::from(movie_rate)
                        + i128::from(duration) * i128::from(rate),
                    scale: i128::from(movie_rate),
                };
                if let Some(pts) = scan(reader, stts, ctts, Some(range))?.last {
                    let mapped = (cursor as f64 / f64::from(movie_rate)
                        + (pts - i128::from(start)) as f64 / f64::from(rate))
                    .max(cursor as f64 / f64::from(movie_rate));
                    final_time = Some(final_time.map_or(mapped, |last| last.max(mapped)));
                }
            }
            cursor = cursor
                .checked_add(duration)
                .ok_or("MP4 edit timeline overflow")?;
        }
        final_time.ok_or("MP4 edits select no video picture")?
    } else {
        summary.last.ok_or("MP4 has no presentation timestamp")? as f64 / f64::from(rate)
    };
    if final_picture_time < 0. {
        return Err("MP4 video presentation ends before movie time zero".into());
    }
    Ok(Timeline {
        duration: ticks as f64 / f64::from(rate),
        frame_duration: summary.ticks as f64 / summary.frames as f64 / f64::from(rate),
        final_picture_time,
    })
}
