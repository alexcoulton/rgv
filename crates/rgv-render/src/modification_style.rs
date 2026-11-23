//! How base-modification calls (`MM`/`ML` tags) are coloured and named: the read-stack cells,
//! the modification summary lane, the exports and the legends all take their colours from here.
//!
//! The convention follows IGV. A cell shows the likelier state of its base: a modification in
//! its type's colour (5mC red, 5hmC amber, 6mA teal, any other code cream; every modification
//! red outside the by-type mode), or no modification in blue. The colour's strength is the
//! call's confidence: a call whose two states are equally likely is a faint tint that fades
//! towards the read body beneath it, and a certain call is the full colour.
//!
//! A mismatch takes priority over a modification on the same base: the semantic page emits no
//! modification cell on a base drawn as a mismatch, so a cell is never two marks at once.

use rgv_alignment_scene::ModificationState;
use rgv_core::ModifiedBaseColorMode;
use rgv_model::ModificationCode;

use crate::palette::{PaletteColor, PaletteToken};

/// The colour classes of modification codes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ModificationColorClass {
    FiveMethylC,
    FiveHydroxymethylC,
    SixMethylA,
    Other,
}

impl ModificationColorClass {
    /// The full colour's palette slot; the faint end of its ramp is the slot before.
    #[must_use]
    pub const fn token(self) -> PaletteToken {
        match self {
            Self::FiveMethylC => PaletteToken::MODIFICATION_5MC,
            Self::FiveHydroxymethylC => PaletteToken::MODIFICATION_5HMC,
            Self::SixMethylA => PaletteToken::MODIFICATION_6MA,
            Self::Other => PaletteToken::MODIFICATION_OTHER,
        }
    }

    const fn faint_token(self) -> PaletteToken {
        match self {
            Self::FiveMethylC => PaletteToken::MODIFICATION_5MC_FAINT,
            Self::FiveHydroxymethylC => PaletteToken::MODIFICATION_5HMC_FAINT,
            Self::SixMethylA => PaletteToken::MODIFICATION_6MA_FAINT,
            Self::Other => PaletteToken::MODIFICATION_OTHER_FAINT,
        }
    }
}

/// `ChEBI` identifiers the SAM specification lists for the single-letter codes.
const CHEBI_5MC: u32 = 27_551;
const CHEBI_5HMC: u32 = 76_792;
const CHEBI_6MA: u32 = 28_871;

/// The colour class of one `MM` modification code; letter codes and their `ChEBI` equivalents
/// agree.
#[must_use]
pub const fn modification_color_class(code: ModificationCode) -> ModificationColorClass {
    match code {
        ModificationCode::Letter(b'm') | ModificationCode::Chebi(CHEBI_5MC) => {
            ModificationColorClass::FiveMethylC
        }
        ModificationCode::Letter(b'h') | ModificationCode::Chebi(CHEBI_5HMC) => {
            ModificationColorClass::FiveHydroxymethylC
        }
        ModificationCode::Letter(b'a') | ModificationCode::Chebi(CHEBI_6MA) => {
            ModificationColorClass::SixMethylA
        }
        _ => ModificationColorClass::Other,
    }
}

/// The palette slot a modification of `code` is drawn in by the by-type mode (read stack and
/// summary lane alike).
#[must_use]
pub const fn modification_type_token(code: ModificationCode) -> PaletteToken {
    modification_color_class(code).token()
}

/// The conventional short name of a modification code (`m` is "5mC"), else the code as written
/// in the `MM` tag.
#[must_use]
pub fn modification_code_name(code: ModificationCode) -> String {
    let name = match code {
        ModificationCode::Letter(b'm') | ModificationCode::Chebi(CHEBI_5MC) => "5mC",
        ModificationCode::Letter(b'h') | ModificationCode::Chebi(CHEBI_5HMC) => "5hmC",
        ModificationCode::Letter(b'f') | ModificationCode::Chebi(76_794) => "5fC",
        ModificationCode::Letter(b'c') | ModificationCode::Chebi(76_793) => "5caC",
        ModificationCode::Chebi(21_839) => "4mC",
        ModificationCode::Letter(b'g') | ModificationCode::Chebi(16_964) => "5hmU",
        ModificationCode::Letter(b'e') | ModificationCode::Chebi(80_961) => "5fU",
        ModificationCode::Letter(b'b') | ModificationCode::Chebi(17_477) => "5caU",
        ModificationCode::Letter(b'a') | ModificationCode::Chebi(CHEBI_6MA) => "6mA",
        ModificationCode::Chebi(17_596) => "inosine",
        ModificationCode::Letter(b'o') | ModificationCode::Chebi(44_605) => "8oxoG",
        ModificationCode::Letter(b'n') | ModificationCode::Chebi(18_107) => "Xao",
        ModificationCode::Letter(letter) if letter.is_ascii_uppercase() => {
            return format!("any {} modification", char::from(letter));
        }
        ModificationCode::Letter(letter) => return char::from(letter).to_string(),
        ModificationCode::Chebi(chebi) => return format!("CHEBI:{chebi}"),
    };
    name.to_owned()
}

