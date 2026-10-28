//! Contiguous F32 ranges in the GXP default uniform buffer.

pub(super) fn contiguous_f32(offset: u32, width: u32, count: u32, buffer_words: u32) -> bool {
    (1..=4).contains(&width)
        && count != 0
        && (count == 1 || width != 3)
        && width
            .checked_mul(count)
            .and_then(|n| offset.checked_add(n))
            .is_some_and(|end| end <= buffer_words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_and_even_vector_arrays_fit_the_reflected_word_range() {
        for width in 1..=4 {
            assert!(contiguous_f32(3, width, 1, 7));
            assert!(!contiguous_f32(3, width, 1, 3 + width - 1));
        }
        for width in [1, 2, 4] {
            assert!(contiguous_f32(4, width, 16, 4 + width * 16));
        }
        assert!(!contiguous_f32(0, 3, 2, 8));
        assert!(!contiguous_f32(0, 0, 1, 8));
        assert!(!contiguous_f32(0, 4, 0, 8));
        assert!(!contiguous_f32(u32::MAX, 4, 1, u32::MAX));
        assert!(!contiguous_f32(0, 4, u32::MAX, u32::MAX));
    }
}
