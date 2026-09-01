use super::super::*;

pub(crate) const fn read_base_display_mode_label(mode: ReadBaseDisplayMode) -> &'static str {
    match mode {
        ReadBaseDisplayMode::Mismatches => "Mismatches only",
        ReadBaseDisplayMode::All => "All bases",
        ReadBaseDisplayMode::Hidden => "Hidden",
    }
}

pub(crate) const fn modified_base_color_mode_label(mode: ModifiedBaseColorMode) -> &'static str {
    match mode {
        ModifiedBaseColorMode::ByType => "By modification type",
        ModifiedBaseColorMode::Mono => "Single colour",
        ModifiedBaseColorMode::ModifiedVsUnmodified => "Modified vs unmodified",
    }
}

pub(crate) fn modification_code_label(code: ModificationCode) -> String {
    match code {
        ModificationCode::Letter(letter) => char::from(letter).to_string(),
        ModificationCode::Chebi(code) => format!("CHEBI:{code}"),
    }
}

pub(crate) fn parse_modification_code(value: &str) -> Option<ModificationCode> {
    if let Some(code) = value.strip_prefix("CHEBI:") {
        return code.parse().ok().map(ModificationCode::Chebi);
    }
    let bytes = value.as_bytes();
    (bytes.len() == 1 && bytes[0].is_ascii_alphabetic()).then(|| ModificationCode::Letter(bytes[0]))
}

pub(crate) fn rgba_to_color32(color: rgv_layout::Rgba) -> egui::Color32 {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(
        channel(color.red),
        channel(color.green),
        channel(color.blue),
        channel(color.alpha),
    )
}

fn collect_scene_modification_codes(snapshot: &AlignmentSceneSnapshot) -> Vec<ModificationCode> {
    let mut codes = std::collections::BTreeSet::new();
    for tile in snapshot.summaries().modification_tiles() {
        for series in &tile.summary().series {
            codes.insert(series.key.code);
            if codes.len() == MAX_HIDDEN_MODIFICATION_CODES {
                return codes.into_iter().collect();
            }
        }
    }
    if let Ok(discovered) = snapshot
        .reads()
        .modification_codes(10_000, MAX_HIDDEN_MODIFICATION_CODES)
    {
        for code in discovered {
            codes.insert(code);
            if codes.len() == MAX_HIDDEN_MODIFICATION_CODES {
                break;
            }
        }
    }
    codes.into_iter().collect()
}

