//! Colours and egui style for the single Otis window.

use eframe::egui::{self, Color32, CornerRadius, Stroke, vec2};

// A near-black ground with warm off-white text and one cool accent.
pub const WINDOW_BG: Color32 = Color32::from_rgb(0x0a, 0x0a, 0x0b);
pub const GRAPH_BG: Color32 = Color32::from_rgb(0x00, 0x00, 0x00);
pub const SURFACE: Color32 = Color32::from_rgb(0x15, 0x15, 0x17);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x23);
pub const SURFACE_ACTIVE: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x30);
pub const HAIRLINE: Color32 = Color32::from_rgb(0x24, 0x24, 0x28);
pub const GRID: Color32 = Color32::from_rgb(0x17, 0x17, 0x1a);
pub const ZERO_LINE: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x40);
pub const PREAMP_LINE: Color32 = Color32::from_rgb(0x2d, 0x2d, 0x35);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xe6, 0xe1);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x85, 0x83, 0x7e);
pub const ACCENT: Color32 = Color32::from_rgb(0xa3, 0xb8, 0xff);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x0a, 0x0d, 0x18);
pub const WARN: Color32 = Color32::from_rgb(0xee, 0xac, 0x3c);
pub const DANGER: Color32 = Color32::from_rgb(0xf0, 0x6a, 0x6a);
pub const CURSOR: Color32 = Color32::from_rgb(0xf4, 0xf2, 0xec);
pub const CURVE_BYPASSED: Color32 = Color32::from_rgb(0x55, 0x55, 0x5c);
pub const MARKER_FILL: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x20);
pub const BAND_FILL: Color32 = Color32::from_rgba_premultiplied(0x16, 0x1b, 0x2e, 0x40);
pub const BAND_LINE: Color32 = Color32::from_rgb(0x56, 0x64, 0x96);
pub const ROW_ALT: Color32 = Color32::from_rgb(0x10, 0x10, 0x12);
pub const ROW_HOVER: Color32 = Color32::from_rgb(0x16, 0x16, 0x19);
pub const ROW_SELECTED: Color32 = Color32::from_rgb(0x17, 0x1a, 0x28);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = WINDOW_BG;
    visuals.window_fill = Color32::from_rgb(0x11, 0x11, 0x13);
    visuals.window_stroke = Stroke::new(1.0, HAIRLINE);
    visuals.extreme_bg_color = GRAPH_BG;
    visuals.faint_bg_color = ROW_ALT;
    visuals.override_text_color = Some(TEXT);
    visuals.selection.bg_fill = Color32::from_rgb(0x2c, 0x36, 0x5c);
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