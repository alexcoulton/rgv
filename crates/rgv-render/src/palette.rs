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

/// Full colours of the base-modification classes in one palette.
///
/// They follow IGV's convention (5mC red, unmodified blue) but are deliberately deeper than the
/// mismatch colours (T is a light red, C a light azure) and far from the insertion purple and
/// the selection blue, so a modification cell is never read as a mismatch, an insertion or a
/// selection. A test holds every palette to a minimum CIE76 difference from those colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModificationColors {
    /// No modification is the likelier state.
    pub unmodified: [f32; 4],
    pub five_methyl_c: [f32; 4],
    pub five_hydroxymethyl_c: [f32; 4],
    pub six_methyl_a: [f32; 4],
    pub other: [f32; 4],
}

/// Alpha of the faint end of every modification ramp: a call whose two states are equally
/// likely is a barely visible tint on its read.
pub const MODIFICATION_FAINT_ALPHA: f32 = 0.15;

/// The Light and Dark palettes' modification colours.
pub const MODIFICATION_COLORS: ModificationColors = ModificationColors {
    unmodified: [0.05, 0.05, 0.85, 1.0],
    five_methyl_c: [0.62, 0.00, 0.08, 1.0],
    // Dark amber: the light orange is the mismatch colour of G.
    five_hydroxymethyl_c: [0.58, 0.38, 0.00, 1.0],
    // Teal: the light green is the mismatch colour of A.
    six_methyl_a: [0.00, 0.55, 0.60, 1.0],
    // Pale cream: far from every base, the insertion purple and the other classes.
    other: [0.95, 1.00, 0.70, 1.0],
};

/// The Okabe-Ito palettes give G black (light) or yellow (dark) and T vermillion, so 5hmC moves
/// to whichever of yellow and amber stays clear of both under simulated dichromacy.
pub const LIGHT_COLOR_BLIND_MODIFICATION_COLORS: ModificationColors = ModificationColors {
    five_hydroxymethyl_c: [0.94, 0.80, 0.10, 1.0],
    ..MODIFICATION_COLORS
};

pub const DARK_COLOR_BLIND_MODIFICATION_COLORS: ModificationColors = ModificationColors {
    five_hydroxymethyl_c: [0.90, 0.62, 0.00, 1.0],
    six_methyl_a: [0.00, 0.62, 0.68, 1.0],
    ..MODIFICATION_COLORS
};

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

// ---------------------------------------------------------------------------------------------
// Semantic palette
// ---------------------------------------------------------------------------------------------

/// Number of entries in a GPU palette. Entry 0 is never read: token 0 means "inline colour".
pub const PALETTE_ENTRIES: usize = 128;

/// One semantic colour slot. GPU instances carry a token, never a colour, so a theme change is a
/// 2 KiB uniform write instead of a page re-upload. The numeric values are part of the WGSL
/// contract (`shaders/palette.wgsl`); a test compares the two.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PaletteToken(u8);

impl PaletteToken {
    /// Not a palette slot: the instance's colour parameter is an inline RGBA8 colour.
    pub const INLINE: Self = Self(0);
    pub const BASE_A: Self = Self(1);
    pub const BASE_C: Self = Self(2);
    pub const BASE_G: Self = Self(3);
    pub const BASE_T: Self = Self(4);
    pub const BASE_N: Self = Self(5);
    pub const UNCATEGORIZED: Self = Self(6);
    pub const READ_NEUTRAL: Self = Self(7);
    pub const STRAND_FORWARD: Self = Self(8);
    pub const STRAND_REVERSE: Self = Self(9);
    /// Start of the mapping-quality ramp; the end is the next slot.
    pub const MAPPING_QUALITY_LOW: Self = Self(10);
    pub const MAPPING_QUALITY_HIGH: Self = Self(11);
    pub const FIRST_OF_PAIR_FORWARD: Self = Self(12);
    pub const FIRST_OF_PAIR_REVERSE: Self = Self(13);
    pub const FIRST_OF_PAIR_UNKNOWN: Self = Self(14);
    /// Four slots indexed by orientation rank.
    pub const PAIR_ORIENTATION_FIRST: Self = Self(15);
    /// Ten slots indexed by classification rank.
    pub const PAIR_CLASSIFICATION_FIRST: Self = Self(19);
    /// Eight slots indexed by `mode * 2 + reverse`.
    pub const SPECIALIST_BODY_FIRST: Self = Self(29);
    /// A base-modification call where no modification is the likelier state (and implicitly
    /// unmodified bases): the faint end of its probability ramp, then the full colour. Every
    /// `MODIFICATION_*` pair is laid out `faint, full` so a cell can be drawn as a
    /// [`PaletteColor::Ramp`] whose position is the call's confidence; see
    /// [`crate::modification_style::modification_cell_color`].
    pub const MODIFICATION_UNMODIFIED_FAINT: Self = Self(37);
    pub const MODIFICATION_UNMODIFIED: Self = Self(38);
    pub const BISULFITE_PROTECTED: Self = Self(39);
    pub const BISULFITE_CONVERTED: Self = Self(40);
    /// Insertions, padding and the specialist indel markers.
    pub const INSERTION: Self = Self(41);
    pub const DELETION: Self = Self(42);
    /// Reference skips and pair connectors.
    pub const SKIP: Self = Self(43);
    pub const SMRT_KINETICS: Self = Self(44);
    pub const SBX_TAIL: Self = Self(45);
    pub const SPECIALIST_UNAVAILABLE: Self = Self(46);
    pub const INTERACTION_SELECTED: Self = Self(47);
    pub const INTERACTION_HOVERED: Self = Self(48);
    pub const INTERACTION_SELECTED_CHAIN: Self = Self(49);
    pub const INTERACTION_HIGHLIGHTED: Self = Self(50);
    pub const SUMMARY_DEPTH: Self = Self(51);
    /// Modification bars when they are not coloured by modification type.
    pub const SUMMARY_MODIFICATION: Self = Self(52);
    pub const SUMMARY_MODIFICATION_BACKGROUND: Self = Self(53);
    pub const JUNCTION_POSITIVE: Self = Self(54);
    pub const JUNCTION_NEGATIVE: Self = Self(55);
    pub const JUNCTION_UNKNOWN: Self = Self(56);
    pub const JUNCTION_BASELINE: Self = Self(57);
    pub const JUNCTION_ANNOTATED: Self = Self(58);
    pub const JUNCTION_NOVEL: Self = Self(59);
    /// Unannotated junction markers and any summary component without a slot of its own.
    pub const SUMMARY_NEUTRAL: Self = Self(60);
    pub const JUNCTION_INTERACTION: Self = Self(61);
    pub const COVERAGE_CENTER_LINE: Self = Self(62);
    pub const COVERAGE_SELECTED_POSITION: Self = Self(63);
    pub const MODIFICATION_SELECTED_POSITION: Self = Self(64);
    pub const SASHIMI_SELECTED_POSITION: Self = Self(65);
    /// A clipped read end's cut edge (soft-clip sawtooth, hard-clip bar), drawn over the read
    /// body: dark and neutral, distinct from the insertion colour.
    pub const CLIP_EDGE: Self = Self(66);
    /// Soft-clipped ghost bases: the read's own colour is mixed towards this slot's colour by
    /// [`CLIP_GHOST_TINT`] and drawn with this slot's alpha. See
    /// [`AlignmentPalette::clip_ghost_color`].
    pub const CLIP_GHOST: Self = Self(67);
    /// 5-methylcytosine (`C+m`), and every modification in the single-colour and
    /// modified-versus-unmodified modes.
    pub const MODIFICATION_5MC_FAINT: Self = Self(68);
    pub const MODIFICATION_5MC: Self = Self(69);
    /// 5-hydroxymethylcytosine (`C+h`).
    pub const MODIFICATION_5HMC_FAINT: Self = Self(70);
    pub const MODIFICATION_5HMC: Self = Self(71);
    /// N6-methyladenine (`A+a`).
    pub const MODIFICATION_6MA_FAINT: Self = Self(72);
    pub const MODIFICATION_6MA: Self = Self(73);
    /// Every other modification code.
    pub const MODIFICATION_OTHER_FAINT: Self = Self(74);
    pub const MODIFICATION_OTHER: Self = Self(75);
    /// One past the last assigned slot.
    pub const END: Self = Self(76);

    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn base(base: u8) -> Self {
        match base.to_ascii_uppercase() {
            b'A' => Self::BASE_A,
            b'C' => Self::BASE_C,
            b'G' => Self::BASE_G,
            b'T' | b'U' => Self::BASE_T,
            _ => Self::BASE_N,
        }
    }

