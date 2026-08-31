#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl Rgba {
    pub const fn new(red: f32, green: f32, blue: f32, alpha: f32) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectPrimitive {
    /// Horizontal position in logical pixels.
    pub x: f32,
    /// Vertical position in logical pixels.
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Rgba,
    pub object_id: u32,
    /// Semantic geometry used consistently by interactive and export renderers.
    pub shape: RectShape,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u32)]
pub enum RectShape {
    #[default]
    Rectangle = 0,
    DirectionalTerminalLeft = 1,
    DirectionalTerminalRight = 2,
    ChevronLeft = 3,
    ChevronRight = 4,
    BreakendJoinBefore = 5,
    BreakendJoinAfter = 6,
    /// A smooth splice-junction arc whose endpoints meet the bottom edge.
    JunctionArcUp = 7,
    /// A smooth splice-junction arc whose endpoints meet the top edge.
    JunctionArcDown = 8,
    /// Transient upward Sashimi hover overlay.
    JunctionArcHoverUp = 9,
    /// Transient downward Sashimi hover overlay.
    JunctionArcHoverDown = 10,
    /// Persistent upward Sashimi selection overlay.
    JunctionArcSelectedUp = 11,
    /// Persistent downward Sashimi selection overlay.
    JunctionArcSelectedDown = 12,
}

impl RectShape {
    pub const fn direction(self) -> Option<Direction> {
        match self {
            Self::DirectionalTerminalLeft | Self::ChevronLeft | Self::BreakendJoinBefore => {
                Some(Direction::Left)
            }
            Self::DirectionalTerminalRight | Self::ChevronRight | Self::BreakendJoinAfter => {
                Some(Direction::Right)
            }
            Self::Rectangle
            | Self::JunctionArcUp
            | Self::JunctionArcDown
            | Self::JunctionArcHoverUp
            | Self::JunctionArcHoverDown
            | Self::JunctionArcSelectedUp
            | Self::JunctionArcSelectedDown => None,
        }
    }

    pub const fn is_junction_arc(self) -> bool {
        matches!(
            self,
            Self::JunctionArcUp
                | Self::JunctionArcDown
                | Self::JunctionArcHoverUp
                | Self::JunctionArcHoverDown
                | Self::JunctionArcSelectedUp
                | Self::JunctionArcSelectedDown
        )
    }

    pub const fn is_junction_arc_up(self) -> bool {
        matches!(
            self,
            Self::JunctionArcUp | Self::JunctionArcHoverUp | Self::JunctionArcSelectedUp
        )
    }

    pub const fn junction_arc_interaction_expansion(self) -> f32 {
        match self {
            Self::JunctionArcHoverUp | Self::JunctionArcHoverDown => 1.5,
            Self::JunctionArcSelectedUp | Self::JunctionArcSelectedDown => 2.0,
            _ => 0.0,
        }
    }

    pub const fn as_junction_arc_hover(self) -> Self {
        match self {
            Self::JunctionArcUp | Self::JunctionArcHoverUp | Self::JunctionArcSelectedUp => {
                Self::JunctionArcHoverUp
            }
            Self::JunctionArcDown | Self::JunctionArcHoverDown | Self::JunctionArcSelectedDown => {
                Self::JunctionArcHoverDown
            }
            _ => self,
        }
    }

    pub const fn as_junction_arc_selected(self) -> Self {
        match self {
            Self::JunctionArcUp | Self::JunctionArcHoverUp | Self::JunctionArcSelectedUp => {
                Self::JunctionArcSelectedUp
            }
            Self::JunctionArcDown | Self::JunctionArcHoverDown | Self::JunctionArcSelectedDown => {
                Self::JunctionArcSelectedDown
            }
            _ => self,
        }
    }
}

/// Stroke width for a junction arc. Arc height already carries the support
/// scale, so deriving the stroke from it keeps both visual channels in sync
/// without expanding the dominant rectangle instance format.
pub fn junction_arc_stroke_width(height: f32) -> f32 {
    (1.0 + height.max(0.0) * 0.075).clamp(1.25, 4.5)
}

/// One screen-space sample of the exact Sashimi curve used for rendering and picking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JunctionArcGeometrySample {
    /// Logical screen-space Y coordinate of the sampled curve centre.
    pub curve_y: f32,
    /// Local `dy/dx` slope at the sample or bounded endpoint.
    pub slope: f32,
    /// Normalised stroke distance, or Euclidean distance beyond an endpoint.
    pub distance: f32,
    pub bounded_to_endpoint: bool,
}

