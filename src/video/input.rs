//! Complete SDK reads while preserving bounded source read-ahead.
use std::io::{self, BufReader, Read, Seek, SeekFrom};

pub(super) fn read_at<R: Read + Seek>(
    source: &mut BufReader<R>,
    position: u64,
    output: &mut [u8],
) -> io::Result<usize> {
    let current = source.stream_position()?;
    if let Ok(delta) = i64::try_from(i128::from(position) - i128::from(current)) {
        source.seek_relative(delta)?;
    } else {
        source.seek(SeekFrom::Start(position))?;
    }
    let mut done = 0;
    while done < output.len() {
        match source.read(&mut output[done..]) {
            Ok(0) => break,
            Ok(count) => done += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(done)
}