    /// Slot of a pair-orientation rank; ranks outside the closed table are uncategorised.
    #[must_use]
    pub const fn pair_orientation(rank: u8) -> Self {
        if (rank as usize) < PAIR_ORIENTATION_COLORS.len() {
            Self(Self::PAIR_ORIENTATION_FIRST.0 + rank)
        } else {
            Self::UNCATEGORIZED
        }
    }

    #[must_use]
    pub const fn pair_classification(rank: u8) -> Self {
        if (rank as usize) < PAIR_CLASSIFICATION_COLORS.len() {
            Self(Self::PAIR_CLASSIFICATION_FIRST.0 + rank)
        } else {
            Self::UNCATEGORIZED
        }
    }

    #[must_use]
    pub const fn specialist_body(mode: u8, reverse: bool) -> Self {
        if (mode as usize) < SPECIALIST_BODY_COLORS.len() {
            Self(Self::SPECIALIST_BODY_FIRST.0 + mode * 2 + reverse as u8)
        } else {
            Self::UNCATEGORIZED
        }
    }
}

/// Flag bit of the eight-bit palette index carried by an instance: the colour is a ramp between
/// the indexed slot and the next one, and the parameter word holds the f32 bits of the position.
pub const PALETTE_INDEX_RAMP_FLAG: u8 = 0x80;

/// How far a ghost base's colour moves from its read's colour towards [`PaletteToken::CLIP_GHOST`].
/// Mirrored by `CLIP_GHOST_TINT` in `alignment_scene.wgsl`.
pub const CLIP_GHOST_TINT: f32 = 0.5;
const PALETTE_INDEX_SLOT_MASK: u8 = 0x7F;

/// The colour of one instance, before a palette is applied.
///
/// Three forms cover every colour the renderers draw:
///
/// * [`Self::Token`]: a closed semantic category; the common case, recoloured by any theme.
/// * [`Self::Ramp`]: a continuous quantity (mapping quality today; quality-scaled or
///   probability-scaled colours fit the same form) expressed as a position between two adjacent
///   palette slots, so a theme still chooses both ends.
/// * [`Self::Inline`]: the escape hatch for open-ended categories (hashed read group, haplotype,
///   tag value, modification code) whose colour cannot come from a fixed table. The colour is
///   carried in the instance as RGBA8 and is the same under every theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PaletteColor {
    Token(PaletteToken),
    Ramp { low: PaletteToken, position: f32 },
    Inline([u8; 4]),
}

impl PaletteColor {
    /// Quantises an arbitrary colour for the inline form.
    #[must_use]
    pub fn inline(color: [f32; 4]) -> Self {
        Self::Inline(color.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8))
    }

    /// The `(index, parameter)` pair stored in a GPU instance.
    #[must_use]
    pub const fn encode(self) -> (u8, u32) {
        match self {
            Self::Token(token) => (token.0, 0),
            Self::Ramp { low, position } => (low.0 | PALETTE_INDEX_RAMP_FLAG, position.to_bits()),
            Self::Inline(rgba) => (0, u32::from_le_bytes(rgba)),
        }
    }

    /// Inverse of [`Self::encode`] for any index/parameter pair, including ones no packer emits.
    #[must_use]
    pub const fn decode(index: u8, parameter: u32) -> Self {
        let slot = index & PALETTE_INDEX_SLOT_MASK;
        if slot == 0 {
            Self::Inline(parameter.to_le_bytes())
        } else if index & PALETTE_INDEX_RAMP_FLAG != 0 {
            Self::Ramp {
                low: PaletteToken(slot),
                position: f32::from_bits(parameter),
            }
        } else {
            Self::Token(PaletteToken(slot))
        }
    }
}

/// One complete set of semantic colours, uploaded as a uniform. Mirrors `Palette` in
/// `shaders/palette.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct AlignmentPalette {
    colors: [[f32; 4]; PALETTE_ENTRIES],
}

impl AlignmentPalette {
    /// The colour of one slot.
    #[must_use]
    pub const fn color(&self, token: PaletteToken) -> [f32; 4] {
        self.colors[(token.0 & PALETTE_INDEX_SLOT_MASK) as usize]
    }

