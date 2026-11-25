//! Core data model: bands, crossfeed and the tuning workspace.

/// PipeWire's `param_eq` holds 64 biquads per channel; one is the preamp.
pub const MAX_BANDS: usize = 63;

pub const FREQ_MIN: f64 = 20.0;
pub const FREQ_MAX: f64 = 20_000.0;
pub const Q_MIN: f64 = 0.1;
pub const Q_MAX: f64 = 18.0;
pub const GAIN_LIMIT: f64 = 30.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterType {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
}

impl FilterType {
    pub const ALL: [FilterType; 5] = [
        FilterType::Peak,
        FilterType::LowShelf,
        FilterType::HighShelf,
        FilterType::LowPass,
        FilterType::HighPass,
    ];

    /// Equalizer APO token, also used as the short UI label.
    /// Spelled-out name for the UI.
    pub fn name(self) -> &'static str {
        match self {
            FilterType::Peak => "Peak",
            FilterType::LowShelf => "Low Shelf",
            FilterType::HighShelf => "High Shelf",
            FilterType::LowPass => "Low Pass",
            FilterType::HighPass => "High Pass",
        }
    }

    /// Equalizer APO token, used in preset files.
    pub fn label(self) -> &'static str {
        match self {
            FilterType::Peak => "PK",
            FilterType::LowShelf => "LSC",
            FilterType::HighShelf => "HSC",
            FilterType::LowPass => "LP",
            FilterType::HighPass => "HP",
        }
    }

    pub fn from_apo(token: &str) -> Option<Self> {
        Some(match token.to_ascii_uppercase().as_str() {
            "PK" | "PEQ" => FilterType::Peak,
            "LSC" | "LS" => FilterType::LowShelf,
            "HSC" | "HS" => FilterType::HighShelf,
            "LP" | "LPQ" => FilterType::LowPass,
            "HP" | "HPQ" => FilterType::HighPass,
            _ => return None,
        })
    }

    /// Label of the matching PipeWire builtin biquad.
    pub fn pipewire_label(self) -> &'static str {
        match self {
            FilterType::Peak => "bq_peaking",
            FilterType::LowShelf => "bq_lowshelf",
            FilterType::HighShelf => "bq_highshelf",
            FilterType::LowPass => "bq_lowpass",
            FilterType::HighPass => "bq_highpass",
        }
    }

    pub fn has_gain(self) -> bool {
        !matches!(self, FilterType::LowPass | FilterType::HighPass)
    }
}

/// Which channel a band applies to. Locked to `Stereo` in v1; v2 will emit
/// `param_eq`'s per-channel `filters1` / `filters2` arrays from this.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(dead_code)]
pub enum ChannelScope {
    #[default]
    Stereo,
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Band {
    pub enabled: bool,
    pub kind: FilterType,
    pub freq: f64,
    pub gain: f64,
    pub q: f64,
    pub scope: ChannelScope,
}

impl Band {
    pub fn peak(freq: f64) -> Self {
        Band {
            enabled: true,
            kind: FilterType::Peak,
            freq,
            gain: 0.0,
            q: 1.41,
            scope: ChannelScope::Stereo,
        }
    }

    /// Clamp to legal ranges and round to the precision the preset format
    /// stores, so the workspace always round-trips through text exactly.
    pub fn normalize(&mut self) {
        self.freq = round_to(self.freq.clamp(FREQ_MIN, FREQ_MAX), 10.0);
        self.gain = round_to(self.gain.clamp(-GAIN_LIMIT, GAIN_LIMIT), 10.0);
        self.q = round_to(self.q.clamp(Q_MIN, Q_MAX), 100.0);
    }
}

/// Crossfeed feed range. The floor matches BS2B's lightest setting (15 dB);
/// the ceiling stops short of a near-mono blend.
pub const FEED_MIN: f64 = -15.0;
pub const FEED_MAX: f64 = -3.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Crossfeed {
    pub enabled: bool,
    pub feed_db: f64,
}

impl Default for Crossfeed {
    fn default() -> Self {
        Crossfeed {
            enabled: false,
            feed_db: -6.0,
        }
    }
}

/// Everything the user tunes: the content of a preset.
#[derive(Clone, PartialEq, Debug)]
pub struct Workspace {
    pub bands: Vec<Band>,
    pub auto_preamp: bool,
    /// Used only while `auto_preamp` is off.
    pub manual_preamp: f64,
    pub crossfeed: Crossfeed,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace {
            bands: Vec::new(),
            auto_preamp: true,
            manual_preamp: 0.0,
            crossfeed: Crossfeed::default(),
        }
    }
}

impl Workspace {
    /// Effective preamp in dB.
    pub fn preamp_db(&self) -> f64 {
        if self.auto_preamp {
            crate::dsp::auto_preamp(&self.bands, &self.crossfeed)
        } else {
            self.manual_preamp
        }
    }

    pub fn normalize(&mut self) {
        self.bands.truncate(MAX_BANDS);
        self.bands.iter_mut().for_each(Band::normalize);
        self.manual_preamp = round_to(self.manual_preamp.clamp(-GAIN_LIMIT, GAIN_LIMIT), 10.0);
        self.crossfeed.feed_db = round_to(self.crossfeed.feed_db.clamp(FEED_MIN, FEED_MAX), 10.0);
    }
}

fn round_to(v: f64, scale: f64) -> f64 {
    (v * scale).round() / scale
}
