//! Domain: the breaches the guard finds in one sample, and the level of each.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Level {
    Hold,
    Pause,
    Kill,
}

impl Level {
    pub(super) fn label(self) -> &'static str {
        match self {
            Level::Hold => "hold",
            Level::Pause => "pause",
            Level::Kill => "kill",
        }
    }
}

/// Breaches found in one sample.
#[derive(Debug, Default)]
pub(super) struct Findings(pub(super) Vec<(Level, String)>);

impl Findings {
    pub(super) fn add(&mut self, level: Level, reason: String) {
        self.0.push((level, reason));
    }

    pub(super) fn has(&self, level: Level) -> bool {
        self.0.iter().any(|(l, _)| *l == level)
    }

    /// Kill-level when `value < kill`, else pause when `value < pause`.
    pub(super) fn below(
        &mut self,
        what: &str,
        value: f64,
        limits: (Option<f64>, Option<f64>),
        unit: &str,
    ) {
        self.below_with(Level::Pause, what, value, limits, unit);
    }

    /// Like `below`, with `soft` as the level of the first line.
    pub(super) fn below_with(
        &mut self,
        soft: Level,
        what: &str,
        value: f64,
        limits: (Option<f64>, Option<f64>),
        unit: &str,
    ) {
        let (first, kill) = limits;
        if let Some(k) = kill.filter(|k| value < *k) {
            self.add(Level::Kill, format!("{what} {value:.1}{unit} < {k}{unit}"));
        } else if let Some(p) = first.filter(|p| value < *p) {
            self.add(soft, format!("{what} {value:.1}{unit} < {p}{unit}"));
        }
    }

    /// Kill-level when `value > kill`, else pause when `value > pause`.
    pub(super) fn above(
        &mut self,
        what: &str,
        value: f64,
        limits: (Option<f64>, Option<f64>),
        unit: &str,
    ) {
        let (pause, kill) = limits;
        if let Some(k) = kill.filter(|k| value > *k) {
            self.add(Level::Kill, format!("{what} {value:.1}{unit} > {k}{unit}"));
        } else if let Some(p) = pause.filter(|p| value > *p) {
            self.add(Level::Pause, format!("{what} {value:.1}{unit} > {p}{unit}"));
        }
    }
}