    /// Resolves an instance colour exactly as `palette_color` in `shaders/palette.wgsl` does.
    #[must_use]
    pub fn resolve(&self, color: PaletteColor) -> [f32; 4] {
        match color {
            PaletteColor::Token(token) => self.color(token),
            PaletteColor::Ramp { low, position } => {
                let low_color = self.color(low);
                let high_color = self.colors[usize::from((low.0 + 1) & PALETTE_INDEX_SLOT_MASK)];
                // Not clamped: mapping qualities above the nominal maximum extrapolate, and the
                // colour target clamps the result, as the baked colours always did.
                let position = if position.is_finite() { position } else { 0.0 };
                std::array::from_fn(|channel| {
                    low_color[channel] + (high_color[channel] - low_color[channel]) * position
                })
            }
            PaletteColor::Inline(rgba) => rgba.map(|channel| f32::from(channel) / 255.0),
        }
    }

    /// Blends `color` towards the interaction colour of the strongest active state, keeping its
    /// alpha, exactly as `interaction_color` in `alignment_scene.wgsl` does. Returns `color` when
    /// nothing is active.
    #[must_use]
    pub fn interaction_color(
        &self,
        mut color: [f32; 4],
        token: rgv_alignment_scene::AlignmentInteractionToken,
    ) -> [f32; 4] {
        let (target, amount) = if token.selected {
            (PaletteToken::INTERACTION_SELECTED, 0.58)
        } else if token.hovered {
            (PaletteToken::INTERACTION_HOVERED, 0.32)
        } else if token.selected_chain {
            (PaletteToken::INTERACTION_SELECTED_CHAIN, 0.38)
        } else if token.highlighted {
            (PaletteToken::INTERACTION_HIGHLIGHTED, 0.46)
        } else {
            return color;
        };
        let target = self.color(target);
        for channel in 0..3 {
            color[channel] += (target[channel] - color[channel]) * amount;
        }
        // The alpha is kept: a faint (low-confidence) modification cell on a selected read
        // stays faint rather than becoming an opaque block.
        color
    }

    /// The faint, translucent tint a soft-clipped ghost base is drawn in, from the colour its
    /// read is drawn in (after any interaction emphasis), exactly as `fragment_main` in
    /// `alignment_scene.wgsl` derives it for shape 14.
    #[must_use]
    pub fn clip_ghost_color(&self, read: [f32; 4]) -> [f32; 4] {
        let tint = self.color(PaletteToken::CLIP_GHOST);
        [
            read[0] + (tint[0] - read[0]) * CLIP_GHOST_TINT,
            read[1] + (tint[1] - read[1]) * CLIP_GHOST_TINT,
            read[2] + (tint[2] - read[2]) * CLIP_GHOST_TINT,
            tint[3],
        ]
    }

    fn set(&mut self, token: PaletteToken, color: [f32; 4]) {
        self.colors[usize::from(token.0)] = color;
    }

    fn set_run<const N: usize>(&mut self, first: PaletteToken, colors: [[f32; 4]; N]) {
        for (offset, color) in colors.into_iter().enumerate() {
            self.colors[usize::from(first.0) + offset] = color;
        }
    }

    /// The colours RGV has always drawn; every other palette starts from these.
    #[must_use]
    pub fn light() -> Self {
        Self::light_over(UNCATEGORIZED_COLOR)
    }

    /// [`Self::light`] with every unassigned slot set to `filler`.
    fn light_over(filler: [f32; 4]) -> Self {
        use PaletteToken as T;
        let mut palette = Self {
            colors: [filler; PALETTE_ENTRIES],
        };
        palette.set_run(T::BASE_A, BASE_COLORS);
        palette.set(T::UNCATEGORIZED, UNCATEGORIZED_COLOR);
        palette.set(T::READ_NEUTRAL, [0.73, 0.73, 0.73, 1.0]);
        palette.set(T::STRAND_FORWARD, [0.30, 0.50, 0.78, 1.0]);
        palette.set(T::STRAND_REVERSE, [0.78, 0.34, 0.38, 1.0]);
        palette.set(T::MAPPING_QUALITY_LOW, [0.25, 0.30, 0.40, 1.0]);
        palette.set(T::MAPPING_QUALITY_HIGH, [0.60, 0.70, 0.85, 1.0]);
        palette.set(T::FIRST_OF_PAIR_FORWARD, [0.28, 0.56, 0.78, 1.0]);
        palette.set(T::FIRST_OF_PAIR_REVERSE, [0.80, 0.38, 0.42, 1.0]);
        palette.set(T::FIRST_OF_PAIR_UNKNOWN, [0.50, 0.50, 0.54, 1.0]);
        palette.set_run(T::PAIR_ORIENTATION_FIRST, PAIR_ORIENTATION_COLORS);
        palette.set_run(T::PAIR_CLASSIFICATION_FIRST, PAIR_CLASSIFICATION_COLORS);
        palette.set_specialist_bodies(SPECIALIST_BODY_COLORS);
        palette.set_modifications(MODIFICATION_COLORS);
        palette.set(T::BISULFITE_PROTECTED, [0.86, 0.46, 0.12, 1.0]);
        palette.set(T::BISULFITE_CONVERTED, [0.18, 0.62, 0.78, 1.0]);
        palette.set(T::INSERTION, [0.55, 0.20, 0.70, 1.0]);
        palette.set(T::DELETION, [0.18, 0.18, 0.20, 1.0]);
        palette.set(T::SKIP, [0.38, 0.40, 0.46, 1.0]);
        palette.set(T::SMRT_KINETICS, [0.10, 0.63, 0.56, 1.0]);
        palette.set(T::SBX_TAIL, [0.86, 0.52, 0.12, 1.0]);
        palette.set(T::SPECIALIST_UNAVAILABLE, [0.52, 0.52, 0.54, 0.8]);
        palette.set(T::INTERACTION_SELECTED, [0.16, 0.42, 0.92, 1.0]);
        palette.set(T::INTERACTION_HOVERED, [0.28, 0.52, 0.94, 1.0]);
        palette.set(T::INTERACTION_SELECTED_CHAIN, [0.90, 0.62, 0.16, 1.0]);
        palette.set(T::INTERACTION_HIGHLIGHTED, [0.96, 0.70, 0.12, 1.0]);
        palette.set(T::SUMMARY_DEPTH, [0.48, 0.48, 0.48, 1.0]);
        palette.set_modification_summary();
        palette.set(T::JUNCTION_POSITIVE, [0.84, 0.30, 0.38, 0.82]);
        palette.set(T::JUNCTION_NEGATIVE, [0.25, 0.45, 0.82, 0.82]);
        palette.set(T::JUNCTION_UNKNOWN, [0.42, 0.45, 0.50, 0.76]);
        palette.set(T::JUNCTION_BASELINE, [0.36, 0.38, 0.42, 0.24]);
        palette.set(T::JUNCTION_ANNOTATED, [0.16, 0.58, 0.28, 1.0]);
        palette.set(T::JUNCTION_NOVEL, [0.92, 0.48, 0.10, 1.0]);
        palette.set(T::SUMMARY_NEUTRAL, [0.46, 0.48, 0.52, 0.92]);
        palette.set(T::JUNCTION_INTERACTION, [0.16, 0.42, 0.92, 1.0]);
        palette.set(T::COVERAGE_CENTER_LINE, [0.72, 0.75, 0.82, 0.72]);
        palette.set(T::COVERAGE_SELECTED_POSITION, [1.0, 0.78, 0.18, 0.92]);
        palette.set(T::MODIFICATION_SELECTED_POSITION, [0.08, 0.08, 0.08, 0.34]);
        palette.set(T::SASHIMI_SELECTED_POSITION, [0.16, 0.42, 0.92, 0.40]);
        palette.set(T::CLIP_EDGE, [0.12, 0.12, 0.14, 1.0]);
        palette.set(T::CLIP_GHOST, [1.0, 1.0, 1.0, 0.45]);
        palette
    }