impl Viewer {
    pub(crate) fn available_modification_codes(
        &mut self,
        track_id: TrackId,
    ) -> Vec<ModificationCode> {
        let panel_id = self.panels.active().id;
        let Some(authority) = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id && track.scene_engine.is_some())
            .and_then(|_| self.presented_alignment_lane(panel_id, track_id))
            .filter(|presented| {
                presented.lane.summary_current() && presented.source.semantic_target_complete
            })
            .and_then(|presented| presented.lane.snapshot.as_ref())
            .cloned()
        else {
            self.modification_code_cache = None;
            return Vec::new();
        };
        if let Some(cache) = &self.modification_code_cache
            && cache.panel_id == panel_id
            && cache.track_id == track_id
            && Arc::ptr_eq(&cache.authority, &authority)
        {
            return cache.codes.clone();
        }
        let codes = collect_scene_modification_codes(&authority);
        self.modification_code_cache = Some(CachedModificationCodes {
            panel_id,
            track_id,
            authority,
            codes: codes.clone(),
        });
        codes
    }

    pub(crate) fn alignment_pair_summary_for_track(
        &self,
        track_id: TrackId,
    ) -> Option<rgv_hts::PairSummary> {
        let panel_id = self.panels.active().id;
        let track = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)?;
        if track.scene_engine.is_none() {
            return None;
        }
        let presented = self.presented_alignment_lane(panel_id, track_id)?;
        if !presented.lane.summary_current() || !presented.source.semantic_target_complete {
            return None;
        }
        Some(
            presented
                .lane
                .snapshot
                .as_ref()?
                .summaries()
                .prepared_pair_summary(),
        )
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn alignment_style_controls(
    ui: &mut egui::Ui,
    color_mode: &mut AlignmentColorMode,
    group_mode: &mut AlignmentGroupMode,
    group_order: &mut AlignmentOrderDirection,
    sort_mode: &mut AlignmentSortMode,
    sort_order: &mut AlignmentOrderDirection,
    style_tag: &mut String,
) {
    egui::Grid::new("alignment-style-grid")
        .num_columns(2)
        .show(ui, |ui| {
            let label = ui.label("Colour");
            egui::ComboBox::from_id_salt("alignment-color-mode")
                .selected_text(alignment_color_mode_label(*color_mode))
                .show_ui(ui, |ui| {
                    ui.selectable_value(color_mode, AlignmentColorMode::None, "IGV grey");
                    ui.selectable_value(color_mode, AlignmentColorMode::Strand, "Strand");
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::MappingQuality,
                        "Mapping quality",
                    );
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::ReadGroup,
                        "Read group (RG)",
                    );
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::Haplotype,
                        "Haplotype (HP)",
                    );
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::TemplateLength,
                        "Template length",
                    );
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::PairOrientation,
                        "Pair orientation",
                    );
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::UnexpectedPair,
                        "Unexpected pair",
                    );
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::MateContig,
                        "Mate chromosome",
                    );
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::FirstOfPairStrand,
                        "First-of-pair strand",
                    );
                    ui.separator();
                    for context in [
                        BisulfiteContext::Cg,
                        BisulfiteContext::Chg,
                        BisulfiteContext::Chh,
                        BisulfiteContext::Hcg,
                        BisulfiteContext::Gch,
                        BisulfiteContext::Wcg,
                    ] {
                        ui.selectable_value(
                            color_mode,
                            AlignmentColorMode::Bisulfite(context),
                            format!("Bisulfite · {}", bisulfite_context_label(context)),
                        );
                    }
                    ui.separator();
                    for mode in [
                        SmrtKineticsMode::SubreadIpd,
                        SmrtKineticsMode::SubreadPulseWidth,
                        SmrtKineticsMode::CcsForwardIpd,
                        SmrtKineticsMode::CcsForwardPulseWidth,
                        SmrtKineticsMode::CcsReverseIpd,
                        SmrtKineticsMode::CcsReversePulseWidth,
                    ] {
                        ui.selectable_value(
                            color_mode,
                            AlignmentColorMode::SmrtKinetics(mode),
                            format!("PacBio SMRT · {}", smrt_kinetics_mode_label(mode)),
                        );
                    }
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::Ultima(UltimaDisplayOptions::default()),
                        "Ultima flow indels",
                    );
                    ui.selectable_value(
                        color_mode,
                        AlignmentColorMode::Sbx(SbxDisplayOptions::default()),
                        "SBX evidence",
                    );
                    ui.separator();
                    let tag = parse_style_tag(style_tag).unwrap_or(*b"NM");
                    ui.selectable_value(color_mode, AlignmentColorMode::Tag(tag), "Custom tag");
                })
                .response
                .labelled_by(label.id);
            ui.end_row();

            let label = ui.label("Group");
            egui::ComboBox::from_id_salt("alignment-group-mode")
                .selected_text(format!("{group_mode:?}"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(group_mode, AlignmentGroupMode::None, "None");
                    ui.selectable_value(group_mode, AlignmentGroupMode::Strand, "Strand");
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::ReadGroup,
                        "Read group (RG)",
                    );
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::Haplotype,
                        "Haplotype (HP)",
                    );
                    ui.selectable_value(group_mode, AlignmentGroupMode::Chimeric, "Chimeric (SA)");
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::Supplementary,
                        "Supplementary",
                    );
                    ui.selectable_value(group_mode, AlignmentGroupMode::LeftClip, "Left clipped");
                    ui.selectable_value(group_mode, AlignmentGroupMode::RightClip, "Right clipped");
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::PairOrientation,
                        "Pair orientation",
                    );
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::UnexpectedPair,
                        "Unexpected pair",
                    );
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::MateContig,
                        "Mate chromosome",
                    );
                    ui.selectable_value(group_mode, AlignmentGroupMode::ReadOrder, "Read order");
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::FirstOfPairStrand,
                        "First-of-pair strand",
                    );
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::Base,
                        "Base at fixed position",
                    );
                    ui.selectable_value(
                        group_mode,
                        AlignmentGroupMode::Insertion,
                        "Insertion at fixed position",
                    );
                    let tag = parse_style_tag(style_tag).unwrap_or(*b"NM");
                    ui.selectable_value(group_mode, AlignmentGroupMode::Tag(tag), "Custom tag");
                })
                .response
                .labelled_by(label.id);
            ui.end_row();

            ui.label("Group order");
            ui.horizontal(|ui| {
                ui.selectable_value(group_order, AlignmentOrderDirection::Natural, "Natural");
                ui.selectable_value(group_order, AlignmentOrderDirection::Reverse, "Reverse");
            });
            ui.end_row();

            let label = ui.label("Sort");
            egui::ComboBox::from_id_salt("alignment-sort-mode")
                .selected_text(format!("{sort_mode:?}"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(sort_mode, AlignmentSortMode::Start, "Position");
                    ui.selectable_value(sort_mode, AlignmentSortMode::Strand, "Strand");
                    ui.selectable_value(
                        sort_mode,
                        AlignmentSortMode::FirstOfPairStrand,
                        "First-of-pair strand",
                    );
                    ui.selectable_value(
                        sort_mode,
                        AlignmentSortMode::MappingQuality,
                        "Mapping quality",
                    );
                    ui.selectable_value(sort_mode, AlignmentSortMode::ReadName, "Read name");
                    ui.selectable_value(sort_mode, AlignmentSortMode::ReadGroup, "Read group (RG)");
                    ui.selectable_value(sort_mode, AlignmentSortMode::Haplotype, "Haplotype (HP)");
                    ui.selectable_value(
                        sort_mode,
                        AlignmentSortMode::AlignedLength,
                        "Aligned length",
                    );
                    ui.selectable_value(sort_mode, AlignmentSortMode::LeftClip, "Left clip");
                    ui.selectable_value(sort_mode, AlignmentSortMode::RightClip, "Right clip");
                    ui.selectable_value(
                        sort_mode,
                        AlignmentSortMode::TemplateLength,
                        "Template length",
                    );
                    ui.selectable_value(
                        sort_mode,
                        AlignmentSortMode::PairOrientation,
                        "Pair orientation",
                    );
                    ui.selectable_value(
                        sort_mode,
                        AlignmentSortMode::MateContig,
                        "Mate chromosome",
                    );
                    ui.selectable_value(sort_mode, AlignmentSortMode::PairStatus, "Pair status");
                    ui.selectable_value(sort_mode, AlignmentSortMode::ReadOrder, "Read order");
                    ui.selectable_value(sort_mode, AlignmentSortMode::Base, "Base at centre");
                    let tag = parse_style_tag(style_tag).unwrap_or(*b"NM");
                    ui.selectable_value(sort_mode, AlignmentSortMode::Tag(tag), "Custom tag");
                })
                .response
                .labelled_by(label.id);
            ui.end_row();

            ui.label("Sort order");
            ui.horizontal(|ui| {
                ui.selectable_value(sort_order, AlignmentOrderDirection::Natural, "Natural");
                ui.selectable_value(sort_order, AlignmentOrderDirection::Reverse, "Reverse");
                if ui.button("Re-pack").clicked() {
                    *sort_mode = AlignmentSortMode::Start;
                    *sort_order = AlignmentOrderDirection::Natural;
                }
            });
            ui.end_row();

            let label = ui.label("Custom SAM tag");
            ui.add(
                egui::TextEdit::singleline(style_tag)
                    .desired_width(40.0)
                    .char_limit(2),
            )
            .labelled_by(label.id);
            ui.end_row();
        });
    if parse_style_tag(style_tag).is_none() {
        ui.colored_label(egui::Color32::DARK_RED, "Use two ASCII SAM-tag characters");
    }
    if let AlignmentColorMode::Bisulfite(context) = *color_mode {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} context · ", bisulfite_context_label(context)));
            ui.colored_label(egui::Color32::from_rgb(219, 31, 31), "protected");
            ui.label(" / ");
            ui.colored_label(egui::Color32::from_rgb(31, 87, 224), "converted");
            ui.label(" · grey/sage = first-of-pair strand");
            if ui.button("Copy legend").clicked() {
                ui.ctx().copy_text(bisulfite_legend(context));
            }
        });
        ui.small(
            "A reference FASTA is required; without it reads remain visible and conversion state is not inferred.",
        );
    }
    if let AlignmentColorMode::SmrtKinetics(mode) = *color_mode {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} · ", smrt_kinetics_mode_label(mode)));
            ui.colored_label(egui::Color32::from_rgb(221, 37, 29), "0–100 red intensity");
            ui.label(" · ");
            ui.colored_label(
                egui::Color32::from_rgb(239, 185, 20),
                "100–600 red to yellow",
            );
            ui.label(" · ");
            ui.colored_label(
                egui::Color32::from_rgb(26, 204, 210),
                "600–6000 yellow to cyan",
            );
            if ui.button("Copy legend").clicked() {
                ui.ctx().copy_text(smrt_kinetics_legend(mode));
            }
        });
        ui.small(
            "Reads without the required PacBio integer array remain visible in grey; malformed and length-mismatched arrays are reported rather than guessed.",
        );
    }
    if let AlignmentColorMode::Ultima(mut options) = *color_mode {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(rgba_to_color32(ultima_quality_color(0.0)), "Q0 low");
            ui.label(" to ");
            ui.colored_label(rgba_to_color32(ultima_quality_color(21.0)), "Q21");
            ui.label(" to ");
            ui.colored_label(rgba_to_color32(ultima_quality_color(42.0)), "Q42 high");
            ui.label(" · position within the indel marker also shows quality");
            if ui.button("Copy legend").clicked() {
                ui.ctx().copy_text(ultima_legend(options));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Flow quality");
            ui.selectable_value(
                &mut options.quality_mode,
                UltimaQualityMode::Directional,
                "Directional tp",
            );
            ui.selectable_value(
                &mut options.quality_mode,
                UltimaQualityMode::Minimum,
                "Minimum in homopolymer",
            );
        });
        let mut override_maximum = options.maximum_homopolymer_length_override.is_some();
        if ui
            .checkbox(
                &mut override_maximum,
                "Override maximum homopolymer length from read group (mc)",
            )
            .changed()
        {
            options.maximum_homopolymer_length_override = override_maximum.then_some(12);
        }
        if override_maximum {
            let maximum = options
                .maximum_homopolymer_length_override
                .get_or_insert(12);
            ui.add(
                egui::Slider::new(maximum, 1..=1_000)
                    .text("Maximum homopolymer length")
                    .suffix(" bases"),
            );
        } else {
            options.maximum_homopolymer_length_override = None;
        }
        ui.small(
            "Insertions use the signed tp direction array and base qualities. Single-base non-homopolymer deletions use t0; homopolymer deletions also require the matching reference. Missing evidence leaves the ordinary indel visible and is reported explicitly.",
        );
        *color_mode = AlignmentColorMode::Ultima(options);
    }
    if let AlignmentColorMode::Sbx(mut options) = *color_mode {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                egui::Color32::from_rgb(138, 43, 209),
                "purple concordant insertion",
            );
            ui.label(" · ");
            ui.colored_label(
                egui::Color32::from_rgb(140, 140, 140),
                "dark-grey partial discordance",
            );
            ui.label(" · ");
            ui.colored_label(
                egui::Color32::from_rgb(180, 180, 180),
                "light-grey full discordance",
            );
            if ui.button("Copy legend").clicked() {
                ui.ctx().copy_text(sbx_legend(options));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Simplex tails");
            ui.selectable_value(
                &mut options.simplex_tail_mode,
                SbxSimplexTailMode::Off,
                "Off",
            );
            ui.selectable_value(
                &mut options.simplex_tail_mode,
                SbxSimplexTailMode::Highlight,
                "Highlight",
            );
            ui.selectable_value(
                &mut options.simplex_tail_mode,
                SbxSimplexTailMode::Hide,
                "Hide",
            );
        });
        egui::Grid::new("sbx-threshold-grid")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Discordant base Q≤");
                ui.add(egui::Slider::new(
                    &mut options.discordant_quality_maximum,
                    0..=92,
                ));
                ui.end_row();
                options.simplex_tail_quality_maximum = options
                    .simplex_tail_quality_maximum
                    .max(options.discordant_quality_maximum.saturating_add(1));
                ui.label("Simplex-tail base Q≤");
                ui.add(egui::Slider::new(
                    &mut options.simplex_tail_quality_maximum,
                    options.discordant_quality_maximum.saturating_add(1)..=93,
                ));
                ui.end_row();
            });
        ui.small(format!(
            "Tail detection examines contiguous read ends with Q≤{}; insertion evidence also examines inserted, adjacent, and containing-homopolymer bases at Q≤{}.",
            options.simplex_tail_quality_maximum,
            options.discordant_quality_maximum,
        ));
        *color_mode = AlignmentColorMode::Sbx(options);
    }
}

