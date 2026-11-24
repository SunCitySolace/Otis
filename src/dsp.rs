//! Biquad maths, response evaluation, Auto-Preamp and the preview engine.
//!
//! The coefficient formulas mirror PipeWire's `biquad.c` (the code behind
//! `param_eq`) line for line: RBJ cookbook filters with
//! `alpha = sin(w0) / (2 * Q)` for every type, which is also what
//! Equalizer APO uses for PK / LSC / HSC / LPQ / HPQ.

use crate::model::{Band, Crossfeed, FilterType, MAX_BANDS};
use std::f64::consts::PI;

/// The preview stream always runs at this rate so coefficients can be
/// computed on the UI thread without knowing the device rate.
pub const PREVIEW_RATE: f64 = 48_000.0;

/// Fixed head-shadow crossfeed parameters (Chu Moy / BS2B corner + ±30° ITD).
pub const CROSSFEED_HZ: f64 = 700.0;
pub const CROSSFEED_Q: f64 = 0.5;
pub const CROSSFEED_DELAY_US: f64 = 260.0;

/// Headroom Auto-Preamp leaves below the response peak.
pub const PREAMP_MARGIN_DB: f64 = 0.2;

pub const LEVEL_DEFAULT_DB: f32 = -24.0;
pub const LEVEL_MIN_DB: f32 = -60.0;
/// Hard ceiling of the preview output, enforced again in the audio thread.
pub const LEVEL_CAP_DB: f32 = -6.0;

/// Normalised biquad (a0 == 1).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    fn new(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Coeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    fn gain(g: f64) -> Self {
        Coeffs { b0: g, ..Coeffs::IDENTITY }
    }

    /// Magnitude response in dB at `freq` for sample rate `rate`.
    pub fn db_at(&self, freq: f64, rate: f64) -> f64 {
        let w = 2.0 * PI * freq / rate;
        let (c1, c2) = (w.cos(), (2.0 * w).cos());
        let num = self.b0 * self.b0 + self.b1 * self.b1 + self.b2 * self.b2
            + 2.0 * (self.b0 * self.b1 + self.b1 * self.b2) * c1
            + 2.0 * self.b0 * self.b2 * c2;
        let den = 1.0 + self.a1 * self.a1 + self.a2 * self.a2
            + 2.0 * (self.a1 + self.a1 * self.a2) * c1
            + 2.0 * self.a2 * c2;
        10.0 * (num / den).max(1e-20).log10()
    }
}

/// Port of PipeWire's `biquad_set` for the five supported filter types.
pub fn coeffs(kind: FilterType, freq: f64, q: f64, gain_db: f64, rate: f64) -> Coeffs {
    // PipeWire works with frequency normalised to Nyquist, clipped to 0..1.
    let f = (freq * 2.0 / rate).clamp(0.0, 1.0);
    let a = 10f64.powf(gain_db / 40.0);
    let edge = f <= 0.0 || f >= 1.0;
    let w0 = PI * f;
    let k = w0.cos();
    let q = if q <= 0.0 { std::f64::consts::FRAC_1_SQRT_2 } else { q };
    let alpha = w0.sin() / (2.0 * q);

    match kind {
        FilterType::Peak => {
            if edge {
                return Coeffs::IDENTITY;
            }
            Coeffs::new(1.0 + alpha * a, -2.0 * k, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * k, 1.0 - alpha / a)
        }
        FilterType::LowShelf => {
            if f >= 1.0 {
                return Coeffs::gain(a * a);
            }
            if f <= 0.0 {
                return Coeffs::IDENTITY;
            }
            let k2 = 2.0 * a.sqrt() * alpha;
            let (ap, am) = (a + 1.0, a - 1.0);
            Coeffs::new(
                a * (ap - am * k + k2),
                2.0 * a * (am - ap * k),
                a * (ap - am * k - k2),
                ap + am * k + k2,
                -2.0 * (am + ap * k),
                ap + am * k - k2,
            )
        }
        FilterType::HighShelf => {
            if f >= 1.0 {
                return Coeffs::IDENTITY;
            }
            if f <= 0.0 {
                return Coeffs::gain(a * a);
            }
            let k2 = 2.0 * a.sqrt() * alpha;
            let (ap, am) = (a + 1.0, a - 1.0);
            Coeffs::new(
                a * (ap + am * k + k2),
                -2.0 * a * (am + ap * k),
                a * (ap + am * k - k2),
                ap - am * k + k2,
                2.0 * (am - ap * k),
                ap - am * k - k2,
            )
        }
        FilterType::LowPass => {
            if edge {
                return Coeffs::gain(f);
            }
            Coeffs::new((1.0 - k) / 2.0, 1.0 - k, (1.0 - k) / 2.0, 1.0 + alpha, -2.0 * k, 1.0 - alpha)
        }
        FilterType::HighPass => {
            if edge {
                return Coeffs::gain(1.0 - f);
            }
            Coeffs::new((1.0 + k) / 2.0, -(1.0 + k), (1.0 + k) / 2.0, 1.0 + alpha, -2.0 * k, 1.0 - alpha)
        }
    }
}

