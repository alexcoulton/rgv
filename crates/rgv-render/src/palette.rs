//! Shared colour tables for the alignment renderers and the software export.
//!
//! Every closed category (bases, pair orientation, pair classification, specialist read bodies)
//! has an explicit table here so that the GPU scene, the summary shader and the CPU export agree
//! and so that no two members of a category can collapse onto near-identical hues. Only
//! open-ended categories (read group, haplotype, arbitrary tag values, mate contig) are hashed.
//!
//! All values are non-linear sRGB components, written unchanged into an `*Unorm` (non-sRGB)
//! colour target; see the surface-format policy in the desktop viewer runtime.

/// Index of each nucleotide in [`BASE_COLORS`]; also the allele token used by the summary shader.
pub const BASE_A: usize = 0;
pub const BASE_C: usize = 1;
pub const BASE_G: usize = 2;
pub const BASE_T: usize = 3;
pub const BASE_N: usize = 4;

/// The one nucleotide palette (A, C, G, T/U, anything else). These match the reference-sequence
/// track in `rgv-layout` and the `allele_color` constants in `alignment_summary.wgsl`; a test
/// compares the shader text against this table.
pub const BASE_COLORS: [[f32; 4]; 5] = [
    [0.24, 0.78, 0.38, 1.0],
    [0.22, 0.52, 0.96, 1.0],
    [0.96, 0.65, 0.18, 1.0],
    [0.94, 0.28, 0.28, 1.0],
    [0.60, 0.64, 0.70, 1.0],
];

#[must_use]
pub const fn base_color(base: u8) -> [f32; 4] {
    match base.to_ascii_uppercase() {
        b'A' => BASE_COLORS[BASE_A],
        b'C' => BASE_COLORS[BASE_C],
        b'G' => BASE_COLORS[BASE_G],
        b'T' | b'U' => BASE_COLORS[BASE_T],
        _ => BASE_COLORS[BASE_N],
    }
}

/// Body colour for a read whose category is unavailable or outside a closed table.
pub const UNCATEGORIZED_COLOR: [f32; 4] = [0.48, 0.48, 0.52, 1.0];

/// Pair-orientation palette indexed by the semantic page's orientation rank
/// (inward, outward, same strand, unknown). The hues follow the IGV convention: the expected
/// inward (LR) orientation stays quiet grey, outward (RL) pairs are green and same-strand
/// (LL/RR) pairs are blue-teal.
pub const PAIR_ORIENTATION_COLORS: [[f32; 4]; 4] = [
    [0.73, 0.73, 0.73, 1.0],
    [0.14, 0.62, 0.20, 1.0],
    [0.05, 0.50, 0.78, 1.0],
    [0.40, 0.32, 0.26, 1.0],
];

/// Pair-classification palette indexed by the semantic page's classification rank (proper,
/// concordant, short template, long template, discordant orientation, inter-contig, mate
/// unmapped, mate missing, read unmapped, unpaired). Template-length colouring uses the same
/// table. Following IGV, a shorter-than-expected template is blue and a longer one red.
pub const PAIR_CLASSIFICATION_COLORS: [[f32; 4]; 10] = [
    [0.73, 0.73, 0.73, 1.0],
    [0.50, 0.58, 0.76, 1.0],
    [0.16, 0.30, 0.85, 1.0],
    [0.85, 0.16, 0.16, 1.0],
    [0.05, 0.60, 0.60, 1.0],
    [0.60, 0.25, 0.75, 1.0],
    [0.93, 0.55, 0.10, 1.0],
    [0.72, 0.78, 0.15, 1.0],
    [0.25, 0.25, 0.28, 1.0],
    [0.55, 0.35, 0.20, 1.0],
];

/// Specialist read bodies indexed by `[mode][reverse]` (bisulfite, SMRT kinetics, Ultima, SBX).
/// The bodies stay pale so the specialist per-base overlays remain the dominant signal, while
/// forward (cool) and reverse (warm) strands of one mode are still clearly separable.
pub const SPECIALIST_BODY_COLORS: [[[f32; 4]; 2]; 4] = [
    [[0.60, 0.68, 0.82, 1.0], [0.84, 0.66, 0.60, 1.0]],
    [[0.58, 0.78, 0.68, 1.0], [0.80, 0.72, 0.50, 1.0]],
    [[0.70, 0.62, 0.86, 1.0], [0.90, 0.58, 0.66, 1.0]],
    [[0.52, 0.76, 0.86, 1.0], [0.86, 0.78, 0.62, 1.0]],
];

fn table_color<const N: usize>(table: &[[f32; 4]; N], rank: u8) -> [f32; 4] {
    table
        .get(usize::from(rank))
        .copied()
        .unwrap_or(UNCATEGORIZED_COLOR)
}

#[must_use]
pub fn pair_orientation_color(rank: u8) -> [f32; 4] {
    table_color(&PAIR_ORIENTATION_COLORS, rank)
}

#[must_use]
pub fn pair_classification_color(rank: u8) -> [f32; 4] {
    table_color(&PAIR_CLASSIFICATION_COLORS, rank)
}

#[must_use]
pub fn specialist_body_color(mode: u8, reverse: bool) -> [f32; 4] {
    SPECIALIST_BODY_COLORS
        .get(usize::from(mode))
        .map_or(UNCATEGORIZED_COLOR, |strands| strands[usize::from(reverse)])
}

