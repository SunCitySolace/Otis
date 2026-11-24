//! The frequency response canvas, drawn directly with `egui::Painter`.
//!
//! The graph is read-only apart from the sweep cursor: bands are edited in
//! the table or from the keyboard, never dragged here.

use crate::dsp;
use crate::model::{FREQ_MAX, FREQ_MIN, Workspace};
use crate::theme;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use eframe::epaint::Mesh;

const GRID_FREQS: [(f64, &str); 10] = [
    (20.0, "20"),
    (50.0, "50"),
    (100.0, "100"),
    (200.0, "200"),
    (500.0, "500"),
    (1000.0, "1k"),
    (2000.0, "2k"),
    (5000.0, "5k"),
    (10_000.0, "10k"),
    (20_000.0, "20k"),
];
const MIN_RANGE_DB: f64 = 12.0;
const MARKER_RADIUS: f32 = 9.0;

pub struct View<'a> {
    pub workspace: &'a Workspace,
    pub selected: Option<usize>,
    pub cursor_hz: f64,
    pub bypass: bool,
}

pub enum Event {
    /// A band marker was clicked.
    SelectBand(usize),
    /// The sweep cursor was clicked or dragged to a frequency.
    MoveCursor(f64),
}

/// Paint the graph into the remaining space. `dragging` is the caller-held
/// flag for an in-progress cursor drag.
pub fn show(ui: &mut Ui, view: &View, dragging: &mut bool) -> Option<Event> {
    let (outer, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
    let painter = ui.painter_at(outer);
    painter.rect_filled(outer, 6.0, theme::GRAPH_BG);
    let plot = Rect::from_min_max(outer.min + vec2(40.0, 12.0), outer.max - vec2(14.0, 24.0));
    if plot.width() < 40.0 || plot.height() < 40.0 {
        return None;
    }

    let bands = &view.workspace.bands;
    let preamp = view.workspace.preamp_db();
    let combined = |freq: f64| dsp::eq_db(bands, freq) + preamp;

    let steps = (plot.width() / 2.0) as usize;
    let freqs: Vec<f64> = (0..=steps).map(|i| FREQ_MIN * (FREQ_MAX / FREQ_MIN).powf(i as f64 / steps as f64)).collect();
    let curve: Vec<f64> = freqs.iter().map(|&f| combined(f)).collect();

    // ±12 dB by default, growing in 6 dB steps when the response is pushed further.
    // Low/high-pass skirts fall forever, so cuts only count as far as the
    // band gains and preamp can explain them.
    let cut_budget = preamp.abs()
        + bands.iter().filter(|b| b.enabled && b.kind.has_gain()).map(|b| (-b.gain).max(0.0)).sum::<f64>();
    let highest = curve.iter().copied().fold(0.0, f64::max);
    let lowest = curve.iter().copied().fold(0.0, f64::min);
    let extent = MIN_RANGE_DB.max(highest).max((-lowest).min(cut_budget));
    let range = ((extent / 6.0).ceil() * 6.0).min(60.0);

    let span = (FREQ_MAX / FREQ_MIN).ln();
    let x_of = |freq: f64| plot.left() + ((freq / FREQ_MIN).ln() / span) as f32 * plot.width();
    let y_of = |db: f64| plot.center().y - (db / range).clamp(-1.2, 1.2) as f32 * plot.height() / 2.0;
    let freq_at = |x: f32| {
        let t = ((x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64;
        FREQ_MIN * (span * t).exp()
    };

    // Grid.
    let label_font = FontId::proportional(11.0);
    for (freq, label) in GRID_FREQS {
        let x = x_of(freq);
        painter.vline(x, plot.y_range(), Stroke::new(1.0, theme::GRID));
        painter.text(pos2(x, plot.bottom() + 5.0), Align2::CENTER_TOP, label, label_font.clone(), theme::TEXT_DIM);
    }
    let db_step = if range <= 24.0 { 6.0 } else { 12.0 };
    let mut db = -range;
    while db <= range {
        if db != 0.0 {
            painter.hline(plot.x_range(), y_of(db), Stroke::new(1.0, theme::GRID));
        }
        painter.text(
            pos2(plot.left() - 6.0, y_of(db)),
            Align2::RIGHT_CENTER,
            if db == 0.0 { "0".to_string() } else { format!("{db:+.0}") },
            label_font.clone(),
            theme::TEXT_DIM,
        );
        db += db_step;
    }

    // 1. The 0 dB reference line, plus a subtle preamp baseline when shifted.
    painter.hline(plot.x_range(), y_of(0.0), Stroke::new(1.0, theme::ZERO_LINE));

    let clipped = painter.with_clip_rect(plot);
    if preamp.abs() > 0.05 {
        let py = y_of(preamp);
        if py >= plot.top() && py <= plot.bottom() {
            clipped.extend(Shape::dashed_line(
                &[pos2(plot.left(), py), pos2(plot.right(), py)],
                Stroke::new(1.0, theme::PREAMP_LINE),
                4.0,
                4.0,
            ));
        }
    }

    let to_points = |values: &[f64]| -> Vec<Pos2> { freqs.iter().zip(values).map(|(&f, &v)| pos2(x_of(f), y_of(v))).collect() };

    // 2. The selected band's own curve, softly shaded from the preamp baseline.
    if let Some(band) = view.selected.and_then(|i| bands.get(i)) {
        let own: Vec<f64> = freqs.iter().map(|&f| preamp + dsp::band_db(band, f)).collect();
        let points = to_points(&own);
        let baseline = y_of(preamp);
        let mut mesh = Mesh::default();
        for (i, p) in points.iter().enumerate() {
            mesh.colored_vertex(*p, theme::BAND_FILL);
            mesh.colored_vertex(pos2(p.x, baseline), theme::BAND_FILL);
            if i > 0 {
                let n = (i * 2) as u32;
                mesh.add_triangle(n - 2, n - 1, n);
                mesh.add_triangle(n - 1, n, n + 1);
            }
        }
        clipped.add(Shape::mesh(mesh));
        clipped.line(points, Stroke::new(1.0, theme::BAND_LINE));
    }

    // 3. The combined, preamp-shifted response. Dimmed and dashed under
    //    bypass, when it is not what you are hearing.
    let points = to_points(&curve);
    if view.bypass {
        clipped.extend(Shape::dashed_line(&points, Stroke::new(1.5, theme::CURVE_BYPASSED), 6.0, 5.0));
    } else {
        clipped.line(points, Stroke::new(2.0, theme::ACCENT));
    }

    // 4. Numbered band markers on the curve. Click nearest to select; never draggable.
    let centers: Vec<Pos2> = bands
        .iter()
        .map(|band| pos2(x_of(band.freq), y_of(combined(band.freq)).clamp(plot.top(), plot.bottom())))
        .collect();
    let hit_radius = MARKER_RADIUS + 4.0;
    let nearest_marker = |pos: Pos2| -> Option<usize> {
        centers
            .iter()
            .enumerate()
            .filter_map(|(i, &c)| {
                let d = pos.distance(c);
                (d <= hit_radius).then_some((i, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };

    let hovered_band = (!*dragging).then(|| response.hover_pos().and_then(nearest_marker)).flatten();
    let pressed_at = response
        .is_pointer_button_down_on()
        .then(|| ui.input(|i| i.pointer.primary_pressed().then(|| i.pointer.interact_pos()).flatten()))
        .flatten();
    let mut event = pressed_at.and_then(nearest_marker).map(Event::SelectBand);

    // Draw hovered and selected markers on top of neighbours.
    let mut order: Vec<usize> = (0..bands.len()).collect();
    order.sort_by_key(|&i| {
        if view.selected == Some(i) {
            2
        } else if hovered_band == Some(i) {
            1
        } else {
            0
        }
    });
    for i in order {
        let band = &bands[i];
        let center = centers[i];
        let selected = view.selected == Some(i);
        let hovered = hovered_band == Some(i);
        let (fill, stroke, stroke_w, text) = match (selected, hovered, band.enabled) {
            (true, _, _) => (theme::ACCENT, theme::ACCENT, 1.5, theme::GRAPH_BG),
            (false, true, true) => (theme::SURFACE_ACTIVE, theme::ACCENT, 1.75, theme::TEXT),
            (false, true, false) => (theme::SURFACE, theme::TEXT, 1.5, theme::TEXT),
            (false, false, true) => (theme::MARKER_FILL, theme::TEXT, 1.5, theme::TEXT),
            (false, false, false) => (theme::GRAPH_BG, theme::TEXT_DIM, 1.5, theme::TEXT_DIM),
        };
        painter.circle(center, MARKER_RADIUS, fill, Stroke::new(stroke_w, stroke));
        painter.text(center, Align2::CENTER_CENTER, (i + 1).to_string(), FontId::proportional(11.0), text);
    }

    // 5. The sweep cursor and its readout pill.
    let cursor_x = x_of(view.cursor_hz);
    painter.vline(cursor_x, plot.y_range(), Stroke::new(1.5, theme::CURSOR));
    let readout = format!("{} · {:+.1} dB", format_hz(view.cursor_hz), combined(view.cursor_hz));
    let galley = painter.layout_no_wrap(readout, FontId::proportional(12.0), theme::GRAPH_BG);
    let pill = Rect::from_center_size(pos2(cursor_x, plot.top() + 12.0), galley.size() + vec2(14.0, 6.0));
    let shift = (plot.left() - pill.left()).max(0.0) - (pill.right() - plot.right()).max(0.0);
    let pill = pill.translate(vec2(shift, 0.0));
    painter.rect_filled(pill, pill.height() / 2.0, theme::CURSOR);
    painter.galley(pill.center() - galley.size() / 2.0, galley, Color32::PLACEHOLDER);

    // Interaction: a press on a marker selects it; a press anywhere else
    // starts a cursor drag.
    if response.is_pointer_button_down_on() {
        if pressed_at.is_some() {
            *dragging = event.is_none();
        }
        if *dragging && let Some(pos) = response.interact_pointer_pos() {
            event = Some(Event::MoveCursor(freq_at(pos.x)));
        }
    } else {
        *dragging = false;
    }
    if *dragging {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    } else if hovered_band.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    } else if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    event
}

/// "6,420 Hz"; one decimal below 100 Hz where it matters.
pub fn format_hz(freq: f64) -> String {
    if freq < 100.0 {
        return format!("{freq:.1} Hz");
    }
    let whole = freq.round() as u64;
    if whole >= 1000 { format!("{},{:03} Hz", whole / 1000, whole % 1000) } else { format!("{whole} Hz") }
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_frequencies() {
        assert_eq!(super::format_hz(6420.3), "6,420 Hz");
        assert_eq!(super::format_hz(20000.0), "20,000 Hz");
        assert_eq!(super::format_hz(440.0), "440 Hz");
        assert_eq!(super::format_hz(31.5), "31.5 Hz");
    }
}