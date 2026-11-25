//! Colours and egui style for the single Otis window.
//!
//! Neutral deep-dark grays and crisp white chrome, with Hi-Res Gold (#E2B857)
//! reserved solely for the sweep cursor and LED green/red for active "On" and bypass states.

use eframe::egui::{self, Color32, CornerRadius, Stroke, vec2};

pub const WINDOW_BG: Color32 = Color32::from_rgb(0x12, 0x12, 0x14);
pub const GRAPH_BG: Color32 = Color32::from_rgb(0x0a, 0x0a, 0x0c);
pub const SURFACE: Color32 = Color32::from_rgb(0x1b, 0x1b, 0x1e);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x25, 0x25, 0x29);
pub const SURFACE_ACTIVE: Color32 = Color32::from_rgb(0x2f, 0x2f, 0x35);
pub const HAIRLINE: Color32 = Color32::from_rgb(0x28, 0x28, 0x2d);
pub const GRID: Color32 = Color32::from_rgb(0x17, 0x17, 0x1b);
pub const ZERO_LINE: Color32 = Color32::from_rgb(0x3f, 0x3f, 0x47);
pub const PREAMP_LINE: Color32 = Color32::from_rgb(0x34, 0x34, 0x3b);
pub const TEXT: Color32 = Color32::from_rgb(0xed, 0xed, 0xf0);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8c, 0x8c, 0x96);
pub const ACCENT: Color32 = Color32::from_rgb(0xe4, 0xe4, 0xe8);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x12, 0x12, 0x14);
/// Hi-Res Gold, reserved strictly for the graph sweep/frequency cursor.
pub const CURSOR: Color32 = Color32::from_rgb(0xe2, 0xb8, 0x57);
/// Hardware LED green for active "On" indicators (Crossfeed, Sweep On).
pub const LED_GREEN: Color32 = Color32::from_rgb(0x3f, 0xd8, 0x72);
pub const LED_GLOW: Color32 = Color32::from_rgba_premultiplied(0x10, 0x36, 0x1c, 0x40);
/// Red LED for the Bypassing indicator.
pub const LED_RED: Color32 = Color32::from_rgb(0xf0, 0x4a, 0x44);
pub const LED_RED_GLOW: Color32 = Color32::from_rgba_premultiplied(0x3a, 0x10, 0x0f, 0x40);
pub const WARN: Color32 = Color32::from_rgb(0xe5, 0x85, 0x3a);
pub const DANGER: Color32 = Color32::from_rgb(0xe8, 0x62, 0x5c);
pub const CURVE_BYPASSED: Color32 = Color32::from_rgb(0x5a, 0x5a, 0x63);
pub const CURVE_FILL: Color32 = Color32::from_rgba_premultiplied(0x12, 0x12, 0x14, 0x16);
pub const BAND_FILL: Color32 = Color32::from_rgba_premultiplied(0x28, 0x28, 0x2d, 0x30);
pub const BAND_LINE: Color32 = Color32::from_rgb(0x9a, 0x9a, 0xa4);
pub const ROW_ALT: Color32 = Color32::from_rgb(0x16, 0x16, 0x19);
pub const ROW_HOVER: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x23);
pub const ROW_SELECTED: Color32 = Color32::from_rgb(0x23, 0x23, 0x28);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = WINDOW_BG;
    visuals.window_fill = Color32::from_rgb(0x16, 0x16, 0x19);
    visuals.window_stroke = Stroke::new(1.0, HAIRLINE);
    visuals.extreme_bg_color = GRAPH_BG;
    visuals.faint_bg_color = ROW_ALT;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = Color32::from_rgb(0x36, 0x36, 0x3e);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    for (widget, fill, edge) in [
        (&mut visuals.widgets.inactive, SURFACE, HAIRLINE),
        (&mut visuals.widgets.hovered, SURFACE_HOVER, ZERO_LINE),
        (&mut visuals.widgets.active, SURFACE_ACTIVE, ACCENT),
        (&mut visuals.widgets.open, SURFACE_HOVER, ZERO_LINE),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::new(1.0, edge);
        widget.fg_stroke = Stroke::new(1.0, TEXT);
    }
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(6);
    }
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(9.0, 4.0);
        style.spacing.interact_size.y = 24.0;
    });
}