pub fn band_coeffs(band: &Band) -> Coeffs {
    coeffs(band.kind, band.freq, band.q, band.gain, PREVIEW_RATE)
}

/// Response of one band in dB, regardless of its enabled flag.
pub fn band_db(band: &Band, freq: f64) -> f64 {
    band_coeffs(band).db_at(freq, PREVIEW_RATE)
}

/// Combined response of all enabled bands in dB (no preamp).
pub fn eq_db(bands: &[Band], freq: f64) -> f64 {
    bands.iter().filter(|b| b.enabled).map(|b| band_db(b, freq)).sum()
}

/// Normalised crossfeed mixer gains `(direct, cross)`.
///
/// They sum to exactly 1, so in-phase (mono) bass, where the low-passed
/// opposite channel adds fully, comes out at unity and can never clip.
pub fn crossfeed_gains(feed_db: f64) -> (f64, f64) {
    let feed = db_to_lin(feed_db);
    (1.0 / (1.0 + feed), feed / (1.0 + feed))
}

/// Worst-case gain the crossfeed stage adds at `freq`, in dB (always <= 0).
fn crossfeed_db(cf: &Crossfeed, freq: f64) -> f64 {
    if !cf.enabled {
        return 0.0;
    }
    let (direct, cross) = crossfeed_gains(cf.feed_db);
    let lp = coeffs(FilterType::LowPass, CROSSFEED_HZ, CROSSFEED_Q, 0.0, PREVIEW_RATE);
    let lp_gain = db_to_lin(lp.db_at(freq, PREVIEW_RATE));
    20.0 * (direct + cross * lp_gain).log10()
}

/// Preamp that keeps the true peak of the combined response (EQ plus the
/// low-frequency crossfeed sum) just under 0 dBFS.
pub fn auto_preamp(bands: &[Band], cf: &Crossfeed) -> f64 {
    const POINTS: usize = 720;
    let (lo, hi) = (5f64.ln(), 23_000f64.ln());
    let grid = (0..=POINTS).map(|i| (lo + (hi - lo) * i as f64 / POINTS as f64).exp());
    // Narrow peaks can sit between grid points, so probe band centres too.
    let centres = bands.iter().filter(|b| b.enabled).map(|b| b.freq);
    let peak = grid
        .chain(centres)
        .map(|f| eq_db(bands, f) + crossfeed_db(cf, f))
        .fold(f64::MIN, f64::max);
    if peak <= 0.0 {
        0.0
    } else {
        -((peak + PREAMP_MARGIN_DB) * 10.0).ceil() / 10.0
    }
}

pub fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

// --- Preview engine ----------------------------------------------------------

