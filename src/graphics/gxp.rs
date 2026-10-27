//! SDK-produced GXP files may include up to three trailing alignment bytes.
use super::{Error, types::Result};
pub(super) fn payload(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.len() < 0x9c || bytes.get(..4) != Some(b"GXP\0") {
        return Err(Error::Invalid("GXP header"));
    }
    let declared = u32::from_le_bytes(bytes[8..12].try_into().expect("header")) as usize;
    if declared < 0x9c || declared > bytes.len() || bytes.len() > declared.next_multiple_of(4) {
        return Err(Error::Invalid("GXP declared length or compiler padding"));
    }
    Ok(&bytes[..declared])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_official_compiler_tail_padding_and_rejects_truncation() {
        for declared in [251usize, 345, 373, 376] {
            let mut data = std::vec![0u8;declared.next_multiple_of(4)];
            data[..4].copy_from_slice(b"GXP\0");
            data[8..12].copy_from_slice(&(declared as u32).to_le_bytes());
            assert_eq!(payload(&data).unwrap().len(), declared);
            assert_eq!(payload(&data[..declared]).unwrap().len(), declared);
            assert!(payload(&data[..declared - 1]).is_err());
            data.extend([0, 0, 0, 0]);
            assert!(payload(&data).is_err());
        }
    }
}