    /// Sets every modification pair from its full colour: the faint end is the same colour at
    /// [`MODIFICATION_FAINT_ALPHA`], so a low-confidence call fades towards whatever the read
    /// body beneath it is drawn in.
    fn set_modifications(&mut self, colors: ModificationColors) {
        use PaletteToken as T;
        for (faint, color) in [
            (T::MODIFICATION_UNMODIFIED_FAINT, colors.unmodified),
            (T::MODIFICATION_5MC_FAINT, colors.five_methyl_c),
            (T::MODIFICATION_5HMC_FAINT, colors.five_hydroxymethyl_c),
            (T::MODIFICATION_6MA_FAINT, colors.six_methyl_a),
            (T::MODIFICATION_OTHER_FAINT, colors.other),
        ] {
            self.set_run(
                faint,
                [
                    [color[0], color[1], color[2], MODIFICATION_FAINT_ALPHA],
                    color,
                ],
            );
        }
    }

    /// The modification summary lane draws modified fractions in the 5mC colour and the
    /// unmodified band in the unmodified colour, as the read stack does.
    fn set_modification_summary(&mut self) {
        use PaletteToken as T;
        let [r, g, b, _] = self.color(T::MODIFICATION_5MC);
        self.set(T::SUMMARY_MODIFICATION, [r, g, b, 0.92]);
        let [r, g, b, _] = self.color(T::MODIFICATION_UNMODIFIED);
        self.set(T::SUMMARY_MODIFICATION_BACKGROUND, [r, g, b, 0.20]);
    }

    fn set_specialist_bodies(&mut self, bodies: [[[f32; 4]; 2]; 4]) {
        for (mode, strands) in bodies.into_iter().enumerate() {
            self.set_run(PaletteToken::specialist_body(mode as u8, false), strands);
        }
    }

    /// Light colours re-chosen for a dark canvas: quiet categories move towards the background,
    /// and anything that was near-black on white (deletions, unmapped reads) becomes near-white.
    #[must_use]
    pub fn dark() -> Self {
        use PaletteToken as T;
        let mut palette = Self::light();
        let neutral = [0.36, 0.37, 0.40, 1.0];
        palette.set(T::UNCATEGORIZED, [0.52, 0.52, 0.56, 1.0]);
        palette.set(T::READ_NEUTRAL, neutral);
        palette.set(T::STRAND_FORWARD, [0.34, 0.54, 0.82, 1.0]);
        palette.set(T::STRAND_REVERSE, [0.82, 0.40, 0.44, 1.0]);
        palette.set(T::MAPPING_QUALITY_LOW, [0.20, 0.23, 0.30, 1.0]);
        palette.set(T::MAPPING_QUALITY_HIGH, [0.50, 0.62, 0.80, 1.0]);
        palette.set(T::FIRST_OF_PAIR_FORWARD, [0.32, 0.60, 0.82, 1.0]);
        palette.set(T::FIRST_OF_PAIR_REVERSE, [0.84, 0.44, 0.48, 1.0]);
        palette.set_run(
            T::PAIR_ORIENTATION_FIRST,
            [
                neutral,
                [0.22, 0.70, 0.30, 1.0],
                [0.16, 0.60, 0.88, 1.0],
                [0.62, 0.50, 0.40, 1.0],
            ],
        );
        palette.set_run(
            T::PAIR_CLASSIFICATION_FIRST,
            [
                neutral,
                [0.50, 0.58, 0.76, 1.0],
                [0.30, 0.45, 0.95, 1.0],
                [0.92, 0.28, 0.28, 1.0],
                [0.10, 0.70, 0.70, 1.0],
                [0.70, 0.40, 0.85, 1.0],
                [0.93, 0.55, 0.10, 1.0],
                [0.72, 0.78, 0.15, 1.0],
                [0.86, 0.86, 0.90, 1.0],
                [0.65, 0.45, 0.28, 1.0],
            ],
        );
        palette.set_specialist_bodies([
            [[0.30, 0.38, 0.52, 1.0], [0.52, 0.36, 0.30, 1.0]],
            [[0.28, 0.46, 0.38, 1.0], [0.50, 0.42, 0.22, 1.0]],
            [[0.40, 0.32, 0.56, 1.0], [0.58, 0.28, 0.36, 1.0]],
            [[0.24, 0.46, 0.56, 1.0], [0.56, 0.48, 0.32, 1.0]],
        ]);
        palette.set(T::INSERTION, [0.74, 0.42, 0.90, 1.0]);
        palette.set(T::DELETION, [0.88, 0.88, 0.90, 1.0]);
        palette.set(T::SKIP, [0.62, 0.64, 0.70, 1.0]);
        palette.set(T::INTERACTION_SELECTED, [0.40, 0.62, 1.00, 1.0]);
        palette.set(T::INTERACTION_HOVERED, [0.50, 0.70, 1.00, 1.0]);
        palette.set(T::SUMMARY_DEPTH, [0.56, 0.57, 0.60, 1.0]);
        palette.set(T::JUNCTION_UNKNOWN, [0.62, 0.65, 0.70, 0.76]);
        palette.set(T::JUNCTION_BASELINE, [0.70, 0.72, 0.76, 0.24]);
        palette.set(T::JUNCTION_ANNOTATED, [0.25, 0.72, 0.38, 1.0]);
        palette.set(T::SUMMARY_NEUTRAL, [0.62, 0.64, 0.68, 0.92]);
        palette.set(T::JUNCTION_INTERACTION, [0.40, 0.62, 1.00, 1.0]);
        palette.set(T::COVERAGE_CENTER_LINE, [0.50, 0.53, 0.60, 0.72]);
        palette.set(T::MODIFICATION_SELECTED_POSITION, [0.95, 0.95, 0.95, 0.34]);
        palette.set(T::SASHIMI_SELECTED_POSITION, [0.40, 0.62, 1.00, 0.40]);
        // Drawn on the read body, whose dark-theme colours are mid-tones, so near-black still
        // separates clearly from every body colour.
        palette.set(T::CLIP_EDGE, [0.04, 0.04, 0.05, 1.0]);
        // Ghosts fade towards the dark canvas rather than towards white.
        palette.set(T::CLIP_GHOST, [0.10, 0.11, 0.13, 0.55]);
        palette
    }

