use num_complex::Complex32;
/// Causal windowed sinc resampler. Output clock and history persist across packets.
pub struct Resampler {
    input_rate: f64,
    output_rate: f64,
    next: f64,
    total: u64,
    history: Vec<Complex32>,
    radius: usize,
}
impl Resampler {
    pub fn new(input: f64, output: f64) -> Self {
        Self {
            input_rate: input,
            output_rate: output,
            next: 0.,
            total: 0,
            history: vec![],
            radius: 32,
        }
    }
    pub fn push(&mut self, input: &[Complex32]) -> Vec<Complex32> {
        let start = self.total as i64 - self.history.len() as i64;
        self.history.extend_from_slice(input);
        self.total += input.len() as u64;
        let mut output = vec![];
        let cutoff = (self.output_rate.min(12500.) / self.input_rate).min(1.) * 0.98;
        let radius = (self.radius as f64 / cutoff).ceil() as i64;
        while self.next + (radius as f64) < (self.total as f64) {
            let center = self.next.floor() as i64;
            let mut acc = Complex32::new(0., 0.);
            let mut norm = 0.;
            for index in center - radius..=center + radius {
                let x = index as f64 - self.next;
                let t = x * cutoff;
                let sinc = if t.abs() < 1e-10 {
                    1.
                } else {
                    (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                };
                let window = 0.5 + 0.5 * (std::f64::consts::PI * x / radius as f64).cos();
                let w = (sinc * window * cutoff) as f32;
                if index >= start && index >= 0 {
                    acc += self.history[(index - start) as usize] * w;
                }
                norm += w;
            }
            output.push(acc / norm);
            self.next += self.input_rate / self.output_rate;
        }
        let keep_from = (self.next.floor() as i64 - radius - 1).max(start).max(0);
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
