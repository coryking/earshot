use num_complex::Complex32;
/// Causal windowed sinc resampler. Output clock and history persist across packets.
pub struct Resampler {
    input_rate: f64,
    output_rate: f64,
    next: f64,
    total: u64,
    history: Vec<Complex32>,
    radius: i64,
    coefficients: Vec<Vec<f32>>,
}
impl Resampler {
    pub fn new(input: f64, output: f64) -> Self {
        assert!(input.is_finite() && output.is_finite() && input > 0. && output > 0.);
        let cutoff = (output.min(12500.) / input).min(1.) * 0.98;
        let radius = (32. / cutoff).ceil() as i64;
        let ratio = input / output;
        let phases = (1..=1024)
            .find(|n| {
                let x = ratio * (*n as f64);
                (x - x.round()).abs() < 1e-8
            })
            .unwrap_or(1024);
        let coefficients = (0..phases)
            .map(|phase| {
                let fraction = phase as f64 / phases as f64;
                let mut row: Vec<f32> = (-radius..=radius)
                    .map(|index| {
                        let x = index as f64 - fraction;
                        let t = x * cutoff;
                        let sinc = if t.abs() < 1e-10 {
                            1.
                        } else {
                            (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                        };
                        let window = if x.abs() <= radius as f64 {
                            0.5 + 0.5 * (std::f64::consts::PI * x / radius as f64).cos()
                        } else {
                            0.
                        };
                        (sinc * window * cutoff) as f32
                    })
                    .collect();
                let sum: f32 = row.iter().sum();
                for v in &mut row {
                    *v /= sum;
                }
                row
            })
            .collect();
        Self {
            input_rate: input,
            output_rate: output,
            next: 0.,
            total: 0,
            history: vec![],
            radius,
            coefficients,
        }
    }
    pub fn push(&mut self, input: &[Complex32]) -> Vec<Complex32> {
        let start = self.total as i64 - self.history.len() as i64;
        self.history.extend_from_slice(input);
        self.total += input.len() as u64;
        let mut output = Vec::with_capacity(
            (input.len() as f64 * self.output_rate / self.input_rate).ceil() as usize,
        );
        while self.next + (self.radius as f64) + 1. < (self.total as f64) {
            let mut center = self.next.floor() as i64;
            let phases = self.coefficients.len();
            let mut phase = ((self.next - center as f64) * phases as f64).round() as usize;
            if phase == phases {
                phase = 0;
                center += 1;
            }
            let first = (center - self.radius).max(start).max(0);
            let last = (center + self.radius + 1).min(self.total as i64);
            let row = &self.coefficients[phase];
            let offset = (first - (center - self.radius)) as usize;
            let mut acc = Complex32::new(0., 0.);
            for (sample, weight) in self.history[(first - start) as usize..(last - start) as usize]
                .iter()
                .zip(&row[offset..offset + (last - first) as usize])
            {
                acc += *sample * *weight;
            }
            output.push(acc);
            self.next += self.input_rate / self.output_rate;
        }
        let keep_from = (self.next.floor() as i64 - self.radius - 2)
            .max(start)
            .max(0);
        self.history.drain(..(keep_from - start) as usize);
        output
    }
}
pub struct ChannelDsp {
    resample: Resampler,
    phase: Complex32,
    step: Complex32,
    previous: Option<Complex32>,
    dc_x: f32,
    dc_y: f32,
    count: u64,
}
impl ChannelDsp {
    pub fn new(rate: f64, offset: f64) -> Self {
        Self {
            resample: Resampler::new(rate, 48000.),
            phase: Complex32::new(1., 0.),
            step: Complex32::from_polar(1., (-2. * std::f64::consts::PI * offset / rate) as f32),
            previous: None,
            dc_x: 0.,
            dc_y: 0.,
            count: 0,
        }
    }
    pub fn push(&mut self, iq: &[Complex32]) -> Vec<i16> {
        let mixed: Vec<_> = iq
            .iter()
            .map(|x| {
                let y = *x * self.phase;
                self.phase *= self.step;
                self.count += 1;
                if self.count.is_multiple_of(4096) {
                    self.phase /= self.phase.norm();
                }
                y
            })
            .collect();
        self.resample
            .push(&mixed)
            .iter()
            .map(|&x| {
                let p = self.previous.unwrap_or(x);
                self.previous = Some(x);
                let fm = (x * p.conj()).arg();
                let dc = fm - self.dc_x + 0.999 * self.dc_y;
                self.dc_x = fm;
                self.dc_y = dc;
                (dc * 48000. / (2. * std::f32::consts::PI * 5000.) * 16000.).clamp(-32768., 32767.)
                    as i16
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_boundaries_do_not_change_dsp() {
        let data: Vec<_> = (0..25000)
            .map(|i| Complex32::from_polar(1000., i as f32 * 0.04))
            .collect();
        let mut a = ChannelDsp::new(25000., 0.);
        let all = a.push(&data);
        let mut b = ChannelDsp::new(25000., 0.);
        let chunks: Vec<_> = data.chunks(127).flat_map(|c| b.push(c)).collect();
        assert_eq!(all, chunks);
    }
}