pub(crate) fn alignment_color_mode_label(mode: AlignmentColorMode) -> String {
    match mode {
        AlignmentColorMode::None => "IGV grey".to_owned(),
        AlignmentColorMode::Strand => "Strand".to_owned(),
        AlignmentColorMode::MappingQuality => "Mapping quality".to_owned(),
        AlignmentColorMode::ReadGroup => "Read group (RG)".to_owned(),
        AlignmentColorMode::Haplotype => "Haplotype (HP)".to_owned(),
        AlignmentColorMode::TemplateLength => "Template length".to_owned(),
        AlignmentColorMode::PairOrientation => "Pair orientation".to_owned(),
        AlignmentColorMode::UnexpectedPair => "Unexpected pair".to_owned(),
        AlignmentColorMode::MateContig => "Mate chromosome".to_owned(),
        AlignmentColorMode::FirstOfPairStrand => "First-of-pair strand".to_owned(),
        AlignmentColorMode::Bisulfite(context) => {
            format!("Bisulfite · {}", bisulfite_context_label(context))
        }
        AlignmentColorMode::SmrtKinetics(mode) => {
            format!("PacBio SMRT · {}", smrt_kinetics_mode_label(mode))
        }
        AlignmentColorMode::Ultima(_) => "Ultima flow indels".to_owned(),
        AlignmentColorMode::Sbx(_) => "SBX evidence".to_owned(),
        AlignmentColorMode::Tag(tag) => {
            format!("Custom tag {}{}", char::from(tag[0]), char::from(tag[1]))
        }
    }
}