/// Confidence of a call whose shown state has probability `probability` of 255: zero when both
/// states are equally likely, one when the shown state is certain.
fn confidence(probability: u8) -> f32 {
    ((2.0 * f32::from(probability) - 255.0) / 255.0).clamp(0.0, 1.0)
}

/// The colour of one modification cell of the read stack.
///
/// * `Modified`: `probability` is the shown code's `ML` value (no `ML` means certain); the
///   colour is the code's type colour when `by_type`, else the 5mC red.
/// * `Canonical`: `probability` is the summed probability of every modification at the base,
///   so no modification has `255 - probability`; `None` is an implicitly unmodified base.
///
/// The colour ramps from the class's faint slot to its full slot by the confidence of the shown
/// state, so low-confidence calls fade towards the read body.
#[must_use]
pub fn modification_cell_color(
    code: ModificationCode,
    probability: Option<u8>,
    state: ModificationState,
    by_type: bool,
) -> PaletteColor {
    let (faint, shown) = match state {
        ModificationState::Modified => (
            if by_type {
                modification_color_class(code).faint_token()
            } else {
                PaletteToken::MODIFICATION_5MC_FAINT
            },
            probability.unwrap_or(u8::MAX),
        ),
        ModificationState::Canonical => (
            PaletteToken::MODIFICATION_UNMODIFIED_FAINT,
            u8::MAX - probability.unwrap_or(0),
        ),
    };
    // The full slot is the one after the faint slot.
    PaletteColor::Ramp {
        low: faint,
        position: confidence(shown),
    }
}

/// One entry of a modification legend: a swatch in `token`'s colour and its label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModificationLegendEntry {
    pub token: PaletteToken,
    pub label: String,
}

/// What the read stack's modification colours mean, for a legend's hover text.
pub const MODIFICATION_LEGEND_HELP: &str = "Base modifications from the MM/ML tags. Each cell shows the likelier state of its base; \
its colour strength is the call probability (ML), so low-confidence calls fade towards the \
read. A mismatch is drawn instead of a modification on the same base. Reads without MM/ML \
tags (often supplementary alignments) are drawn normally.";