    /// Replaces every closed category that relied on a red/green contrast with Okabe-Ito
    /// colours (Okabe & Ito 2008, "Color Universal Design"). `dark` selects the member of each
    /// pair that keeps contrast against a dark canvas.
    fn with_okabe_ito(mut self, dark: bool) -> Self {
        use PaletteToken as T;
        let neutral = self.color(T::READ_NEUTRAL);
        // Bases follow the Sanger-trace convention the scheme can express: A green, C blue,
        // T red (vermillion), and G black on a light canvas or yellow on a dark one. Orange is
        // avoided for G because orange and vermillion are the scheme's closest pair for
        // protanopes.
        let (guanine, unknown_base, extreme) = if dark {
            (
                okabe_ito::YELLOW,
                [0.80, 0.80, 0.80, 1.0],
                [0.92, 0.92, 0.92, 1.0],
            )
        } else {
            (
                okabe_ito::BLACK,
                [0.75, 0.75, 0.75, 1.0],
                [0.15, 0.15, 0.15, 1.0],
            )
        };
        self.set_run(
            T::BASE_A,
            [
                okabe_ito::BLUISH_GREEN,
                okabe_ito::BLUE,
                guanine,
                okabe_ito::VERMILLION,
                unknown_base,
            ],
        );
        self.set(T::STRAND_FORWARD, okabe_ito::BLUE);
        self.set(T::STRAND_REVERSE, okabe_ito::VERMILLION);
        self.set(T::FIRST_OF_PAIR_FORWARD, okabe_ito::SKY_BLUE);
        self.set(T::FIRST_OF_PAIR_REVERSE, okabe_ito::ORANGE);
        self.set_run(
            T::PAIR_ORIENTATION_FIRST,
            [
                neutral,
                okabe_ito::BLUISH_GREEN,
                okabe_ito::BLUE,
                okabe_ito::VERMILLION,
            ],
        );
        self.set_run(
            T::PAIR_CLASSIFICATION_FIRST,
            [
                neutral,
                okabe_ito::SKY_BLUE,
                okabe_ito::BLUE,
                okabe_ito::VERMILLION,
                okabe_ito::BLUISH_GREEN,
                okabe_ito::REDDISH_PURPLE,
                okabe_ito::ORANGE,
                okabe_ito::YELLOW,
                extreme,
                // Tenth category (unpaired): the scheme has eight colours, so this one is an
                // indigo chosen by search to stay far from the other nine under both
                // dichromacies.
                [0.32, 0.26, 0.95, 1.0],
            ],
        );
        self.set_modifications(if dark {
            DARK_COLOR_BLIND_MODIFICATION_COLORS
        } else {
            LIGHT_COLOR_BLIND_MODIFICATION_COLORS
        });
        self.set_modification_summary();
        self.set(T::BISULFITE_PROTECTED, okabe_ito::ORANGE);
        self.set(T::BISULFITE_CONVERTED, okabe_ito::SKY_BLUE);
        let with_alpha = |color: [f32; 4], alpha: f32| [color[0], color[1], color[2], alpha];
        self.set(
            T::JUNCTION_POSITIVE,
            with_alpha(okabe_ito::VERMILLION, 0.82),
        );
        self.set(T::JUNCTION_NEGATIVE, with_alpha(okabe_ito::BLUE, 0.82));
        self.set(T::JUNCTION_ANNOTATED, okabe_ito::BLUISH_GREEN);
        self.set(T::JUNCTION_NOVEL, okabe_ito::ORANGE);
        self
    }

    #[must_use]
    pub fn light_color_blind() -> Self {
        Self::light().with_okabe_ito(false)
    }

    #[must_use]
    pub fn dark_color_blind() -> Self {
        Self::dark().with_okabe_ito(true)
    }

    #[must_use]
    pub fn for_theme(theme: ThemeId) -> Self {
        match theme {
            ThemeId::Light => Self::light(),
            ThemeId::Dark => Self::dark(),
            ThemeId::LightColorBlind => Self::light_color_blind(),
            ThemeId::DarkColorBlind => Self::dark_color_blind(),
        }
    }
}

impl Default for AlignmentPalette {
    fn default() -> Self {
        Self::light()
    }
}

/// The eight Okabe-Ito colours (sRGB 8-bit values from the original publication).
pub mod okabe_ito {
    const fn rgb(red: u8, green: u8, blue: u8) -> [f32; 4] {
        [
            red as f32 / 255.0,
            green as f32 / 255.0,
            blue as f32 / 255.0,
            1.0,
        ]
    }
    pub const BLACK: [f32; 4] = rgb(0, 0, 0);
    pub const ORANGE: [f32; 4] = rgb(230, 159, 0);
    pub const SKY_BLUE: [f32; 4] = rgb(86, 180, 233);
    pub const BLUISH_GREEN: [f32; 4] = rgb(0, 158, 115);
    pub const YELLOW: [f32; 4] = rgb(240, 228, 66);
    pub const BLUE: [f32; 4] = rgb(0, 114, 178);
    pub const VERMILLION: [f32; 4] = rgb(213, 94, 0);
    pub const REDDISH_PURPLE: [f32; 4] = rgb(204, 121, 167);
}

/// The four built-in themes.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ThemeId {
    #[default]
    Light,
    Dark,
    LightColorBlind,
    DarkColorBlind,
}

impl ThemeId {
    pub const ALL: [Self; 4] = [
        Self::Light,
        Self::Dark,
        Self::LightColorBlind,
        Self::DarkColorBlind,
    ];

    #[must_use]
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Dark | Self::DarkColorBlind)
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::LightColorBlind => "Light (colour-blind safe)",
            Self::DarkColorBlind => "Dark (colour-blind safe)",
        }
    }
}

