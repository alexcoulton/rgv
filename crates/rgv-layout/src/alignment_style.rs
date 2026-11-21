use crate::Rgba;
use rgv_model::ModificationCode;

pub const MAX_HIDDEN_MODIFICATION_CODES: usize = 8;

#[must_use]
pub fn alignment_read_name_hash(name: &[u8]) -> u64 {
    name.iter().fold(0xcbf_29ce_4842_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[must_use]
pub fn modification_code_color(code: ModificationCode) -> Rgba {
    match code {
        ModificationCode::Letter(b'm') => Rgba::new(0.86, 0.20, 0.58, 1.0),
        ModificationCode::Letter(b'h') => Rgba::new(0.12, 0.62, 0.70, 1.0),
        ModificationCode::Letter(b'a') => Rgba::new(0.92, 0.52, 0.10, 1.0),
        ModificationCode::Letter(b'f') => Rgba::new(0.48, 0.32, 0.80, 1.0),
        ModificationCode::Letter(letter) => palette_color(&char::from(letter).to_string(), 1.0),
        ModificationCode::Chebi(code) => palette_color(&format!("CHEBI:{code}"), 1.0),
    }
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

fn palette_color(label: &str, alpha: f32) -> Rgba {
    const PALETTE: [[f32; 3]; 8] = [
        [0.26, 0.70, 0.94],
        [0.94, 0.40, 0.42],
        [0.42, 0.82, 0.48],
        [0.80, 0.48, 0.94],
        [0.96, 0.68, 0.24],
        [0.24, 0.78, 0.76],
        [0.94, 0.42, 0.74],
        [0.66, 0.72, 0.30],
    ];
    let hash = label.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let color = PALETTE[hash as usize % PALETTE.len()];
    Rgba::new(color[0], color[1], color[2], alpha)
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