/// The legend of the read stack's modification colours for the codes `codes` present in the
/// data (in their order, hidden codes already removed) under `mode`. Empty when there is
/// nothing to explain.
#[must_use]
pub fn modification_legend(
    codes: &[ModificationCode],
    mode: ModifiedBaseColorMode,
) -> Vec<ModificationLegendEntry> {
    if codes.is_empty() {
        return Vec::new();
    }
    let mut entries = Vec::with_capacity(codes.len() + 1);
    match mode {
        ModifiedBaseColorMode::ByType => {
            for code in codes {
                entries.push(ModificationLegendEntry {
                    token: modification_type_token(*code),
                    label: modification_code_name(*code),
                });
            }
        }
        ModifiedBaseColorMode::Mono | ModifiedBaseColorMode::ModifiedVsUnmodified => {
            let names = codes
                .iter()
                .map(|code| modification_code_name(*code))
                .collect::<Vec<_>>()
                .join(", ");
            entries.push(ModificationLegendEntry {
                token: PaletteToken::MODIFICATION_5MC,
                label: format!("{names} modified"),
            });
        }
    }
    entries.push(ModificationLegendEntry {
        token: PaletteToken::MODIFICATION_UNMODIFIED,
        label: "unmodified".to_owned(),
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::AlignmentPalette;

    const M: ModificationCode = ModificationCode::Letter(b'm');
    const H: ModificationCode = ModificationCode::Letter(b'h');
    const A: ModificationCode = ModificationCode::Letter(b'a');
    const F: ModificationCode = ModificationCode::Letter(b'f');

    fn resolved(color: PaletteColor) -> [f32; 4] {
        AlignmentPalette::light().resolve(color)
    }

    #[test]
    fn modification_cells_are_coloured_by_type_and_state() {
        let palette = AlignmentPalette::light();
        let full = |token| palette.color(token);
        let certain = |code, by_type| {
            resolved(modification_cell_color(
                code,
                Some(255),
                ModificationState::Modified,
                by_type,
            ))
        };
        assert_eq!(certain(M, true), full(PaletteToken::MODIFICATION_5MC));
        assert_eq!(certain(H, true), full(PaletteToken::MODIFICATION_5HMC));
        assert_eq!(certain(A, true), full(PaletteToken::MODIFICATION_6MA));
        assert_eq!(certain(F, true), full(PaletteToken::MODIFICATION_OTHER));
        assert_eq!(
            certain(ModificationCode::Chebi(CHEBI_5HMC), true),
            full(PaletteToken::MODIFICATION_5HMC)
        );
        // Outside the by-type mode every modification is the 5mC red.
        for code in [M, H, A, F] {
            assert_eq!(certain(code, false), full(PaletteToken::MODIFICATION_5MC));
        }
        // A missing ML value is a certain call; an implicitly unmodified base is certain blue.
        assert_eq!(
            resolved(modification_cell_color(
                H,
                None,
                ModificationState::Modified,
                true
            )),
            full(PaletteToken::MODIFICATION_5HMC)
        );
        assert_eq!(
            resolved(modification_cell_color(
                M,
                None,
                ModificationState::Canonical,
                true
            )),
            full(PaletteToken::MODIFICATION_UNMODIFIED)
        );
    }

    #[test]
    fn colour_strength_is_the_call_probability() {
        let palette = AlignmentPalette::light();
        let alpha = |probability, state| {
            resolved(modification_cell_color(M, Some(probability), state, true))[3]
        };
        let faint = crate::palette::MODIFICATION_FAINT_ALPHA;
        // High ML: nearly the full colour. A call at one half: the faint tint.
        assert!(alpha(250, ModificationState::Modified) > 0.95);
        assert!((alpha(128, ModificationState::Modified) - faint).abs() < 0.01);
        // Strength rises monotonically with the probability of the shown state.
        let mut previous = 0.0;
        for probability in (128..=255).step_by(8) {
            let current = alpha(probability, ModificationState::Modified);
            assert!(current >= previous, "{probability}");
            previous = current;
        }
        // An unmodified call is strong when every modification is unlikely.
        assert!(alpha(5, ModificationState::Canonical) > 0.95);
        assert!(alpha(120, ModificationState::Canonical) < 0.25);
        // The hue never changes with probability: only the alpha moves.
        let strong = resolved(modification_cell_color(
            M,
            Some(240),
            ModificationState::Modified,
            true,
        ));
        let weak = resolved(modification_cell_color(
            M,
            Some(140),
            ModificationState::Modified,
            true,
        ));
        assert_eq!(strong[..3], weak[..3]);
        assert_eq!(
            strong[..3],
            palette.color(PaletteToken::MODIFICATION_5MC)[..3]
        );
        assert!(weak[3] < strong[3]);
    }

    #[test]
    fn the_legend_lists_only_the_codes_present() {
        let legend = modification_legend(&[M, A], ModifiedBaseColorMode::ByType);
        assert_eq!(
            legend,
            vec![
                ModificationLegendEntry {
                    token: PaletteToken::MODIFICATION_5MC,
                    label: "5mC".to_owned()
                },
                ModificationLegendEntry {
                    token: PaletteToken::MODIFICATION_6MA,
                    label: "6mA".to_owned()
                },
                ModificationLegendEntry {
                    token: PaletteToken::MODIFICATION_UNMODIFIED,
                    label: "unmodified".to_owned()
                },
            ]
        );
        let two_colour = modification_legend(&[M, H], ModifiedBaseColorMode::ModifiedVsUnmodified);
        assert_eq!(two_colour.len(), 2);
        assert_eq!(two_colour[0].label, "5mC, 5hmC modified");
        assert_eq!(two_colour[0].token, PaletteToken::MODIFICATION_5MC);
        assert_eq!(two_colour[1].token, PaletteToken::MODIFICATION_UNMODIFIED);
        assert!(modification_legend(&[], ModifiedBaseColorMode::ByType).is_empty());
    }

    #[test]
    fn code_names_follow_the_sam_specification() {
        assert_eq!(modification_code_name(M), "5mC");
        assert_eq!(modification_code_name(H), "5hmC");
        assert_eq!(modification_code_name(A), "6mA");
        assert_eq!(
            modification_code_name(ModificationCode::Chebi(21_839)),
            "4mC"
        );
        assert_eq!(
            modification_code_name(ModificationCode::Chebi(1)),
            "CHEBI:1"
        );
        assert_eq!(
            modification_code_name(ModificationCode::Letter(b'C')),
            "any C modification"
        );
        assert_eq!(modification_code_name(ModificationCode::Letter(b'z')), "z");
    }
}
