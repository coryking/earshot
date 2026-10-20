//! Channel filtering against real off-air IQ (Pluto, 6 Msps at LO 454.216998 MHz).
//! The recordings are private and Git-ignored; point RADIO_WIDEBAND_WAV at
//! recordings/baseband_454216998Hz_wide-30s_*.wav to run:
//!
//!   RADIO_WIDEBAND_WAV=... cargo test --release --test real_iq -- --ignored --nocapture
//!
//! A coarse 100 ksps slice is cut from the 6 Msps stream the way the FPGA slicer will
//! (mix, Kaiser lowpass, decimate by 60). Each 12.5 kHz channel in it is then filtered
//! with the production channel filter and compared with the slice's spectrum.
use num_complex::Complex32;
use radio_engine::dsp::{ChannelDsp, ChannelSpec, Resampler, SliceMeter, TargetBand};
use radio_engine::fm::{FmAudio, Squelch};
use std::{fs::File, io::{Read, Seek, SeekFrom}};

const LO: f64 = 454_216_998.;
const RATE: f64 = 6_000_000.;

fn read_iq(path: &str, start_s: f64, seconds: f64) -> Vec<(i16, i16)> {
    let mut f = File::open(path).unwrap();
    f.seek(SeekFrom::Start(44 + (start_s * RATE) as u64 * 4)).unwrap();
    let n = (seconds * RATE) as usize;
    let mut raw = vec![0u8; n * 4];
    f.read_exact(&mut raw).unwrap();
    raw.chunks_exact(4)
        .map(|b| (i16::from_le_bytes([b[0], b[1]]), i16::from_le_bytes([b[2], b[3]])))
        .collect()
}
/// Mix `center` to baseband and decimate 6 Msps -> 100 ksps with a Kaiser lowpass.
fn slice(samples: &[(i16, i16)], center: f64) -> Vec<Complex32> {
    let spec = ChannelSpec { pass_hz: 42_000., stop_hz: 55_000., atten_db: 80., band_hz: 0. };
    let mut decimator = Resampler::new(RATE, 100_000., spec);
    let mut out = Vec::new();
    let step = -2. * std::f64::consts::PI * (center - LO) / RATE;
    for (c, chunk) in samples.chunks(500_000).enumerate() {
        let mixed: Vec<Complex32> = chunk
            .iter()
            .enumerate()
            .map(|(i, &(re, im))| {
                let n = (c * 500_000 + i) as f64;
                let rot = Complex32::from_polar(1., ((step * n) % (2. * std::f64::consts::PI)) as f32);
                Complex32::new(re as f32, im as f32) * rot
            })
            .collect();
        out.extend(decimator.push(&mixed));
    }
    out
}
fn db(x: f64) -> f64 {
    10. * x.max(1e-30).log10()
}