/// Everything a front end needs to draw one theme: the alignment palette and the colours of the
/// canvas and the chrome painted around it. Chrome colours are opaque sRGB bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub id: ThemeId,
    pub palette: AlignmentPalette,
    /// Clear colour of the genomic canvas.
    pub canvas: [u8; 3],
    /// Fill of panels, windows and other chrome drawn around the canvas.
    pub chrome_fill: [u8; 3],
    /// A second, slightly stronger chrome fill (alternating bands, faint backgrounds).
    pub chrome_fill_strong: [u8; 3],
    /// Primary text.
    pub text: [u8; 3],
    /// Accent for links, selection and focus.
    pub accent: [u8; 3],
}

impl Theme {
    #[must_use]
    pub fn new(id: ThemeId) -> Self {
        let palette = AlignmentPalette::for_theme(id);
        if id.is_dark() {
            Self {
                id,
                palette,
                canvas: [27, 29, 33],
                chrome_fill: [36, 39, 44],
                chrome_fill_strong: [48, 52, 59],
                text: [226, 229, 234],
                accent: [102, 158, 255],
            }
        } else {
            Self {
                id,
                palette,
                canvas: [255, 255, 255],
                chrome_fill: [246, 247, 249],
                chrome_fill_strong: [234, 237, 242],
                text: [28, 32, 38],
                accent: [41, 107, 235],
            }
        }
    }

    /// The canvas colour as the components of a wgpu clear colour for the non-sRGB (`*Unorm`)
    /// targets RGV renders into.
    #[must_use]
    pub fn canvas_clear(&self) -> [f64; 4] {
        [
            f64::from(self.canvas[0]) / 255.0,
            f64::from(self.canvas[1]) / 255.0,
            f64::from(self.canvas[2]) / 255.0,
            1.0,
        ]
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(ThemeId::Light)
    }
}

/// The palette uniform and its bind group (group 1 of the alignment pipelines).
#[derive(Debug)]
pub(crate) struct PaletteBinding {
    palette: AlignmentPalette,
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    uploads: u64,
}

