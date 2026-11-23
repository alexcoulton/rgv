use crate::Rgba;

pub const MAX_HIDDEN_MODIFICATION_CODES: usize = 8;

#[must_use]
pub fn alignment_read_name_hash(name: &[u8]) -> u64 {
    name.iter().fold(0xcbf_29ce_4842_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// IGV's 44-entry Ultima jet scale, reproduced with the same integer arithmetic.
#[must_use]
pub fn ultima_quality_color(quality: f32) -> Rgba {
    const SIZE: usize = 44;
    let index = quality.clamp(0.0, (SIZE - 1) as f32) as usize;
    let green = ultima_jet_green(index);
    let blue = if index >= SIZE * 7 / 8 {
        0
    } else {
        ultima_jet_green((index + SIZE / 4) % SIZE)
    };
    let red = if index < SIZE / 8 {
        0
    } else {
        ultima_jet_green((index + SIZE * 6 / 8) % SIZE)
    };
    Rgba::new(
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
        1.0,
    )
}

fn ultima_jet_green(index: usize) -> u8 {
    const SIZE: usize = 44;
    if !(SIZE / 8..SIZE * 7 / 8).contains(&index) {
        0
    } else if index < SIZE * 3 / 8 {
        u8::try_from(255 * (index - SIZE / 8) * 4 / SIZE).unwrap_or(u8::MAX)
    } else if index < SIZE * 5 / 8 {
        u8::MAX
    } else {
        u8::try_from(255 - 255 * (index - SIZE * 5 / 8) * 4 / SIZE).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_name_hash_is_stable_and_order_sensitive() {
        assert_eq!(
            alignment_read_name_hash(b"read-1"),
            alignment_read_name_hash(b"read-1")
        );
        assert_ne!(
            alignment_read_name_hash(b"read-1"),
            alignment_read_name_hash(b"1-read")
        );
    }

    #[test]
    fn ultima_scale_keeps_low_and_high_qualities_distinct() {
        assert_ne!(ultima_quality_color(0.0), ultima_quality_color(42.0));
    }
}