pub(crate) const fn smrt_kinetics_mode_label(mode: SmrtKineticsMode) -> &'static str {
    match mode {
        SmrtKineticsMode::SubreadIpd => "Subread IPD (ip)",
        SmrtKineticsMode::SubreadPulseWidth => "Subread pulse width (pw)",
        SmrtKineticsMode::CcsForwardIpd => "CCS forward-strand IPD (fi/ri)",
        SmrtKineticsMode::CcsForwardPulseWidth => "CCS forward-strand pulse width (fp/rp)",
        SmrtKineticsMode::CcsReverseIpd => "CCS reverse-strand IPD (ri/fi)",
        SmrtKineticsMode::CcsReversePulseWidth => "CCS reverse-strand pulse width (rp/fp)",
    }
}

pub(crate) fn smrt_kinetics_legend(mode: SmrtKineticsMode) -> String {
    format!(
        "RGV PacBio SMRT kinetics: {}\n0–100: increasing red opacity\n100–600: red to yellow\n600–6000: yellow to cyan\nValues above 6000: cyan\nSubread tags: ip = IPD, pw = pulse width\nCCS tags: fi/ri = forward/reverse IPD, fp/rp = forward/reverse pulse width; RGV swaps the selected physical tag for reverse-aligned records so the requested molecule strand remains consistent.\n",
        smrt_kinetics_mode_label(mode)
    )
}

