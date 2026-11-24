//! The unified preset format: Equalizer APO / AutoEQ text plus an
//! `# otis:` comment header for Otis-only settings.

use crate::model::{Band, ChannelScope, FEED_MAX, FilterType, Workspace};

const HEADER: &str = "# otis:";

pub fn serialize(ws: &Workspace) -> String {
    let on_off = |b: bool| if b { "on" } else { "off" };
    let mut out = format!(
        "{HEADER} auto_preamp={} crossfeed={} feed_db={:.1}\n",
        on_off(ws.auto_preamp),
        on_off(ws.crossfeed.enabled),
        ws.crossfeed.feed_db,
    );
    out += &format!("Preamp: {:.1} dB\n", ws.preamp_db());
    for (i, b) in ws.bands.iter().enumerate() {
        let state = if b.enabled { "ON" } else { "OFF" };
        let (n, kind, fc) = (i + 1, b.kind.label(), num(b.freq, 1));
        out += &if b.kind.has_gain() {
            format!("Filter {n}: {state} {kind} Fc {fc} Hz Gain {:.1} dB Q {:.2}\n", b.gain, b.q)
        } else {
            format!("Filter {n}: {state} {kind} Fc {fc} Hz Q {:.2}\n", b.q)
        };
    }
    out
}

/// Parse a preset. Lenient by design: unknown lines and filter types are
/// skipped so files from AutoEQ, Oratory1990 and Squiglink import cleanly.
pub fn parse(text: &str) -> Workspace {
    let mut ws = Workspace::default();
    let mut has_header = false;
    let mut has_preamp = false;

    for line in text.lines().map(str::trim) {
        if let Some(meta) = line.strip_prefix(HEADER) {
            has_header = true;
            for (key, value) in meta.split_whitespace().filter_map(|kv| kv.split_once('=')) {
                match key {
                    "auto_preamp" => ws.auto_preamp = value == "on",
                    "crossfeed" => {
                        if let Ok(pct) = value.parse::<u8>() {
                            ws.crossfeed.enabled = pct > 0;
                            if pct > 0 {
                                let lin = (pct.min(100) as f64 / 100.0) * crate::dsp::db_to_lin(FEED_MAX);
                                ws.crossfeed.feed_db = 20.0 * lin.log10();
                            }
                        } else {
                            ws.crossfeed.enabled = value == "on";
                        }
                    }
                    "feed_db" => ws.crossfeed.feed_db = value.parse().unwrap_or(ws.crossfeed.feed_db),
                    _ => {}
                }
            }
        } else if let Some(rest) = strip_prefix_ci(line, "preamp:") {
            if let Some(db) = rest.split_whitespace().next().and_then(|v| v.parse().ok()) {
                ws.manual_preamp = db;
                has_preamp = true;
            }
        } else if strip_prefix_ci(line, "filter").is_some()
            && let Some(band) = line.split_once(':').and_then(|(_, spec)| parse_filter(spec))
        {
            ws.bands.push(band);
        }
    }

    // A foreign file carries its author's preamp; honour it literally
    // instead of silently replacing it with Auto-Preamp.
    if !has_header && has_preamp {
        ws.auto_preamp = false;
    }
    // Under Auto-Preamp the Preamp line is derived output, not state.
    if ws.auto_preamp {
        ws.manual_preamp = 0.0;
    }
    ws.normalize();
    ws
}

fn parse_filter(spec: &str) -> Option<Band> {
    let tokens: Vec<&str> = spec.split_whitespace().collect();
    let enabled = match tokens.first()?.to_ascii_uppercase().as_str() {
        "ON" => true,
        "OFF" => false,
        _ => return None,
    };
    let kind = FilterType::from_apo(tokens.get(1)?)?;
    let value_after = |key: &str| {
        let at = tokens.iter().position(|t| t.eq_ignore_ascii_case(key))?;
        tokens.get(at + 1)?.parse::<f64>().ok()
    };
    Some(Band {
        enabled,
        kind,
        freq: value_after("Fc")?,
        gain: value_after("Gain").unwrap_or(0.0),
        q: value_after("Q").unwrap_or(std::f64::consts::FRAC_1_SQRT_2),
        scope: ChannelScope::Stereo,
    })
}

fn strip_prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &line[prefix.len()..])
}

/// Format with up to `decimals` places, dropping trailing zeros.
pub fn num(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_autoeq() {
        let ws = parse(
            "Preamp: -4.2 dB\n\
             Filter 1: ON LSC Fc 105 Hz Gain 3.5 dB Q 0.71\n\
             Filter 2: ON PK Fc 6400 Hz Gain -4.0 dB Q 4.50\n\
             Filter 3: OFF HSC Fc 10000 Hz Gain 1.0 dB Q 0.70\n\
             Filter 4: ON NO Fc 50 Hz\n",
        );
        assert_eq!(ws.bands.len(), 3);
        assert!(!ws.auto_preamp);
        assert_eq!(ws.manual_preamp, -4.2);
        assert_eq!(ws.bands[0].kind, FilterType::LowShelf);
        assert_eq!((ws.bands[1].freq, ws.bands[1].gain, ws.bands[1].q), (6400.0, -4.0, 4.5));
        assert!(!ws.bands[2].enabled);
    }

    #[test]
    fn round_trips() {
        let mut ws = Workspace::default();
        ws.bands.push(Band { gain: 3.3, q: 0.71, ..Band::peak(105.5) });
        ws.bands.push(Band { kind: FilterType::HighPass, enabled: false, ..Band::peak(30.0) });
        ws.crossfeed.enabled = true;
        ws.crossfeed.feed_db = -4.5;
        ws.normalize();
        let text = serialize(&ws);
        assert!(text.contains("crossfeed=on feed_db=-4.5"));
        assert!(text.contains("Filter 2: OFF HP Fc 30 Hz Q 1.41"));
        assert_eq!(parse(&text), ws);
        assert_eq!(serialize(&parse(&text)), text);
    }
}
