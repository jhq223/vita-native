//! Device-independent input, layout and bounded MP4 metadata checks.
#[path = "../src/app/memory.rs"]
pub mod memory;
mod app {
    pub use crate::memory;
}
#[path = "../src/video/info.rs"]
mod info;
pub use info::Info;
#[path = "../src/video/input.rs"]
mod input;
#[path = "../src/video/metadata.rs"]
mod metadata;
#[path = "../src/video/pcm.rs"]
mod pcm;
#[path = "../src/video/timing.rs"]
mod timing;
use std::{
    io::{self, BufReader, Cursor, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Budget {
    limit: u64,
    live: Arc<AtomicUsize>,
}
struct Lease {
    bytes: usize,
    live: Arc<AtomicUsize>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.live.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}
impl memory::MemoryBudget for Budget {
    fn reserve(&self, bytes: u64) -> Result<Box<dyn memory::MemoryReservation>, String> {
        if bytes > self.limit {
            return Err("test metadata budget exceeded".into());
        }
        self.live.fetch_add(bytes as usize, Ordering::Relaxed);
        Ok(Box::new(Lease {
            bytes: bytes as usize,
            live: self.live.clone(),
        }))
    }
}
fn budget(limit: u64) -> Budget {
    Budget {
        limit,
        live: Arc::new(AtomicUsize::new(0)),
    }
}

fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::from(((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}
fn container(kind: &[u8; 4], children: &[Vec<u8>]) -> Vec<u8> {
    atom(kind, &children.concat())
}
fn handler(kind: &[u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0; 8];
    bytes.extend_from_slice(kind);
    atom(b"hdlr", &bytes)
}
fn video_track(
    width: u16,
    height: u16,
    scale: u32,
    ticks: u32,
    tables: &[Vec<u8>],
    edits: Option<Vec<u8>>,
) -> Vec<u8> {
    let mut visual = vec![0; 78];
    visual[24..26].copy_from_slice(&width.to_be_bytes());
    visual[26..28].copy_from_slice(&height.to_be_bytes());
    let mut avcc = vec![1, 66, 0, 30, 0xff, 0xe1, 0, 4];
    avcc.extend_from_slice(&[0x67, 0x42, 0, 0x1e]);
    visual.extend(atom(b"avcC", &avcc));
    let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
    stsd.extend(atom(b"avc1", &visual));
    let mut children = vec![atom(b"stsd", &stsd)];
    children.extend_from_slice(tables);
    let samples = container(b"stbl", &children);
    let mut mdhd = vec![0; 24];
    mdhd[12..16].copy_from_slice(&scale.to_be_bytes());
    mdhd[16..20].copy_from_slice(&ticks.to_be_bytes());
    let mut track = vec![container(
        b"mdia",
        &[
            handler(b"vide"),
            atom(b"mdhd", &mdhd),
            container(b"minf", &[samples]),
        ],
    )];
    track.extend(edits);
    container(b"trak", &track)
}
fn movie(width: u16, height: u16) -> Vec<u8> {
    let mut timing = vec![0, 0, 0, 0, 0, 0, 0, 1];
    timing.extend_from_slice(&30_u32.to_be_bytes());
    timing.extend_from_slice(&1000_u32.to_be_bytes());
    let video = video_track(
        width,
        height,
        30_000,
        30_000,
        &[atom(b"stts", &timing)],
        None,
    );

    let specific = vec![5, 2, 0x12, 0x08]; // AAC-LC 44.1 kHz mono.
    let mut config = vec![0; 13];
    config[0] = 0x40;
    config.extend(specific);
    let mut descriptor = vec![4, config.len() as u8];
    descriptor.extend(config);
    let mut es = vec![0, 1, 0];
    es.extend(descriptor);
    let mut esds = vec![0; 4];
    esds.extend([3, es.len() as u8]);
    esds.extend(es);
    let mut audio_entry = vec![0; 28];
    audio_entry.extend(atom(b"esds", &esds));
    let mut audio_stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
    audio_stsd.extend(atom(b"mp4a", &audio_entry));
    let audio = container(
        b"trak",
        &[container(
            b"mdia",
            &[
                handler(b"soun"),
                container(
                    b"minf",
                    &[container(b"stbl", &[atom(b"stsd", &audio_stsd)])],
                ),
            ],
        )],
    );
    container(b"moov", &[video, audio])
}

#[test]
fn scalar_metadata_keeps_visible_size_timeline_and_aac_channels_with_exact_sps_ownership() {
    let data = movie(960, 540);
    let ledger = budget(65535);
    let parsed = metadata::read(
        &mut Cursor::new(&data),
        data.len() as u64,
        &AtomicBool::new(false),
        &ledger,
    )
    .unwrap();
    let metadata::Metadata {
        info,
        sps,
        sps_storage,
    } = parsed;
    assert_eq!((info.width, info.height), (960, 540));
    assert_eq!(info.duration, 1.);
    assert!((info.frame_duration - 1. / 30.).abs() < 1e-12);
    assert!((info.final_picture_time - 29. / 30.).abs() < 1e-12);
    assert_eq!(info.audio_sample_rate, Some(44100));
    assert_eq!(info.audio_channels, 1);
    assert_eq!(sps, [0x67, 0x42, 0, 0x1e]);
    assert_eq!(ledger.live.load(Ordering::Relaxed), sps.capacity());
    drop(sps);
    drop(sps_storage);
    assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
}

struct SparseMovie {
    header: [u8; 16],
    tail: Vec<u8>,
    tail_at: u64,
    at: u64,
    read: Arc<AtomicUsize>,
}
impl Read for SparseMovie {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let len = self.tail_at + self.tail.len() as u64;
        let count = output.len().min(len.saturating_sub(self.at) as usize);
        output[..count].fill(0);
        for (index, byte) in output[..count].iter_mut().enumerate() {
            let at = self.at + index as u64;
            if at < 16 {
                *byte = self.header[at as usize];
            } else if at >= self.tail_at {
                *byte = self.tail[(at - self.tail_at) as usize];
            }
        }
        self.at += count as u64;
        self.read.fetch_add(count, Ordering::Relaxed);
        Ok(count)
    }
}
impl Seek for SparseMovie {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let at = match to {
            SeekFrom::Start(at) => i128::from(at),
            SeekFrom::Current(by) => i128::from(self.at) + i128::from(by),
            SeekFrom::End(by) => i128::from(self.tail_at + self.tail.len() as u64) + i128::from(by),
        };
        self.at = u64::try_from(at).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(self.at)
    }
}
#[test]
fn large_media_payload_is_skipped_through_seekable_resource_reads() {
    let tail = movie(960, 544);
    let tail_at = 128_u64 << 20;
    let mut header = [0; 16];
    header[..4].copy_from_slice(&1_u32.to_be_bytes());
    header[4..8].copy_from_slice(b"mdat");
    header[8..].copy_from_slice(&tail_at.to_be_bytes());
    let length = tail_at + tail.len() as u64;
    let read = Arc::new(AtomicUsize::new(0));
    let source = SparseMovie {
        header,
        tail,
        tail_at,
        at: 0,
        read: read.clone(),
    };
    let mut source = BufReader::with_capacity(64 << 10, source);
    let parsed =
        metadata::read(&mut source, length, &AtomicBool::new(false), &budget(65535)).unwrap();
    assert_eq!(parsed.info.width, 960);
    assert!(read.load(Ordering::Relaxed) < 130 << 10);
}

#[test]
fn malformed_profile_fields_and_cancelled_metadata_preserve_budget() {
    for (width, height) in [(961, 540), (960, 545), (0, 2), (2, 1)] {
        let data = movie(width, height);
        let ledger = budget(65535);
        let error = metadata::read(
            &mut Cursor::new(&data),
            data.len() as u64,
            &AtomicBool::new(false),
            &ledger,
        )
        .err()
        .unwrap();
        assert!(error.contains("dimensions"), "{error}");
        assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
    }
    let data = movie(960, 540);
    assert!(
        metadata::read(
            &mut Cursor::new(&data),
            data.len() as u64,
            &AtomicBool::new(true),
            &budget(65535)
        )
        .is_err()
    );
    assert!(
        metadata::read(
            &mut Cursor::new(&data),
            data.len() as u64,
            &AtomicBool::new(false),
            &budget(3)
        )
        .is_err()
    );
    let mut truncated = data.clone();
    truncated.truncate(truncated.len() - 1);
    assert!(
        metadata::read(
            &mut Cursor::new(&truncated),
            truncated.len() as u64,
            &AtomicBool::new(false),
            &budget(65535)
        )
        .is_err()
    );
}

struct ShortReads {
    bytes: Cursor<Vec<u8>>,
    stopped: Arc<AtomicBool>,
    calls: usize,
}
impl Read for ShortReads {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.calls == 1 {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if self.calls == 3 {
            self.stopped.store(true, Ordering::Release);
        }
        let len = out.len().min(7);
        self.bytes.read(&mut out[..len])
    }
}
impl Seek for ShortReads {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.bytes.seek(to)
    }
}
#[test]
fn sdk_read_completes_short_requests_during_transport_cancellation_and_reports_eof() {
    let stopped = Arc::new(AtomicBool::new(false));
    let source = ShortReads {
        bytes: Cursor::new((0..128).collect()),
        stopped: stopped.clone(),
        calls: 0,
    };
    let mut reader = BufReader::with_capacity(8, source);
    let mut out = [0; 96];
    assert_eq!(input::read_at(&mut reader, 11, &mut out).unwrap(), 96);
    assert_eq!(out, std::array::from_fn(|index| index as u8 + 11));
    assert!(stopped.load(Ordering::Acquire));
    assert_eq!(input::read_at(&mut reader, 120, &mut out).unwrap(), 8);
    assert_eq!(&out[..8], &[120, 121, 122, 123, 124, 125, 126, 127]);
    assert_eq!(input::read_at(&mut reader, 128, &mut out).unwrap(), 0);
}

fn timed_movie(
    decode: &[(u32, u32)],
    composition: Option<(u8, &[(u32, i64)])>,
    edits: Option<(u8, &[(u64, i64)])>,
) -> Vec<u8> {
    let mut stts = vec![0; 4];
    stts.extend_from_slice(&(decode.len() as u32).to_be_bytes());
    for &(count, delta) in decode {
        stts.extend_from_slice(&count.to_be_bytes());
        stts.extend_from_slice(&delta.to_be_bytes());
    }
    let mut tables = vec![atom(b"stts", &stts)];
    if let Some((version, entries)) = composition {
        let mut ctts = vec![version, 0, 0, 0];
        ctts.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for &(count, offset) in entries {
            ctts.extend_from_slice(&count.to_be_bytes());
            let offset = if version == 0 {
                u32::try_from(offset).unwrap().to_be_bytes()
            } else {
                i32::try_from(offset).unwrap().to_be_bytes()
            };
            ctts.extend_from_slice(&offset);
        }
        tables.push(atom(b"ctts", &ctts));
    }
    let edits = edits.map(|(version, entries)| {
        let mut elst = vec![version, 0, 0, 0];
        elst.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for &(duration, start) in entries {
            if version == 0 {
                elst.extend_from_slice(&u32::try_from(duration).unwrap().to_be_bytes());
                elst.extend_from_slice(&i32::try_from(start).unwrap().to_be_bytes());
            } else {
                elst.extend_from_slice(&duration.to_be_bytes());
                elst.extend_from_slice(&start.to_be_bytes());
            }
            elst.extend_from_slice(&[0, 1, 0, 0]);
        }
        container(b"edts", &[atom(b"elst", &elst)])
    });
    let ticks = decode
        .iter()
        .map(|&(count, delta)| count * delta)
        .sum::<u32>()
        .max(1);
    let mut mvhd = vec![0; 24];
    mvhd[12..16].copy_from_slice(&1000_u32.to_be_bytes());
    mvhd[16..20].copy_from_slice(&10_000_u32.to_be_bytes());
    container(
        b"moov",
        &[
            atom(b"mvhd", &mvhd),
            video_track(960, 544, 1000, ticks, &tables, edits),
        ],
    )
}
fn read_timing(data: &[u8]) -> Result<Info, String> {
    metadata::read(
        &mut Cursor::new(data),
        data.len() as u64,
        &AtomicBool::new(false),
        &budget(65535),
    )
    .map(|parsed| parsed.info)
}
#[test]
fn composition_order_uses_final_presentation_and_signed_version_one_offsets() {
    let signed = timed_movie(&[(4, 40)], Some((1, &[(1, 0), (1, 80), (2, -40)])), None);
    let info = read_timing(&signed).unwrap();
    // Decode order is 0, 120, 40, 80 ms. The final displayed picture is not the
    // last decoded sample; signed negative offsets must not become u32 values.
    assert!((info.final_picture_time - 0.120).abs() < 1e-12);
    let unsigned = timed_movie(
        &[(2, 40), (2, 20)],
        Some((0, &[(1, 0), (1, 80), (2, 0)])),
        None,
    );
    let info = read_timing(&unsigned).unwrap();
    assert!((info.final_picture_time - 0.120).abs() < 1e-12);
    assert!((info.frame_duration - 0.030).abs() < 1e-12);
    // Version zero is explicitly unsigned, including offsets above i32::MAX.
    let unsigned = timed_movie(&[(1, 40)], Some((0, &[(1, 1_i64 << 31)])), None);
    assert!(
        (read_timing(&unsigned).unwrap().final_picture_time - (1_u64 << 31) as f64 / 1000.).abs()
            < 1e-9
    );
}
#[test]
fn edit_lists_map_composition_into_movie_clock_and_ignore_trailing_empty_edits() {
    for version in [0, 1] {
        let data = timed_movie(
            &[(6, 40)],
            None,
            Some((version, &[(500, -1), (80, 80), (1000, -1)])),
        );
        let info = read_timing(&data).unwrap();
        // The selected media frames at 80 and 120 ms map to absolute movie
        // times 500 and 540 ms; a native stream origin is subtracted only when
        // publishing a copied picture, never when matching the SDK timestamp.
        assert!((info.final_picture_time - 0.540).abs() < 1e-12);
        let crossing = timed_movie(&[(6, 40)], None, Some((version, &[(10, 85)])));
        assert_eq!(read_timing(&crossing).unwrap().final_picture_time, 0.);
    }
}
#[test]
fn inconsistent_or_unsupported_timing_is_rejected_at_metadata_admission() {
    for (data, expected) in [
        (
            timed_movie(&[(4, 40)], Some((1, &[(3, 0)])), None),
            "fewer samples",
        ),
        (
            timed_movie(&[(4, 40)], Some((1, &[(5, 0)])), None),
            "more samples",
        ),
        (
            timed_movie(&[(4, 40)], Some((2, &[(4, 0)])), None),
            "version 2",
        ),
        (timed_movie(&[(0, 40)], None, None), "zero-length"),
        (timed_movie(&[(4, 0)], None, None), "must be positive"),
        (
            timed_movie(&[(4, 40)], Some((1, &[(0, 0)])), None),
            "zero-length",
        ),
        (
            timed_movie(&[(4, 40)], None, Some((0, &[(40, -2)]))),
            "negative media time",
        ),
    ] {
        let error = read_timing(&data).unwrap_err();
        assert!(
            error.contains(expected),
            "expected {expected}, received {error}"
        );
    }
}
struct CountedReads {
    source: Cursor<Vec<u8>>,
    bytes: usize,
}
impl Read for CountedReads {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = self.source.read(out)?;
        self.bytes += count;
        Ok(count)
    }
}
impl Seek for CountedReads {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.source.seek(to)
    }
}
#[test]
fn large_timing_tables_stream_across_run_pages_with_bounded_reads_and_ownership() {
    let decode: Vec<_> = (0..20_001).map(|index| (1, 30 + index % 2 * 10)).collect();
    let composition: Vec<_> = (0..20_001)
        .map(|index| (1, i64::from(index % 2) * 10))
        .collect();
    let data = timed_movie(&decode, Some((1, &composition)), None);
    let length = data.len();
    let expected = (decode
        .iter()
        .map(|&(_, delta)| u64::from(delta))
        .sum::<u64>()
        - 30) as f64
        / 1000.;
    let ledger = budget(4);
    let mut source = CountedReads {
        source: Cursor::new(data),
        bytes: 0,
    };
    let parsed =
        metadata::read(&mut source, length as u64, &AtomicBool::new(false), &ledger).unwrap();
    assert!((parsed.info.final_picture_time - expected).abs() < 1e-9);
    assert!(
        source.bytes < length + 4096,
        "{} bytes read for {length} bytes of metadata",
        source.bytes
    );
    assert_eq!(ledger.live.load(Ordering::Relaxed), 4);
    drop(parsed);
    assert_eq!(ledger.live.load(Ordering::Relaxed), 0);
}
