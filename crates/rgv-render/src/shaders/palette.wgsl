// Shared by the alignment read and summary pipelines: the semantic palette uniform (group 1) and
// the rule that turns an instance's palette index and parameter word into a colour. Mirrored by
// `AlignmentPalette::resolve` in `palette.rs`; a test compares the token constants below with
// `PaletteToken`.

struct Palette {
    colors: array<vec4<f32>, 128>,
};

@group(1) @binding(0)
var<uniform> palette: Palette;

const TOKEN_BASE_A: u32 = 1u;
const TOKEN_BASE_N: u32 = 5u;
const TOKEN_BISULFITE_PROTECTED: u32 = 39u;
const TOKEN_BISULFITE_CONVERTED: u32 = 40u;
const TOKEN_INTERACTION_SELECTED: u32 = 47u;
const TOKEN_INTERACTION_HOVERED: u32 = 48u;
const TOKEN_INTERACTION_SELECTED_CHAIN: u32 = 49u;
const TOKEN_INTERACTION_HIGHLIGHTED: u32 = 50u;
const TOKEN_SUMMARY_DEPTH: u32 = 51u;
const TOKEN_SUMMARY_MODIFICATION: u32 = 52u;
const TOKEN_SUMMARY_MODIFICATION_BACKGROUND: u32 = 53u;
const TOKEN_JUNCTION_POSITIVE: u32 = 54u;
const TOKEN_JUNCTION_NEGATIVE: u32 = 55u;
const TOKEN_JUNCTION_UNKNOWN: u32 = 56u;
const TOKEN_JUNCTION_ANNOTATED: u32 = 58u;
const TOKEN_JUNCTION_NOVEL: u32 = 59u;
const TOKEN_SUMMARY_NEUTRAL: u32 = 60u;
const TOKEN_JUNCTION_INTERACTION: u32 = 61u;

fn palette_slot(token: u32) -> vec4<f32> {
    return palette.colors[token & 127u];
}

// `index` is the eight-bit palette index of an instance and `parameter` its colour word:
//   slot 0            inline colour, `parameter` is RGBA8 with red in the low byte;
//   slot n            `palette.colors[n]`;
//   slot n | 0x80     ramp from slot n to slot n + 1, `parameter` is the f32 bits of the position.
fn palette_color(index: u32, parameter: u32) -> vec4<f32> {
    let slot = index & 127u;
    if slot == 0u {
        return vec4<f32>(
            f32(parameter & 255u),
            f32((parameter >> 8u) & 255u),
            f32((parameter >> 16u) & 255u),
            f32(parameter >> 24u),
        ) / 255.0;
    }
    let low = palette.colors[slot];
    if (index & 128u) == 0u {
        return low;
    }
    let high = palette.colors[(slot + 1u) & 127u];
    // Deliberately unclamped: positions past either end extrapolate and the colour target
    // clamps the result. Packers never emit a non-finite position.
    let position = bitcast<f32>(parameter);
    return low + (high - low) * position;
}