/// Samples the Sashimi centre line at a logical point. Points beyond either
/// endpoint use bounded Euclidean endpoint distance; the parabola is never
/// extended outside the rendered primitive.
pub fn junction_arc_geometry_at_point(
    rectangle: RectPrimitive,
    point_x: f32,
    point_y: f32,
) -> Option<JunctionArcGeometrySample> {
    if !rectangle.shape.is_junction_arc()
        || !rectangle.x.is_finite()
        || !rectangle.y.is_finite()
        || !rectangle.width.is_finite()
        || !rectangle.height.is_finite()
        || !point_x.is_finite()
        || !point_y.is_finite()
        || rectangle.width <= 0.0
        || rectangle.height <= 0.0
    {
        return None;
    }
    let local_x = point_x - rectangle.x;
    let local_y = point_y - rectangle.y;
    let stroke = junction_arc_stroke_width(rectangle.height);
    let inner_height = (rectangle.height - stroke).max(0.0);
    let curve_y_at = |fraction: f32| {
        let parabola = 4.0 * (fraction - 0.5).powi(2);
        if rectangle.shape.is_junction_arc_up() {
            stroke * 0.5 + inner_height * parabola
        } else {
            rectangle.height - stroke * 0.5 - inner_height * parabola
        }
    };
    let bounded_to_endpoint = local_x < 0.0 || local_x > rectangle.width;
    let fraction = (local_x / rectangle.width).clamp(0.0, 1.0);
    let curve_y = curve_y_at(fraction);
    let slope = 8.0 * inner_height * (fraction - 0.5) / rectangle.width.max(1.0);
    let distance = if local_x < 0.0 {
        local_x.hypot(local_y - curve_y)
    } else if local_x > rectangle.width {
        (local_x - rectangle.width).hypot(local_y - curve_y)
    } else {
        (local_y - curve_y).abs() / (1.0 + slope * slope).sqrt()
    };
    Some(JunctionArcGeometrySample {
        curve_y: rectangle.y + curve_y,
        slope,
        distance,
        bounded_to_endpoint,
    })
}