/// Snapshot of everything the audio thread needs; published by the UI
/// thread through a triple buffer.
#[derive(Clone, Copy, PartialEq)]
pub struct PreviewParams {
    pub playing: bool,
    pub bypass: bool,
    pub freq: f64,
    /// Linear output level.
    pub level: f64,
    /// Linear preamp gain.
    pub preamp: f64,
    pub bands: usize,
    pub coeffs: [Coeffs; MAX_BANDS],
}

impl Default for PreviewParams {
    fn default() -> Self {
        PreviewParams {
            playing: false,
            bypass: false,
            freq: 1000.0,
            level: db_to_lin(LEVEL_DEFAULT_DB as f64),
            preamp: 1.0,
            bands: 0,
            coeffs: [Coeffs::IDENTITY; MAX_BANDS],
        }
    }
}

const FADE_SECONDS: f64 = 0.015;

/// Sine oscillator run through a real biquad cascade. Lives entirely in the
/// audio callback: no allocation, no locks.
pub struct Engine {
    phase: f64,
    freq: f64,
    /// Play/stop fade, 0..1.
    gate: f64,
    /// Bypass crossfade, 1 = fully through the EQ.
    wet: f64,
    level: f64,
    state: [[f64; 2]; MAX_BANDS],
}

impl Default for Engine {
    fn default() -> Self {
        Engine { phase: 0.0, freq: 1000.0, gate: 0.0, wet: 1.0, level: 0.0, state: [[0.0; 2]; MAX_BANDS] }
    }
}

impl Engine {
    /// True once a stop has fully faded out.
    pub fn silent(&self, p: &PreviewParams) -> bool {
        !p.playing && self.gate <= 0.0
    }