pub(crate) fn ultima_legend(options: UltimaDisplayOptions) -> String {
    format!(
        "RGV Ultima flow-indel evidence\nQuality method: {}\nColour: 44-step jet scale from Q0 blue through cyan/yellow to Q42 red\nInsertion: horizontal quality line; Q0 at the bottom, Q42 at the top\nDeletion: vertical quality line; Q0 at the left, Q42 at the right\nDirection: negative for insertions and positive for deletions; inspect the event for the exact value\nMaximum homopolymer length: {}\nTags: tp = signed flow direction, t0 = printable Phred+33 deletion quality; base QUAL supplies homopolymer-event quality\n",
        ultima_quality_mode_label(options.quality_mode),
        options.maximum_homopolymer_length_override.map_or_else(
            || "read-group mc (when present)".to_owned(),
            |value| format!("{value} bases (track override)"),
        ),
    )
}

pub(crate) const fn ultima_quality_mode_label(mode: UltimaQualityMode) -> &'static str {
    match mode {
        UltimaQualityMode::Directional => "directional tp match",
        UltimaQualityMode::Minimum => "minimum base quality in the homopolymer",
    }
}

pub(crate) const fn ultima_indel_kind_label(kind: UltimaIndelKind) -> &'static str {
    match kind {
        UltimaIndelKind::Insertion => "insertion",
        UltimaIndelKind::Deletion => "deletion",
    }
}