#[test]
#[ignore = "needs RADIO_WIDEBAND_WAV (private recording)"]
fn real_adjacent_channels_are_not_polluted_by_strong_neighbors() {
    let path = std::env::var("RADIO_WIDEBAND_WAV").expect("RADIO_WIDEBAND_WAV");
    // Slice centered 20 kHz below the always-on 454.217 MHz emitter, so the emitter and
    // its +-12.5 kHz neighbors (and the 454.2607 / 454.2477 emitters) fall inside +-50 kHz.
    let center = 454_200_000.;
    let samples = read_iq(&path, 2., 4.);
    let iq = slice(&samples, center);
    let iq = &iq[2000..];
    println!("slice samples {}", iq.len());
    // Candidate channels on the 12.5 kHz raster around the emitter.
    let raster: Vec<f64> = (-3..=3).map(|k| 454_217_000. + 12_500. * k as f64).collect();
    let mut meter = SliceMeter::new(
        raster
            .iter()
            .map(|&f| TargetBand { logical: f as u64, offset_hz: f - center, bandwidth_hz: 11_250., active: false, level_dbfs: 0. })
            .collect(),
    );
    // Many short packets average the same way the live meter would.
    let mut fft_power = vec![0.; raster.len()];
    let mut packets = 0;
    for packet in iq.chunks_exact(10_000) {
        assert!(meter.measure(packet, 100_000., 32768.));
        for (p, t) in fft_power.iter_mut().zip(&meter.targets) {
            *p += 10f64.powf(t.level_dbfs / 10.);
        }
        packets += 1;
    }
    let fft_db: Vec<f64> = fft_power.iter().map(|p| db(p / packets as f64)).collect();
    let mut filtered_db = vec![];
    for &f in &raster {
        let mut dsp = ChannelDsp::new(100_000., f - center, ChannelSpec::CHANNEL_12_5);
        let out = dsp.filter(iq);
        let p = out[out.len() / 10..].iter().map(|x| x.norm_sqr() as f64).sum::<f64>() / (out.len() * 9 / 10) as f64;
        filtered_db.push(db(p / (32768f64 * 32768.)));
    }
    let strongest = fft_db.iter().cloned().fold(f64::MIN, f64::max);
    for (i, f) in raster.iter().enumerate() {
        println!(
            "{:.4} MHz: spectrum band power {:7.1} dBFS, filtered channel power {:7.1} dBFS ({:+.1} dB), {:5.1} dB below strongest",
            f / 1e6, fft_db[i], filtered_db[i], filtered_db[i] - fft_db[i], strongest - fft_db[i]
        );
    }
    // Where the spectrum shows a neighbor at least 25 dB below the strongest channel, the
    // filter must not report more than 3 dB above the spectrum's own in-band power: any
    // leakage from the strong neighbor would show up as excess here.
    let mut checked = 0;
    for i in 0..raster.len() {
        let adjacent_to_strong = [i.wrapping_sub(1), i + 1]
            .iter()
            .any(|&j| j < raster.len() && fft_db[j] > fft_db[i] + 25.);
        if adjacent_to_strong {
            checked += 1;
            assert!(
                filtered_db[i] - fft_db[i] < 3.,
                "{:.4} MHz: filtered {:.1} dBFS vs spectrum {:.1} dBFS",
                raster[i] / 1e6, filtered_db[i], fft_db[i]
            );
        }
    }
    assert!(checked > 0, "no weak channel next to a strong one in this recording");
    // In-channel power of the strongest channel must agree with the spectrum.
    let k = fft_db.iter().position(|&v| v == strongest).unwrap();
    assert!((filtered_db[k] - fft_db[k]).abs() < 1.5, "strong channel mismatch");
}

/// The noise squelch against real channels: empty channels stay shut, strong carriers open
/// their quieting measurement. Thresholds are calibrated on this recording (see fm.rs).
#[test]
#[ignore = "needs RADIO_WIDEBAND_WAV (private recording)"]
fn fm_noise_squelch_on_real_channels() {
    let path = std::env::var("RADIO_WIDEBAND_WAV").expect("RADIO_WIDEBAND_WAV");
    let samples = read_iq(&path, 2., 4.);
    let pct = |v: &mut Vec<f32>, q: f64| {
        v.sort_by(f32::total_cmp);
        v[((v.len() - 1) as f64 * q) as usize]
    };
    let mut report = vec![];
    for (name, f) in [
        ("empty 454.1795", 454_179_500.),
        ("empty 454.2295", 454_229_500.),
        ("DMR control 454.217", 454_217_000.),
        ("narrow carrier 454.2605", 454_260_500.),
    ] {
        // A 100 ksps slice 20 kHz below the channel, as the slicer would deliver it.
        let center = f - 20_000.;
        let iq = slice(&samples, center);
        let mut dsp = ChannelDsp::new(100_000., f - center, ChannelSpec::CHANNEL_12_5);
        let pcm = dsp.push(&iq[2000..]);
        let mut fm = FmAudio::new();
        let mut squelch = Squelch::default();
        let (mut noise, mut voice, mut open) = (vec![], vec![], 0);
        let blocks: Vec<_> = pcm.chunks_exact(4800).skip(1).collect();
        for block in &blocks {
            let b = fm.push(block);
            noise.push(b.noise_hz);
            voice.push(b.voice_hz);
            if squelch.update(&b) {
                open += 1;
            }
        }
        let line = format!(
            "{name}: noise_hz p10/50/90 {:.0}/{:.0}/{:.0}, voice_hz median {:.0}, squelch open {}/{}",
            pct(&mut noise.clone(), 0.1),
            pct(&mut noise.clone(), 0.5),
            pct(&mut noise.clone(), 0.9),
            pct(&mut voice, 0.5),
            open,
            blocks.len()
        );
        println!("{line}");
        report.push((name, pct(&mut noise, 0.5), open, blocks.len()));
    }
    for (name, median_noise, open, total) in report {
        if name.starts_with("empty") {
            assert!(open * 20 < total, "{name}: squelch open {open}/{total}, noise {median_noise}");
        } else {
            assert!(median_noise < 700., "{name}: median noise {median_noise} Hz not quieted");
        }
    }
}