    /// Produce one mono sample.
    #[inline]
    pub fn next(&mut self, p: &PreviewParams) -> f32 {
        let fade = 1.0 / (FADE_SECONDS * PREVIEW_RATE);
        let ramp = |value: f64, on: bool| if on { (value + fade).min(1.0) } else { (value - fade).max(0.0) };
        self.gate = ramp(self.gate, p.playing);
        self.wet = ramp(self.wet, !p.bypass);
        // Glide frequency and level so cursor drags and slider moves stay click-free.
        self.freq += (p.freq - self.freq) * 0.004;
        self.level += (p.level - self.level) * 0.004;

        self.phase += 2.0 * PI * self.freq / PREVIEW_RATE;
        if self.phase >= 2.0 * PI {
            self.phase -= 2.0 * PI;
        }
        let dry = self.phase.sin();

        // Transposed direct form II; always running so its state stays warm
        // across bypass toggles.
        let mut x = dry;
        for (c, z) in p.coeffs[..p.bands.min(MAX_BANDS)].iter().zip(self.state.iter_mut()) {
            let y = c.b0 * x + z[0];
            z[0] = c.b1 * x - c.a1 * y + z[1];
            z[1] = c.b2 * x - c.a2 * y;
            x = y;
        }
        let wet = x * p.preamp;

        let out = (dry * (1.0 - self.wet) + wet * self.wet) * self.level * self.gate;
        let cap = db_to_lin(LEVEL_CAP_DB as f64);
        out.clamp(-cap, cap) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(kind: FilterType, freq: f64, gain: f64, q: f64) -> Band {
        Band { kind, freq, gain, q, ..Band::peak(freq) }
    }

    #[test]
    fn peaking_hits_its_gain_at_centre() {
        let b = band(FilterType::Peak, 1000.0, 6.0, 2.0);
        assert!((band_db(&b, 1000.0) - 6.0).abs() < 1e-9);
        assert!(band_db(&b, 20.0).abs() < 0.05);
    }

    #[test]
    fn shelves_reach_gain_and_half_gain_at_corner() {
        let ls = band(FilterType::LowShelf, 105.0, 3.5, 0.71);
        assert!((band_db(&ls, 5.0) - 3.5).abs() < 0.05);
        assert!((band_db(&ls, 105.0) - 1.75).abs() < 0.01);
        let hs = band(FilterType::HighShelf, 8000.0, -4.0, 0.71);
        assert!((band_db(&hs, 8000.0) + 2.0).abs() < 0.01);
        assert!(band_db(&hs, 50.0).abs() < 0.01);
    }

    #[test]
    fn pass_filters_follow_q_at_cutoff() {
        // RBJ low/high-pass magnitude at the corner equals Q.
        let lp = band(FilterType::LowPass, 700.0, 0.0, 0.5);
        assert!((band_db(&lp, 700.0) - 20.0 * 0.5f64.log10()).abs() < 1e-6);
        let hp = band(FilterType::HighPass, 80.0, 0.0, 0.707);
        assert!((band_db(&hp, 80.0) - 20.0 * 0.707f64.log10()).abs() < 1e-6);
    }

    #[test]
    fn preamp_is_a_pure_gain_like_pipewire() {
        // param_eq encodes the preamp as a high shelf at 0 Hz.
        let c = coeffs(FilterType::HighShelf, 0.0, 1.0, -4.2, PREVIEW_RATE);
        assert!((c.db_at(1000.0, PREVIEW_RATE) + 4.2).abs() < 1e-9);
    }

    #[test]
    fn auto_preamp_finds_the_constructive_peak() {
        let bands = [band(FilterType::Peak, 1000.0, 4.0, 1.0), band(FilterType::Peak, 1200.0, 4.0, 1.0)];
        let p = auto_preamp(&bands, &Crossfeed::default());
        assert!(p < -4.2 - 2.0, "overlapping boosts must sum: {p}");
        let peak = (0..2000).map(|i| eq_db(&bands, 20.0 * 1.0035f64.powi(i))).fold(f64::MIN, f64::max);
        assert!(peak + p <= -PREAMP_MARGIN_DB + 1e-9 && peak + p > -0.5);
        assert_eq!(auto_preamp(&[band(FilterType::Peak, 500.0, -5.0, 1.0)], &Crossfeed::default()), 0.0);
    }

    #[test]
    fn crossfeed_never_exceeds_unity() {
        let cf = Crossfeed { enabled: true, ..Default::default() };
        let (d, c) = crossfeed_gains(cf.feed_db);
        assert!((d + c - 1.0).abs() < 1e-12);
        for f in [10.0, 100.0, 700.0, 5000.0, 20000.0] {
            assert!(crossfeed_db(&cf, f) <= 1e-9);
        }
        // High-frequency boosts need less preamp once crossfeed attenuates them.
        let bands = [band(FilterType::Peak, 8000.0, 6.0, 1.0)];
        assert!(auto_preamp(&bands, &cf) > auto_preamp(&bands, &Crossfeed::default()));
    }

    #[test]
    fn engine_matches_the_analytic_response_and_respects_the_cap() {
        let b = band(FilterType::Peak, 2001.7, -9.0, 1.5);
        let mut p = PreviewParams { playing: true, freq: 2001.7, level: 0.25, bands: 1, ..Default::default() };
        p.coeffs[0] = band_coeffs(&b);
        let mut e = Engine::default();
        let peak = (0..48_000).map(|_| e.next(&p)).skip(24_000).fold(0f32, |m, s| m.max(s.abs()));
        assert!((20.0 * (peak as f64 / 0.25).log10() + 9.0).abs() < 0.05);

        p.level = 4.0;
        p.bypass = true;
        let peak = (0..48_000).map(|_| e.next(&p)).fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak <= db_to_lin(LEVEL_CAP_DB as f64) as f32 + 1e-6);

        p.playing = false;
        for _ in 0..2000 {
            e.next(&p);
        }
        assert!(e.silent(&p) && e.next(&p) == 0.0);
    }
}
