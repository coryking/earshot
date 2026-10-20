//! Frequency calibration, raster snapping and plain-name labels.
//!
//! The Pluto's reference oscillator is off by tens of ppm, so every frequency the device
//! reports is a *reading*. A single ppm correction maps readings to true frequencies
//! (`true = reading / (1 + ppm * 1e-6)`; positive means the radio reads high). Land-mobile
//! channels sit on a 6.25 kHz raster (12.5 kHz channels on a 6.25 kHz-offset grid are
//! common in 450 to 470 MHz), so a measured carrier within a tolerance of a raster point
//! snaps to it. The labels table is keyed by raster point and built from RadioReference and
//! FCC ULS data by `offload/build_labels.py`.
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};

pub const RASTER_HZ: f64 = 6250.;
/// A carrier this close to a raster point is taken to be on it. The residual after the ppm
/// correction measured on real carriers is about +-0.4 kHz (docs/rf-environment-seattle.md).
pub const SNAP_TOLERANCE_HZ: f64 = 1500.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Calibration {
    pub ppm: f64,
}
impl Calibration {
    pub fn true_hz(&self, reading_hz: f64) -> f64 {
        reading_hz / (1. + self.ppm * 1e-6)
    }
}
/// Nearest raster point when within tolerance, else the value rounded to 100 Hz.
/// The flag says whether it snapped.
pub fn snap(true_hz: f64) -> (f64, bool) {
    let nearest = (true_hz / RASTER_HZ).round() * RASTER_HZ;
    if (true_hz - nearest).abs() <= SNAP_TOLERANCE_HZ {
        (nearest, true)
    } else {
        ((true_hz / 100.).round() * 100., false)
    }
}
#[derive(Clone, Debug, Default)]
pub struct Labels {
    channels: HashMap<i64, Value>,
}
impl Labels {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let file: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        let channels = file["channels"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("labels file has no channels"))?
            .iter()
            .filter_map(|(hz, v)| Some((hz.parse::<i64>().ok()?, v.clone())))
            .collect();
        Ok(Self { channels })
    }
    pub fn len(&self) -> usize {
        self.channels.len()
    }
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }
    /// Label for a true frequency: the raster point it snaps to, when one exists.
    pub fn lookup(&self, true_hz: f64) -> Option<&Value> {
        let (hz, snapped) = snap(true_hz);
        snapped.then(|| self.channels.get(&(hz as i64))).flatten()
    }
}
/// JSON fields describing a label (or its absence) for a channel entry.
pub fn label_fields(label: Option<&Value>) -> Value {
    match label {
        Some(l) => json!({
            "label": l["name"],
            "label_kind": l["kind"],
            "label_detail": l,
        }),
        None => json!({"label": null, "label_kind": null, "label_detail": null}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ppm_correction_maps_the_pluto_reading_to_the_true_frequency() {
        // 23.3 ppm high: a carrier read at 454.217000 MHz is really 454.206419 MHz, within
        // 170 Hz of the 454.20625 MHz Day Wireless control channel.
        let cal = Calibration { ppm: 23.3 };
        let truth = cal.true_hz(454_217_000.);
        assert!((truth - 454_206_419.).abs() < 5., "{truth}");
        assert_eq!(snap(truth), (454_206_250., true));
        assert_eq!(Calibration { ppm: 0. }.true_hz(454_217_000.), 454_217_000.);
    }
    #[test]
    fn snapping_respects_the_tolerance() {
        assert_eq!(snap(454_031_250. + 1400.), (454_031_250., true));
        let (hz, snapped) = snap(454_029_498.);
        assert!(!snapped && hz == 454_029_500., "{hz}");
        // A frequency exactly between raster points does not snap.
        assert!(!snap(454_028_125.).1);
    }
    #[test]
    fn labels_load_and_match_by_raster_point() {
        let dir = std::env::temp_dir().join(format!("labels-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("labels.json");
        std::fs::write(
            &path,
            r#"{"channels":{"454206250":{"name":"Day Wireless (business trunked)","kind":"control"}}}"#,
        )
        .unwrap();
        let labels = Labels::load(&path).unwrap();
        assert_eq!(labels.len(), 1);
        let hit = labels.lookup(454_206_419.).expect("label");
        assert_eq!(hit["name"], "Day Wireless (business trunked)");
        assert!(labels.lookup(454_200_000.).is_none());
        let fields = label_fields(Some(hit));
        assert_eq!(fields["label"], "Day Wireless (business trunked)");
        assert_eq!(label_fields(None)["label"], Value::Null);
    }
    #[test]
    fn label_lookup_edges_and_negative_ppm() {
        let dir = std::env::temp_dir().join(format!("labels-edge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("labels.json");
        std::fs::write(&path, r#"{"channels":{"454206250":{"name":"A"},"not-a-number":{"name":"B"}}}"#).unwrap();
        let labels = Labels::load(&path).unwrap();
        assert_eq!(labels.len(), 1, "non-numeric keys are skipped");
        // Midway between raster points nothing snaps, so a neighbor's label is not borrowed.
        assert!(labels.lookup(454_203_125.).is_none());
        assert!(labels.lookup(454_209_375.).is_none());
        std::fs::write(&path, r#"{"nothing":true}"#).unwrap();
        assert!(Labels::load(&path).is_err(), "a file without channels is an error");
        // A radio that reads LOW needs a negative ppm; the formula is sign-symmetric.
        let cal = Calibration { ppm: -23.3 };
        let truth = cal.true_hz(454_196_000.);
        assert!((truth - 454_206_582.).abs() < 5., "{truth}");
        assert_eq!(snap(truth).1, true);
    }
    #[test]
    fn the_shipped_labels_file_parses_and_names_known_channels() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("data/channel-labels.json");
        let labels = Labels::load(&path).unwrap();
        assert!(labels.len() > 500);
        assert_eq!(
            labels.lookup(454_206_250.).unwrap()["name"],
            "Day Wireless (business trunked)"
        );
        assert_eq!(labels.lookup(451_350_000.).unwrap()["name"], "Convention Center radio");
    }
}
