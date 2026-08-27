# UI icon rendering audit

## Finding

RGV previously encoded icon-only controls as Unicode button text. The affected
controls used left/right arrows, multiplication signs, filled triangles, the
house symbol, mathematical minus, single angle quotation marks, and a gear.
The genotype inspector also used a Unicode circle as a colour swatch. Menu
shortcut and help text contained pictographic key/direction symbols with the
same fallback risk.

This is the source of the recurring empty-square/tofu rendering. egui's
proportional text font does not guarantee coverage of miscellaneous symbols,
dingbats, or geometric shapes. Whether a missing character happens to render
depends on platform font fallback, which can vary by operating system, display
scale, native/headless context, and installed fonts. Replacing one broken
character with another only moves the problem to a different code point.

## Application-wide inventory

The font-backed UI graphics identified by the audit were:

| Area | Previous glyph-backed graphics | Resolution |
| --- | --- | --- |
| Panel selector | move left, move right, close | Central vector icon widget |
| Navigation toolbar | previous, next, home, zoom out, zoom in | Central vector icon widget |
| Properties panel | collapse and expand | Central vector icon widget |
| Track gutter | track settings | Central vector icon painter |
| Variant genotype inspector | colour swatch | Central vector swatch |
| Menu/help copy | command-key and direction pictograms | Platform-aware ASCII key names and words |

The track visibility eyes, section/group chevrons, scrollbars, resize handles,
and genomic directional shapes were already painted geometry. They do not
depend on font glyph coverage and are not at risk from this failure mode.

Mathematical notation and biological text such as `>=`, coverage depth,
strand signs, bases, and CIGAR operators remain text. They carry textual data
rather than standing in for a UI graphic. Keyboard shortcuts now use
platform-aware `Cmd`/`Ctrl` names, and help copy uses directional words.

## Implemented architecture

`apps/rgv-desktop/src/ui/icons.rs` is now the single source for application
icons. It provides:

- a typed `AppIcon` catalogue;
- one `IconButton` widget with consistent sizing, hover/disabled colour, and
  accessibility metadata;
- shared vector painting for embedded controls and non-interactive swatches;
- a regression test that rejects the previously used font-backed icon controls.

Icons are drawn in egui logical coordinates. They therefore inherit display
and accessibility scale without relying on an external font, SVG rasterisation,
or an operating-system fallback chain.

## Policy for future icons

New icon-only controls must use `AppIcon`; a Unicode character must not be used
as the label of an icon-only button. Add the geometry and accessible name in one
place, then exercise its normal, hovered, and disabled states.

The current finite catalogue is small enough that app-owned vector geometry is
the least brittle source. If RGV later needs a broad icon catalogue, adopt a
single SVG/path-based set and vendor only the icons used by the application,
including its licence and pinned source version. Do not adopt a system icon or
icon-font dependency: that would reintroduce font loading, fallback, and glyph
mapping as runtime failure modes.