pub fn junction_arc_distance_to_point(
    rectangle: RectPrimitive,
    point_x: f32,
    point_y: f32,
) -> Option<f32> {
    junction_arc_geometry_at_point(rectangle, point_x, point_y).map(|sample| sample.distance)
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextPrimitive {
    /// Horizontal centre in logical pixels.
    pub x: f32,
    /// Vertical centre in logical pixels.
    pub y: f32,
    pub text: String,
    pub font_size: f32,
    pub color: Rgba,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Left,
    Right,
}

/// Width of a read's directional terminal in screen points.
///
/// The height-derived size matches the compact IGV cue, while the width cap
/// guarantees that short reads retain a recognisable rectangular body as the
/// viewport approaches regional detail.
pub fn read_arrow_head_width(width: f32, height: f32) -> f32 {
    (height.max(0.0) * 0.55).min(width.max(0.0) * 0.35)
}

/// Screen-space dimensions for the orientation-specific breakend bracket.
///
/// The vertical bar identifies the join side while the horizontal stem keeps
/// the glyph legible without making it look like a strand arrow.
pub fn breakend_bracket_dimensions(width: f32, height: f32) -> (f32, f32) {
    let width = width.max(0.0);
    let height = height.max(0.0);
    let bar_width = width.min(2.0);
    let stem_half_height = (height * 0.14).max(height.min(1.0)).min(height * 0.5);
    (bar_width, stem_half_height)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    pub rectangles: Vec<RectPrimitive>,
    pub texts: Vec<TextPrimitive>,
}

impl DisplayList {
    pub fn synthetic_reads(width: f32, rows: u32, count: u32) -> Self {
        Self::synthetic_reads_at(width, rows, count, 72.0)
    }

    pub fn synthetic_reads_at(width: f32, rows: u32, count: u32, top: f32) -> Self {
        let mut rectangles = Vec::with_capacity(count as usize);
        let usable_width = width.max(1.0);
        for index in 0..count {
            let lane = index % rows.max(1);
            let hash = index.wrapping_mul(2_654_435_761);
            let x = (hash % usable_width as u32) as f32;
            let read_width = 8.0 + ((hash >> 16) % 80) as f32;
            let reverse = index % 2 == 1;
            rectangles.push(RectPrimitive {
                x,
                y: top + lane as f32 * 7.0,
                width: read_width.min(usable_width - x).max(1.0),
                height: 5.0,
                color: if reverse {
                    Rgba::new(0.92, 0.35, 0.30, 0.88)
                } else {
                    Rgba::new(0.20, 0.55, 0.92, 0.88)
                },
                object_id: index,
                shape: Default::default(),
            });
        }
        Self {
            rectangles,
            texts: Vec::new(),
        }
    }

    pub fn synthetic_coverage_at(width: f32, top: f32) -> Self {
        let bin_count = width.round().clamp(64.0, 1_024.0) as u32;
        let bin_width = width.max(1.0) / bin_count as f32;
        let mut rectangles = Vec::with_capacity(bin_count as usize);
        for bin in 0..bin_count {
            let hash = bin.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
            let fraction = ((hash >> 16) & 0xff) as f32 / 255.0;
            let height = 4.0 + fraction * 52.0;
            rectangles.push(RectPrimitive {
                x: bin as f32 * bin_width,
                y: top + 58.0 - height,
                width: bin_width.max(1.0),
                height,
                color: Rgba::new(0.24, 0.72, 0.62, 0.82),
                object_id: 0,
                shape: Default::default(),
            });
        }
        Self {
            rectangles,
            texts: Vec::new(),
        }
    }

    pub fn synthetic_coverage_and_reads_at(width: f32, rows: u32, count: u32, top: f32) -> Self {
        let mut display = Self::synthetic_coverage_at(width, top);
        let reads = Self::synthetic_reads_at(width, rows, count, top + 72.0);
        display.rectangles.extend(reads.rectangles);
        display
    }

    /// Applies a vertical canvas offset and clips primitives to the visible
    /// genomic canvas. This happens after cached layout construction.
    pub fn scroll_and_clip_vertical(&mut self, offset: f32, top: f32, bottom: f32) {
        *self = self.scrolled_and_clipped_vertical(offset, top, bottom);
    }

    /// Returns only the primitives visible after applying a vertical canvas
    /// offset. Unlike cloning and then clipping, this allocates and copies only
    /// the visible result, which keeps interactive scrolling bounded by the
    /// viewport rather than the complete laid-out stack.
    #[must_use]
    pub fn scrolled_and_clipped_vertical(&self, offset: f32, top: f32, bottom: f32) -> Self {
        let mut output = Self::default();
        self.write_scrolled_and_clipped_vertical(offset, top, bottom, &mut output);
        output
    }

    /// Reuses an existing display list's allocations while producing a new
    /// vertically clipped view. This is the hot path for native scrollbar
    /// gestures, where allocating equivalent vectors on every frame causes a
    /// visible cold-start hitch.
    pub fn write_scrolled_and_clipped_vertical(
        &self,
        offset: f32,
        top: f32,
        bottom: f32,
        output: &mut Self,
    ) {
        output.rectangles.clear();
        output.texts.clear();
        self.append_scrolled_and_clipped_vertical(offset, top, bottom, output);
    }

    pub fn append_scrolled_and_clipped_vertical(
        &self,
        offset: f32,
        top: f32,
        bottom: f32,
        output: &mut Self,
    ) {
        // Density navigation can place the laid-out content below the top of a
        // larger virtual world. In that case the effective offset is negative:
        // subtracting it translates content down into the camera before clipping.
        let offset = if offset.is_finite() { offset } else { 0.0 };
        output
            .rectangles
            .extend(self.rectangles.iter().filter_map(|rectangle| {
                let mut rectangle = *rectangle;
                rectangle.y -= offset;
                let rectangle_bottom = rectangle.y + rectangle.height;
                if rectangle_bottom <= top || rectangle.y >= bottom {
                    return None;
                }
                let clipped_top = rectangle.y < top;
                let clipped_bottom = rectangle.y + rectangle.height > bottom;
                if clipped_top {
                    rectangle.height -= top - rectangle.y;
                    rectangle.y = top;
                }
                if clipped_bottom {
                    rectangle.height = bottom - rectangle.y;
                }
                if (clipped_top || clipped_bottom) && rectangle.shape.is_junction_arc() {
                    return None;
                }
                if clipped_top || clipped_bottom {
                    rectangle.shape = RectShape::Rectangle;
                }
                (rectangle.height > 0.0).then_some(rectangle)
            }));
        output.texts.extend(self.texts.iter().filter_map(|text| {
            let mut text = text.clone();
            text.y -= offset;
            (text.y >= top && text.y < bottom).then_some(text)
        }));
    }

    /// Translates content-local horizontal coordinates into the application
    /// canvas and clips primitives to the genomic viewport.
    pub fn translate_and_clip_horizontal(&mut self, offset: f32, left: f32, right: f32) {
        self.rectangles.retain_mut(|rectangle| {
            rectangle.x += offset;
            let rectangle_right = rectangle.x + rectangle.width;
            if rectangle_right <= left || rectangle.x >= right {
                return false;
            }
            let clipped_left = rectangle.x < left;
            let clipped_right = rectangle.x + rectangle.width > right;
            if clipped_left {
                rectangle.width -= left - rectangle.x;
                rectangle.x = left;
            }
            if clipped_right {
                rectangle.width = right - rectangle.x;
            }
            if matches!(rectangle.shape.direction(), Some(Direction::Left)) && clipped_left
                || matches!(rectangle.shape.direction(), Some(Direction::Right)) && clipped_right
            {
                rectangle.shape = RectShape::Rectangle;
            }
            rectangle.width > 0.0
        });
        self.texts.retain_mut(|text| {
            text.x += offset;
            text.x >= left - 128.0 && text.x <= right + 128.0
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junction_arc_distance_matches_up_down_curves_and_bounded_endpoints() {
        let up = RectPrimitive {
            x: 10.0,
            y: 20.0,
            width: 20.0,
            height: 10.0,
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            object_id: 0,
            shape: RectShape::JunctionArcUp,
        };
        let stroke = junction_arc_stroke_width(up.height);
        let up_apex_y = up.y + stroke * 0.5;
        let up_endpoint_y = up.y + up.height - stroke * 0.5;
        assert!(
            junction_arc_distance_to_point(up, up.x + up.width * 0.5, up_apex_y).unwrap()
                < f32::EPSILON
        );
        assert!(junction_arc_distance_to_point(up, up.x, up_endpoint_y).unwrap() < f32::EPSILON);
        let bounded = junction_arc_geometry_at_point(up, up.x - 3.0, up_endpoint_y).unwrap();
        assert!(
            (bounded.distance - 3.0).abs() < f32::EPSILON,
            "distance beyond an endpoint must be Euclidean, not an extended parabola"
        );
        assert!(bounded.bounded_to_endpoint);
        assert!((bounded.curve_y - up_endpoint_y).abs() < f32::EPSILON);

        let down = RectPrimitive {
            shape: RectShape::JunctionArcDown,
            ..up
        };
        let down_apex_y = down.y + down.height - stroke * 0.5;
        let down_endpoint_y = down.y + stroke * 0.5;
        assert!(
            junction_arc_distance_to_point(down, down.x + down.width * 0.5, down_apex_y).unwrap()
                < f32::EPSILON
        );
        assert!(
            junction_arc_distance_to_point(down, down.x + down.width, down_endpoint_y).unwrap()
                < f32::EPSILON
        );
    }

    #[test]
    fn junction_arc_distance_is_style_independent_and_rejects_invalid_geometry() {
        let base = RectPrimitive {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 3.0,
            color: Rgba::new(1.0, 1.0, 1.0, 1.0),
            object_id: 0,
            shape: RectShape::JunctionArcUp,
        };
        let styled = RectPrimitive {
            shape: RectShape::JunctionArcSelectedUp,
            ..base
        };
        assert_eq!(
            junction_arc_distance_to_point(base, 0.5, 1.5),
            junction_arc_distance_to_point(styled, 0.5, 1.5)
        );
        assert_eq!(
            junction_arc_distance_to_point(RectPrimitive { width: 0.0, ..base }, 0.0, 0.0),
            None
        );
        assert_eq!(
            junction_arc_distance_to_point(
                RectPrimitive {
                    shape: RectShape::Rectangle,
                    ..base
                },
                0.0,
                0.0
            ),
            None
        );
        assert_eq!(junction_arc_distance_to_point(base, f32::NAN, 0.0), None);
    }

    #[test]
    fn synthetic_layout_is_deterministic_and_bounded() {
        let first = DisplayList::synthetic_reads(800.0, 100, 10_000);
        let second = DisplayList::synthetic_reads(800.0, 100, 10_000);
        assert_eq!(first, second);
        assert_eq!(first.rectangles.len(), 10_000);
        assert!(
            first
                .rectangles
                .iter()
                .all(|rect| rect.x + rect.width <= 800.0)
        );
    }

    #[test]
    fn vertical_scrolling_culls_and_clips_primitives() {
        let mut display = DisplayList {
            rectangles: vec![
                RectPrimitive {
                    x: 0.0,
                    y: 100.0,
                    width: 10.0,
                    height: 20.0,
                    color: Rgba::new(1.0, 1.0, 1.0, 1.0),
                    object_id: 1,
                    shape: Default::default(),
                },
                RectPrimitive {
                    x: 0.0,
                    y: 200.0,
                    width: 10.0,
                    height: 40.0,
                    color: Rgba::new(1.0, 1.0, 1.0, 1.0),
                    object_id: 2,
                    shape: Default::default(),
                },
            ],
            texts: vec![TextPrimitive {
                x: 5.0,
                y: 210.0,
                text: "I12".to_owned(),
                font_size: 9.0,
                color: Rgba::new(0.5, 0.2, 0.8, 1.0),
            }],
        };
        let original = display.clone();
        let derived = display.scrolled_and_clipped_vertical(50.0, 80.0, 180.0);
        display.scroll_and_clip_vertical(50.0, 80.0, 180.0);
        assert_eq!(display, derived);
        assert_eq!(original.rectangles.len(), 2);
        assert_eq!(display.rectangles.len(), 1);
        assert!((display.rectangles[0].y - 150.0).abs() < f32::EPSILON);
        assert!((display.rectangles[0].height - 30.0).abs() < f32::EPSILON);
        assert_eq!(display.texts.len(), 1);
        assert!((display.texts[0].y - 160.0).abs() < f32::EPSILON);
    }

    #[test]
    fn negative_effective_scroll_places_content_inside_virtual_top_padding() {
        let display = DisplayList {
            rectangles: vec![RectPrimitive {
                x: 0.0,
                y: 100.0,
                width: 10.0,
                height: 20.0,
                color: Rgba::new(1.0, 1.0, 1.0, 1.0),
                object_id: 1,
                shape: Default::default(),
            }],
            texts: Vec::new(),
        };

        let translated = display.scrolled_and_clipped_vertical(-50.0, 80.0, 200.0);
        assert_eq!(translated.rectangles.len(), 1);
        assert!((translated.rectangles[0].y - 150.0).abs() < f32::EPSILON);
    }

    #[test]
    fn horizontal_translation_respects_the_genomic_viewport() {
        let mut display = DisplayList {
            rectangles: vec![
                RectPrimitive {
                    x: -10.0,
                    y: 0.0,
                    width: 30.0,
                    height: 10.0,
                    color: Rgba::new(1.0, 1.0, 1.0, 1.0),
                    object_id: 1,
                    shape: Default::default(),
                },
                RectPrimitive {
                    x: 95.0,
                    y: 0.0,
                    width: 20.0,
                    height: 10.0,
                    color: Rgba::new(1.0, 1.0, 1.0, 1.0),
                    object_id: 2,
                    shape: Default::default(),
                },
            ],
            texts: Vec::new(),
        };
        display.translate_and_clip_horizontal(100.0, 100.0, 200.0);
        assert_eq!(display.rectangles.len(), 2);
        assert!((display.rectangles[0].x - 100.0).abs() < f32::EPSILON);
        assert!((display.rectangles[0].width - 20.0).abs() < f32::EPSILON);
        assert!((display.rectangles[1].x - 195.0).abs() < f32::EPSILON);
        assert!((display.rectangles[1].width - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn clipping_removes_a_direction_shape_only_when_its_semantic_tip_is_cut() {
        let mut display = DisplayList {
            rectangles: vec![
                RectPrimitive {
                    x: 90.0,
                    y: 10.0,
                    width: 30.0,
                    height: 10.0,
                    color: Rgba::new(0.0, 0.0, 0.0, 1.0),
                    object_id: 1,
                    shape: RectShape::DirectionalTerminalLeft,
                },
                RectPrimitive {
                    x: 90.0,
                    y: 30.0,
                    width: 30.0,
                    height: 10.0,
                    color: Rgba::new(0.0, 0.0, 0.0, 1.0),
                    object_id: 2,
                    shape: RectShape::DirectionalTerminalRight,
                },
            ],
            texts: Vec::new(),
        };
        display.translate_and_clip_horizontal(0.0, 100.0, 200.0);
        assert_eq!(display.rectangles[0].shape, RectShape::Rectangle);
        assert_eq!(
            display.rectangles[1].shape,
            RectShape::DirectionalTerminalRight
        );
    }

    #[test]
    fn synthetic_genome_view_contains_coverage_and_reads() {
        let display = DisplayList::synthetic_coverage_and_reads_at(800.0, 100, 10_000, 190.0);
        assert!(display.rectangles.len() > 10_000);
        assert!(
            display
                .rectangles
                .iter()
                .take(800)
                .all(|item| item.object_id == 0)
        );
        assert!(
            display
                .rectangles
                .iter()
                .skip(800)
                .any(|item| item.object_id > 0)
        );
    }
}