/// Colour for an open-ended category. The hue advances by the golden-ratio conjugate per unit of
/// `value` (Fibonacci hashing), so adjacent small integers such as haplotype 1 and 2 land far
/// apart on the hue circle instead of one degree apart, and well-mixed fingerprints stay evenly
/// spread.
#[must_use]
pub fn categorical_hash_color(value: u64) -> [f32; 4] {
    /// 2^64 divided by the golden ratio.
    const GOLDEN_RATIO_STEP: u64 = 0x9E37_79B9_7F4A_7C15;
    let mixed = value.wrapping_mul(GOLDEN_RATIO_STEP);
    // The top 24 bits are exactly representable in an f32.
    let hue = (mixed >> 40) as f32 / (1_u32 << 24) as f32;
    let sector = hue * 6.0;
    let fraction = sector.fract();
    let (r, g, b) = match sector as u32 % 6 {
        0 => (1.0, fraction, 0.0),
        1 => (1.0 - fraction, 1.0, 0.0),
        2 => (0.0, 1.0, fraction),
        3 => (0.0, 1.0 - fraction, 1.0),
        4 => (fraction, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - fraction),
    };
    [0.25 + r * 0.50, 0.25 + g * 0.50, 0.25 + b * 0.50, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distance(left: [f32; 4], right: [f32; 4]) -> f32 {
        left.iter()
            .zip(right)
            .take(3)
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f32>()
            .sqrt()
    }

    fn assert_pairwise_distinct(name: &str, colors: &[[f32; 4]], minimum: f32) {
        for (left_index, left) in colors.iter().enumerate() {
            for (right_index, right) in colors.iter().enumerate().skip(left_index + 1) {
                let separation = distance(*left, *right);
                assert!(
                    separation >= minimum,
                    "{name} entries {left_index} and {right_index} are only {separation} apart"
                );
            }
        }
    }

    #[test]
    fn closed_category_palettes_are_pairwise_clearly_distinct() {
        assert_pairwise_distinct("base", &BASE_COLORS, 0.25);
        let orientation = (0..4).map(pair_orientation_color).collect::<Vec<_>>();
        assert_pairwise_distinct("pair orientation", &orientation, 0.25);
        let classification = (0..10).map(pair_classification_color).collect::<Vec<_>>();
        assert_pairwise_distinct("pair classification", &classification, 0.25);
    }

    #[test]
    fn specialist_bodies_separate_strands_and_modes() {
        let mut all = Vec::new();
        for mode in 0..4 {
            let forward = specialist_body_color(mode, false);
            let reverse = specialist_body_color(mode, true);
            assert!(
                distance(forward, reverse) >= 0.25,
                "specialist mode {mode} strands are too similar"
            );
            all.extend([forward, reverse]);
        }
        assert_pairwise_distinct("specialist body", &all, 0.10);
    }

    fn assert_same(left: [f32; 4], right: [f32; 4]) {
        assert!(
            distance(left, right) < 1.0e-6 && (left[3] - right[3]).abs() < 1.0e-6,
            "{left:?} != {right:?}"
        );
    }

    #[test]
    fn out_of_range_ranks_fall_back_to_the_uncategorized_colour() {
        assert_same(pair_orientation_color(4), UNCATEGORIZED_COLOR);
        assert_same(pair_classification_color(10), UNCATEGORIZED_COLOR);
        assert_same(specialist_body_color(4, true), UNCATEGORIZED_COLOR);
    }

    #[test]
    fn hashed_categories_spread_adjacent_small_integers() {
        for value in 0..64_u64 {
            let separation = distance(
                categorical_hash_color(value),
                categorical_hash_color(value + 1),
            );
            assert!(
                separation >= 0.30,
                "categories {value} and {} are only {separation} apart",
                value + 1
            );
        }
        // The first handful of categories (typical haplotype or read-group counts) are all
        // mutually separable, not merely neighbour-separable.
        let first = (0..6).map(categorical_hash_color).collect::<Vec<_>>();
        assert_pairwise_distinct("hashed category", &first, 0.20);
        for color in (0..1024).map(categorical_hash_color) {
            assert!(color.iter().all(|channel| (0.0..=1.0).contains(channel)));
        }
    }

    #[test]
    fn base_lookup_is_case_insensitive_and_total() {
        assert_same(base_color(b'a'), BASE_COLORS[BASE_A]);
        assert_same(base_color(b'C'), BASE_COLORS[BASE_C]);
        assert_same(base_color(b'g'), BASE_COLORS[BASE_G]);
        assert_same(base_color(b'U'), BASE_COLORS[BASE_T]);
        assert_same(base_color(b'N'), BASE_COLORS[BASE_N]);
        assert_same(base_color(b'-'), BASE_COLORS[BASE_N]);
    }

    /// Extracts the `vec4<f32>(...)` literals of `allele_color` from the summary shader text.
    fn shader_allele_colors(shader: &str) -> Vec<[f32; 4]> {
        let body = shader
            .split("fn allele_color(")
            .nth(1)
            .expect("summary shader defines allele_color")
            .split("\n}\n")
            .next()
            .unwrap();
        body.split("vec4<f32>(")
            .skip(1)
            .map(|literal| {
                let values = literal
                    .split(')')
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|value| value.trim().parse::<f32>().unwrap())
                    .collect::<Vec<_>>();
                <[f32; 4]>::try_from(values).unwrap()
            })
            .collect()
    }

    #[test]
    fn summary_shader_allele_constants_match_the_shared_base_table() {
        let shader = include_str!("alignment_summary.wgsl");
        let shader_colors = shader_allele_colors(shader);
        assert_eq!(shader_colors.len(), BASE_COLORS.len());
        for (shader_color, table_color) in shader_colors.into_iter().zip(BASE_COLORS) {
            assert_same(shader_color, table_color);
        }
    }
}