pub(crate) const fn ultima_quality_source_label(source: UltimaQualitySource) -> &'static str {
    match source {
        UltimaQualitySource::TpDirection => "base QUAL at matching tp direction",
        UltimaQualitySource::TpReadMaximumFallback => {
            "maximum read QUAL fallback (no exact tp direction)"
        }
        UltimaQualitySource::MinimumHomopolymer => "minimum base QUAL in homopolymer",
        UltimaQualitySource::T0 => "t0 deletion quality",
        UltimaQualitySource::MaximumHomopolymerLimit => "at/above maximum homopolymer length",
    }
}

pub(crate) const fn ultima_event_unavailable_label(reason: UltimaEventUnavailable) -> &'static str {
    match reason {
        UltimaEventUnavailable::NotBracketedByAlignedBases => {
            "event is not bracketed by aligned blocks"
        }
        UltimaEventUnavailable::ReferenceUnavailable => "matching reference bases are unavailable",
        UltimaEventUnavailable::AmbiguousHomopolymer => "event matches homopolymers on both sides",
        UltimaEventUnavailable::NonHomopolymer => "event is not a supported homopolymer event",
        UltimaEventUnavailable::T0Unavailable => "t0 deletion quality is unavailable",
    }
}

pub(crate) fn sbx_legend(options: SbxDisplayOptions) -> String {
    format!(
        "RGV SBX evidence mode\nPurple insertion: concordant\nDark-grey insertion: at least one inserted, adjacent, or containing-homopolymer base has Q≤{}\nLight-grey insertion: every inserted base has Q≤{}\nSimplex tails: {}; contiguous end bases with Q≤{}\n",
        options.discordant_quality_maximum,
        options.discordant_quality_maximum,
        sbx_simplex_tail_mode_label(options.simplex_tail_mode),
        options.simplex_tail_quality_maximum,
    )
}

pub(crate) const fn sbx_simplex_tail_mode_label(mode: SbxSimplexTailMode) -> &'static str {
    match mode {
        SbxSimplexTailMode::Off => "off",
        SbxSimplexTailMode::Highlight => "highlighted",
        SbxSimplexTailMode::Hide => "hidden",
    }
}

pub(crate) const fn bisulfite_context_label(context: BisulfiteContext) -> &'static str {
    match context {
        BisulfiteContext::Cg => "CG",
        BisulfiteContext::Chg => "CHG",
        BisulfiteContext::Chh => "CHH",
        BisulfiteContext::Hcg => "HCG",
        BisulfiteContext::Gch => "GCH",
        BisulfiteContext::Wcg => "WCG",
    }
}

pub(crate) fn bisulfite_legend(context: BisulfiteContext) -> String {
    format!(
        "RGV bisulfite mode: {} context\nRed: protected/nonconverted C (forward) or G (reverse)\nBlue: converted T (forward) or A (reverse)\nGrey read: forward first-of-pair strand\nSage read: reverse first-of-pair strand\nH = A/C/T; W = A/T\n",
        bisulfite_context_label(context)
    )
}
