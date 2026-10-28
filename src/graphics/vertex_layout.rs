//! Pure validation for indexed and per-instance f32 streams.
use super::types::{VertexAttribute, VertexLayout};
use std::{ops::Range, prelude::v1::*};

pub(super) fn valid(stride: u32, attributes: &[VertexAttribute]) -> bool {
    stride > 0
        && stride <= 16383
        && attributes.iter().enumerate().all(|(index, a)| {
            a.location < 16
                && (1..=4).contains(&a.components)
                && a.offset
                    .checked_add(a.components)
                    .is_some_and(|end| end <= stride)
                && attributes[..index]
                    .iter()
                    .all(|other| other.location != a.location)
        })
}
pub(super) fn compatible(attributes: &[VertexAttribute], instance: VertexLayout<'_>) -> bool {
    valid(instance.stride, instance.attributes)
        && instance.attributes.iter().all(|input| {
            attributes
                .iter()
                .all(|vertex| vertex.location != input.location)
        })
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InstanceDraw {
    pub offset: usize,
    pub count: u32,
    pub wrap: u32,
}
pub(super) fn instance_draw(
    range: &Range<u32>,
    capacity: usize,
    stride: u32,
    indices: u32,
) -> Option<InstanceDraw> {
    let count = range.end.checked_sub(range.start)?;
    if range.end as usize > capacity || count > u32::from(u16::MAX) || !indices.is_multiple_of(3) {
        return None;
    }
    Some(InstanceDraw {
        offset: (range.start as usize)
            .checked_mul(stride as usize)?
            .checked_mul(4)?,
        count: indices.checked_mul(count)?,
        wrap: indices,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streams_have_distinct_locations_and_checked_record_extents() {
        let base = [VertexAttribute {
            location: 0,
            components: 2,
            offset: 0,
        }];
        let records = [VertexAttribute {
            location: 1,
            components: 4,
            offset: 0,
        }];
        assert!(valid(2, &base));
        assert!(compatible(
            &base,
            VertexLayout {
                stride: 4,
                attributes: &records
            }
        ));
        assert!(!compatible(
            &base,
            VertexLayout {
                stride: 2,
                attributes: &base
            }
        ));
        assert!(!valid(2, &[base[0], base[0]]));
        assert!(!valid(3, &records));
        assert!(!valid(16384, &records));
    }
    #[test]
    fn instance_ranges_offset_the_stream_but_wrap_only_the_base_mesh() {
        assert_eq!(
            instance_draw(&(8..24), 32, 12, 6),
            Some(InstanceDraw {
                offset: 384,
                count: 96,
                wrap: 6
            })
        );
        assert!(instance_draw(&(8..33), 32, 12, 6).is_none());
        assert!(instance_draw(&(24..8), 32, 12, 6).is_none());
        assert!(instance_draw(&(0..65536), 65536, 12, 6).is_none());
        assert!(instance_draw(&(0..2), 2, 12, u32::MAX).is_none());
        assert!(instance_draw(&(0..2), 2, 12, 5).is_none());
    }
}