impl PaletteBinding {
    pub(crate) fn layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("RGV alignment palette layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(
                        size_of::<AlignmentPalette>() as u64
                    ),
                },
                count: None,
            }],
        })
    }

    /// Creates the uniform already holding `palette`, without needing a queue.
    pub(crate) fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        palette: &AlignmentPalette,
    ) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("RGV alignment palette"),
            size: size_of::<AlignmentPalette>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: true,
        });
        buffer
            .slice(..)
            .get_mapped_range_mut()
            .copy_from_slice(bytemuck::bytes_of(palette));
        buffer.unmap();
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RGV alignment palette bind group"),
            layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        Self {
            palette: *palette,
            buffer,
            bind_group,
            uploads: 0,
        }
    }

    pub(crate) const fn palette(&self) -> &AlignmentPalette {
        &self.palette
    }

    pub(crate) const fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Number of palette uploads since creation (each is [`size_of::<AlignmentPalette>`] bytes).
    pub(crate) const fn uploads(&self) -> u64 {
        self.uploads
    }

    /// Uploads `palette` if it differs from the current one. Returns whether it was uploaded.
    pub(crate) fn set(&mut self, queue: &wgpu::Queue, palette: &AlignmentPalette) -> bool {
        if self.palette == *palette {
            return false;
        }
        self.palette = *palette;
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(palette));
        self.uploads += 1;
        true
    }
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

    /// Every `const TOKEN_<NAME>: u32 = <n>u;` declared by the shared palette chunk.
    fn shader_tokens() -> Vec<(String, u8)> {
        include_str!("shaders/palette.wgsl")
            .lines()
            .filter_map(|line| line.strip_prefix("const TOKEN_"))
            .map(|line| {
                let (name, value) = line.split_once(": u32 = ").unwrap();
                (
                    name.to_owned(),
                    value.trim_end_matches("u;").parse().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn shader_token_constants_match_the_rust_tokens() {
        use PaletteToken as T;
        let expected = [
            ("BASE_A", T::BASE_A),
            ("BASE_N", T::BASE_N),
            ("BISULFITE_PROTECTED", T::BISULFITE_PROTECTED),
            ("BISULFITE_CONVERTED", T::BISULFITE_CONVERTED),
            ("INTERACTION_SELECTED", T::INTERACTION_SELECTED),
            ("INTERACTION_HOVERED", T::INTERACTION_HOVERED),
            ("INTERACTION_SELECTED_CHAIN", T::INTERACTION_SELECTED_CHAIN),
            ("INTERACTION_HIGHLIGHTED", T::INTERACTION_HIGHLIGHTED),
            ("SUMMARY_DEPTH", T::SUMMARY_DEPTH),
            ("SUMMARY_MODIFICATION", T::SUMMARY_MODIFICATION),
            (
                "SUMMARY_MODIFICATION_BACKGROUND",
                T::SUMMARY_MODIFICATION_BACKGROUND,
            ),
            ("JUNCTION_POSITIVE", T::JUNCTION_POSITIVE),
            ("JUNCTION_NEGATIVE", T::JUNCTION_NEGATIVE),
            ("JUNCTION_UNKNOWN", T::JUNCTION_UNKNOWN),
            ("JUNCTION_ANNOTATED", T::JUNCTION_ANNOTATED),
            ("JUNCTION_NOVEL", T::JUNCTION_NOVEL),
            ("SUMMARY_NEUTRAL", T::SUMMARY_NEUTRAL),
            ("JUNCTION_INTERACTION", T::JUNCTION_INTERACTION),
            ("CLIP_GHOST", T::CLIP_GHOST),
        ];
        let shader = shader_tokens();
        assert_eq!(shader.len(), expected.len(), "{shader:?}");
        for (name, token) in expected {
            let value = shader
                .iter()
                .find(|(shader_name, _)| shader_name == name)
                .unwrap_or_else(|| panic!("palette.wgsl does not declare TOKEN_{name}"))
                .1;
            assert_eq!(value, token.index(), "TOKEN_{name}");
        }
        // The shader's palette array and slot mask are sized like the Rust table.
        let chunk = include_str!("shaders/palette.wgsl");
        assert!(chunk.contains(&format!("array<vec4<f32>, {PALETTE_ENTRIES}>")));
        assert!(chunk.contains(&format!("& {PALETTE_INDEX_SLOT_MASK}u")));
        assert_eq!(size_of::<AlignmentPalette>(), PALETTE_ENTRIES * 16);
    }

    /// The alignment shaders take every colour from the palette or from an instance: a colour
    /// literal in either file would be invisible to theming. (This replaces the former check
    /// that the summary shader's hand-copied base colours matched [`BASE_COLORS`]; with the
    /// literals gone the two can no longer disagree.)
    #[test]
    fn alignment_shaders_contain_no_colour_literals() {
        for (name, shader) in [
            (
                "alignment_summary.wgsl",
                include_str!("alignment_summary.wgsl"),
            ),
            ("alignment_scene.wgsl", include_str!("alignment_scene.wgsl")),
        ] {
            for literal in shader.split("vec4<f32>(").skip(1) {
                let arguments = literal.split(')').next().unwrap();
                let numeric = arguments
                    .split(',')
                    .filter(|value| value.trim().parse::<f32>().is_ok())
                    .count();
                assert!(
                    numeric < 3,
                    "{name} bakes the colour vec4<f32>({arguments})"
                );
            }
            for literal in shader.split("vec3<f32>(").skip(1) {
                let arguments = literal.split(')').next().unwrap();
                assert!(
                    !arguments
                        .split(',')
                        .all(|value| value.trim().parse::<f32>().is_ok()),
                    "{name} bakes the colour vec3<f32>({arguments})"
                );
            }
        }
    }

    #[test]
    fn every_assigned_slot_is_set_in_every_palette_and_allele_slots_are_the_base_table() {
        let unset = AlignmentPalette::light_over([f32::NAN; 4]);
        for slot in 1..PaletteToken::END.index() {
            assert!(
                unset
                    .color(PaletteToken(slot))
                    .iter()
                    .all(|c| c.is_finite()),
                "slot {slot} is not assigned by the Light palette"
            );
        }
        for slot in PaletteToken::END.index()..PALETTE_ENTRIES as u8 {
            assert!(unset.color(PaletteToken(slot))[0].is_nan());
        }
        let light = AlignmentPalette::light();
        for (index, color) in BASE_COLORS.into_iter().enumerate() {
            assert_same(
                light.color(PaletteToken(PaletteToken::BASE_A.0 + index as u8)),
                color,
            );
        }
        for theme in ThemeId::ALL {
            let palette = AlignmentPalette::for_theme(theme);
            assert_eq!(Theme::new(theme).palette, palette);
            for slot in 1..PaletteToken::END.index() {
                assert!(
                    palette
                        .color(PaletteToken(slot))
                        .iter()
                        .all(|channel| (0.0..=1.0).contains(channel)),
                    "{theme:?} slot {slot}"
                );
            }
        }
        assert_ne!(AlignmentPalette::light(), AlignmentPalette::dark());
        assert_ne!(
            AlignmentPalette::light(),
            AlignmentPalette::light_color_blind()
        );
        assert_ne!(
            AlignmentPalette::dark(),
            AlignmentPalette::dark_color_blind()
        );
    }

    // ---- colour-vision-deficiency simulation -------------------------------------------------

    /// Machado, Oliveira & Fernandes (2009) dichromacy matrices (severity 1.0), applied in
    /// linear RGB.
    const PROTANOPIA: [[f64; 3]; 3] = [
        [0.152_286, 1.052_583, -0.204_868],
        [0.114_503, 0.786_281, 0.099_216],
        [-0.003_882, -0.048_116, 1.051_998],
    ];
    const DEUTERANOPIA: [[f64; 3]; 3] = [
        [0.367_322, 0.860_646, -0.227_968],
        [0.280_085, 0.672_501, 0.047_413],
        [-0.011_820, 0.042_940, 0.968_881],
    ];
    const NORMAL: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    fn simulated_lab(color: [f32; 4], vision: &[[f64; 3]; 3]) -> [f64; 3] {
        let linear = [color[0], color[1], color[2]].map(|channel| {
            let channel = f64::from(channel);
            if channel <= 0.040_45 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        });
        let seen: [f64; 3] = std::array::from_fn(|row| {
            (0..3)
                .map(|column| vision[row][column] * linear[column])
                .sum::<f64>()
                .clamp(0.0, 1.0)
        });
        // Linear sRGB (D65) to CIE L*a*b*.
        let x = 0.412_456_4 * seen[0] + 0.357_576_1 * seen[1] + 0.180_437_5 * seen[2];
        let y = 0.212_672_9 * seen[0] + 0.715_152_2 * seen[1] + 0.072_175_0 * seen[2];
        let z = 0.019_333_9 * seen[0] + 0.119_192_0 * seen[1] + 0.950_304_1 * seen[2];
        let f = |t: f64| {
            if t > 0.008_856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (f(x / 0.950_47), f(y), f(z / 1.088_83));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    /// Smallest CIE76 colour difference between any two of `colors` under normal vision,
    /// protanopia and deuteranopia.
    fn minimum_delta_e(colors: &[[f32; 4]]) -> f64 {
        let mut minimum = f64::INFINITY;
        for vision in [&NORMAL, &PROTANOPIA, &DEUTERANOPIA] {
            for (index, left) in colors.iter().enumerate() {
                for right in &colors[index + 1..] {
                    let (left, right) =
                        (simulated_lab(*left, vision), simulated_lab(*right, vision));
                    let delta = (0..3)
                        .map(|axis| (left[axis] - right[axis]).powi(2))
                        .sum::<f64>()
                        .sqrt();
                    minimum = minimum.min(delta);
                }
            }
        }
        minimum
    }

    fn run(palette: &AlignmentPalette, first: PaletteToken, count: u8) -> Vec<[f32; 4]> {
        (0..count)
            .map(|offset| palette.color(PaletteToken(first.0 + offset)))
            .collect()
    }

    #[test]
    fn colour_blind_palettes_stay_distinct_under_simulated_protanopia_and_deuteranopia() {
        use PaletteToken as T;
        for theme in [ThemeId::LightColorBlind, ThemeId::DarkColorBlind] {
            let full = Theme::new(theme);
            let palette = &full.palette;
            let canvas = [
                f32::from(full.canvas[0]) / 255.0,
                f32::from(full.canvas[1]) / 255.0,
                f32::from(full.canvas[2]) / 255.0,
                1.0,
            ];
            // A delta E of 2.3 is one just-noticeable difference; these floors are far above.
            let nucleotides = run(palette, T::BASE_A, 4);
            assert!(
                minimum_delta_e(&nucleotides) >= 30.0,
                "{theme:?} A/C/G/T: {}",
                minimum_delta_e(&nucleotides)
            );
            let with_unknown = run(palette, T::BASE_A, 5);
            assert!(
                minimum_delta_e(&with_unknown) >= 15.0,
                "{theme:?} bases with N: {}",
                minimum_delta_e(&with_unknown)
            );
            // Mismatches are drawn over read bodies and on the bare canvas.
            for base in &nucleotides {
                for backdrop in [palette.color(T::READ_NEUTRAL), canvas] {
                    let delta = minimum_delta_e(&[*base, backdrop]);
                    assert!(delta >= 12.0, "{theme:?} base on its backdrop: {delta}");
                }
            }
            let strand = run(palette, T::STRAND_FORWARD, 2);
            assert!(minimum_delta_e(&strand) >= 40.0, "{theme:?} strand");
            let first_of_pair = run(palette, T::FIRST_OF_PAIR_FORWARD, 3);
            assert!(
                minimum_delta_e(&first_of_pair) >= 25.0,
                "{theme:?} first of pair"
            );
            let orientation = run(palette, T::PAIR_ORIENTATION_FIRST, 4);
            assert!(
                minimum_delta_e(&orientation) >= 15.0,
                "{theme:?} pair orientation: {}",
                minimum_delta_e(&orientation)
            );
            let classification = run(palette, T::PAIR_CLASSIFICATION_FIRST, 10);
            assert!(
                minimum_delta_e(&classification) >= 12.0,
                "{theme:?} pair classification: {}",
                minimum_delta_e(&classification)
            );
            let junction_strand = run(palette, T::JUNCTION_POSITIVE, 2);
            assert!(
                minimum_delta_e(&junction_strand) >= 40.0,
                "{theme:?} junction strand"
            );
            let bisulfite = run(palette, T::BISULFITE_PROTECTED, 2);
            assert!(minimum_delta_e(&bisulfite) >= 40.0, "{theme:?} bisulfite");
        }
        // The check discriminates: the default palettes, which lean on red/green contrasts, do
        // not meet the floors the colour-blind palettes are held to.
        for theme in [ThemeId::Light, ThemeId::Dark] {
            let palette = AlignmentPalette::for_theme(theme);
            assert!(minimum_delta_e(&run(&palette, T::BASE_A, 4)) < 30.0);
            assert!(minimum_delta_e(&run(&palette, T::PAIR_CLASSIFICATION_FIRST, 10)) < 12.0);
        }
    }

    /// CIE76 difference between two colours under one vision.
    fn delta_e(left: [f32; 4], right: [f32; 4], vision: &[[f64; 3]; 3]) -> f64 {
        let (left, right) = (simulated_lab(left, vision), simulated_lab(right, vision));
        (0..3)
            .map(|axis| (left[axis] - right[axis]).powi(2))
            .sum::<f64>()
            .sqrt()
    }

    #[test]
    fn modification_colours_stay_clear_of_insertions_mismatches_and_selection() {
        use PaletteToken as T;
        let modifications = [
            ("unmodified", T::MODIFICATION_UNMODIFIED),
            ("5mC", T::MODIFICATION_5MC),
            ("5hmC", T::MODIFICATION_5HMC),
            ("6mA", T::MODIFICATION_6MA),
            ("other", T::MODIFICATION_OTHER),
        ];
        let neighbours = [
            ("insertion", T::INSERTION),
            ("A", T::BASE_A),
            ("C", T::BASE_C),
            ("G", T::BASE_G),
            ("T", T::BASE_T),
            ("selection", T::INTERACTION_SELECTED),
            ("hover", T::INTERACTION_HOVERED),
            ("read body", T::READ_NEUTRAL),
        ];
        for theme in ThemeId::ALL {
            let palette = AlignmentPalette::for_theme(theme);
            // Normal vision everywhere; the colour-blind palettes also under simulated
            // protanopia and deuteranopia. Ten is roughly four just-noticeable differences.
            let mut floors = vec![(&NORMAL, 20.0)];
            if matches!(theme, ThemeId::LightColorBlind | ThemeId::DarkColorBlind) {
                floors.extend([(&PROTANOPIA, 15.0), (&DEUTERANOPIA, 15.0)]);
            }
            for (vision, floor) in floors {
                for (index, (name, token)) in modifications.iter().enumerate() {
                    let color = palette.color(*token);
                    for (other, neighbour) in neighbours {
                        let delta = delta_e(color, palette.color(neighbour), vision);
                        assert!(
                            delta >= floor,
                            "{theme:?}: {name} is only {delta:.1} from {other}"
                        );
                    }
                    for (other, token) in &modifications[index + 1..] {
                        let delta = delta_e(color, palette.color(*token), vision);
                        assert!(
                            delta >= floor,
                            "{theme:?}: {name} is only {delta:.1} from {other}"
                        );
                    }
                }
            }
            for (name, token) in modifications {
                let full = palette.color(token);
                assert!((full[3] - 1.0).abs() < 1.0e-6, "{theme:?}: {name}");
                // The faint end of each ramp is the same colour, translucent.
                let faint = palette.color(PaletteToken(token.0 - 1));
                assert_same(
                    [faint[0], faint[1], faint[2], 1.0],
                    [full[0], full[1], full[2], 1.0],
                );
                assert!((faint[3] - MODIFICATION_FAINT_ALPHA).abs() < 1.0e-6);
            }
            // The summary lane draws in the read stack's colours.
            let summary = palette.color(T::SUMMARY_MODIFICATION);
            assert_same(
                [summary[0], summary[1], summary[2], 1.0],
                palette.color(T::MODIFICATION_5MC),
            );
            let background = palette.color(T::SUMMARY_MODIFICATION_BACKGROUND);
            assert_same(
                [background[0], background[1], background[2], 1.0],
                palette.color(T::MODIFICATION_UNMODIFIED),
            );
        }
    }

    #[test]
    fn dark_palettes_keep_closed_categories_distinct_and_off_the_canvas() {
        use PaletteToken as T;
        for theme in [ThemeId::Dark, ThemeId::DarkColorBlind] {
            let full = Theme::new(theme);
            let palette = &full.palette;
            let canvas = [
                f32::from(full.canvas[0]) / 255.0,
                f32::from(full.canvas[1]) / 255.0,
                f32::from(full.canvas[2]) / 255.0,
                1.0,
            ];
            assert_pairwise_distinct("dark base", &run(palette, T::BASE_A, 5), 0.20);
            for slot in 1..=T::SPECIALIST_UNAVAILABLE.index() {
                let separation = distance(palette.color(PaletteToken(slot)), canvas);
                assert!(
                    separation >= 0.15,
                    "{theme:?} slot {slot} is only {separation} from the canvas"
                );
            }
            for mode in 0..4 {
                assert!(
                    distance(
                        palette.color(T::specialist_body(mode, false)),
                        palette.color(T::specialist_body(mode, true)),
                    ) >= 0.25
                );
            }
        }
    }
}
