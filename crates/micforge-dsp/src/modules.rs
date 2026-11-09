//! FAZ 2 modülleri: giriş kademesi, high-pass, 3 bantlı ton, çıkış (kazanç + denge).

use crate::Processor;
use crate::biquad::{Biquad, Coeffs, FilterType, butterworth_qs};
use crate::db_to_lin;
use crate::smooth::Smoother;

/// Filtre katsayılarının yumuşatma sırasında yeniden hesaplanma aralığı (örnek).
const COEF_BLOCK: usize = 32;

/// Stereo girişten mono'ya indirgeme kipi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelMode {
    Average,
    Left,
    Right,
}

impl ChannelMode {
    pub fn from_index(i: f32) -> Self {
        match i.round() as i32 {
            1 => ChannelMode::Left,
            2 => ChannelMode::Right,
            _ => ChannelMode::Average,
        }
    }

    #[inline]
    pub fn downmix(self, l: f32, r: f32) -> f32 {
        match self {
            ChannelMode::Average => 0.5 * (l + r),
            ChannelMode::Left => l,
            ChannelMode::Right => r,
        }
    }
}

/// Giriş kazancı (dB) ve faz ters çevirme.
pub struct InputStage {
    gain: Smoother,
    /// +1 veya -1'e yumuşatılan polarite: ters çevirmede tık olmasın.
    polarity: Smoother,
}

impl InputStage {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            gain: Smoother::new(1.0, sample_rate),
            polarity: Smoother::new(1.0, sample_rate),
        }
    }

    pub fn set(&mut self, gain_db: f32, invert: bool) {
        self.gain.set_target(db_to_lin(gain_db));
        self.polarity.set_target(if invert { -1.0 } else { 1.0 });
    }
}

impl Processor for InputStage {
    fn process(&mut self, buf: &mut [f32]) {
        if !self.gain.is_active() && !self.polarity.is_active() {
            let g = self.gain.value() * self.polarity.value();
            if g != 1.0 {
                buf.iter_mut().for_each(|s| *s *= g);
            }
            return;
        }
        for s in buf {
            *s *= self.gain.next_value() * self.polarity.next_value();
        }
    }

    fn reset(&mut self) {
        self.gain.snap(self.gain.target());
        self.polarity.snap(self.polarity.target());
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.gain.set_sample_rate(sample_rate);
        self.polarity.set_sample_rate(sample_rate);
    }
}

/// Seçilebilir eğimli (12/24/48 dB/okt) Butterworth high-pass.
pub struct HighPass {
    sample_rate: f32,
    stages: [Biquad; 4],
    n_stages: usize,
    /// Frekans log alanında yumuşatılır (kulağa doğal gelen süpürme).
    log_freq: Smoother,
}

impl HighPass {
    pub fn new(sample_rate: f32, freq: f32) -> Self {
        let mut h = Self {
            sample_rate,
            stages: Default::default(),
            n_stages: 2,
            log_freq: Smoother::new(freq.ln(), sample_rate),
        };
        h.update_coeffs();
        h
    }

    /// `slope_index`: 0 = 12, 1 = 24, 2 = 48 dB/okt.
    pub fn set(&mut self, freq: f32, slope_index: usize) {
        self.log_freq.set_target(freq.ln());
        let n = [1, 2, 4][slope_index.min(2)];
        if n != self.n_stages {
            // Eğim değişince tüm katların Butterworth Q'ları değişir (1 kat: 0.707,
            // 2 kat: 0.541/1.307, ...). Eski durumla yeni katsayılar uyumsuz olur
            // (tık/geçici salınım), bu yüzden ilk kat dahil hepsi sıfırlanır.
            for s in &mut self.stages {
                s.reset();
            }
            self.n_stages = n;
            self.update_coeffs();
        }
    }

    fn update_coeffs(&mut self) {
        let f = self.log_freq.value().exp();
        let order = self.n_stages * 2;
        for (s, q) in self.stages.iter_mut().zip(butterworth_qs(order)) {
            s.set_coeffs(Coeffs::new(
                FilterType::HighPass,
                self.sample_rate,
                f,
                q,
                0.0,
            ));
        }
    }

    /// Toplam genlik tepkisi (dB).
    pub fn magnitude_db(&self, freq: f32) -> f32 {
        self.stages[..self.n_stages]
            .iter()
            .map(|s| s.coeffs().magnitude_db(freq, self.sample_rate))
            .sum()
    }
}

impl Processor for HighPass {
    fn process(&mut self, buf: &mut [f32]) {
        for block in buf.chunks_mut(COEF_BLOCK) {
            if self.log_freq.is_active() {
                self.log_freq.advance(block.len());
                self.update_coeffs();
            }
            for s in &mut self.stages[..self.n_stages] {
                s.process(block);
            }
        }
    }

    fn reset(&mut self) {
        self.stages.iter_mut().for_each(Biquad::reset);
        self.log_freq.snap(self.log_freq.target());
        self.update_coeffs();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.log_freq.set_sample_rate(sample_rate);
        self.update_coeffs();
    }
}

/// Bass (low shelf), Mid (peak), Tiz (high shelf).
pub struct Tone {
    sample_rate: f32,
    bands: [ToneBand; 3],
}

struct ToneBand {
    kind: FilterType,
    freq: f32,
    q: f32,
    gain_db: Smoother,
    filter: Biquad,
    /// 0 dB'de atlanan bant, tekrar devreye girerken durumunu sıfırlar.
    idle: bool,
}

pub const BASS_FREQ: f32 = 120.0;
pub const MID_FREQ: f32 = 1000.0;
pub const TREBLE_FREQ: f32 = 6000.0;

impl Tone {
    pub fn new(sample_rate: f32) -> Self {
        let band = |kind, freq, q| ToneBand {
            kind,
            freq,
            q,
            gain_db: Smoother::new(0.0, sample_rate),
            filter: Biquad::default(),
            idle: true,
        };
        Self {
            sample_rate,
            bands: [
                band(
                    FilterType::LowShelf,
                    BASS_FREQ,
                    std::f32::consts::FRAC_1_SQRT_2,
                ),
                band(FilterType::Peak, MID_FREQ, 0.7),
                band(
                    FilterType::HighShelf,
                    TREBLE_FREQ,
                    std::f32::consts::FRAC_1_SQRT_2,
                ),
            ],
        }
    }

    pub fn set(&mut self, bass_db: f32, mid_db: f32, treble_db: f32) {
        for (b, g) in self.bands.iter_mut().zip([bass_db, mid_db, treble_db]) {
            b.gain_db.set_target(g);
        }
    }

    /// Toplam genlik tepkisi (dB), hedef kazançlarla.
    pub fn magnitude_db(&self, freq: f32) -> f32 {
        self.bands
            .iter()
            .map(|b| {
                Coeffs::new(b.kind, self.sample_rate, b.freq, b.q, b.gain_db.target())
                    .magnitude_db(freq, self.sample_rate)
            })
            .sum()
    }
}

impl ToneBand {
    fn update(&mut self, sample_rate: f32) {
        self.filter.set_coeffs(Coeffs::new(
            self.kind,
            sample_rate,
            self.freq,
            self.q,
            self.gain_db.value(),
        ));
    }
}

impl Processor for Tone {
    fn process(&mut self, buf: &mut [f32]) {
        let fs = self.sample_rate;
        for b in &mut self.bands {
            // 0 dB'de ve hareketsizken bant birim filtredir: CPU harcama.
            if !b.gain_db.is_active() && b.gain_db.value() == 0.0 {
                b.idle = true;
                continue;
            }
            if b.idle {
                b.filter.reset();
                b.update(fs);
                b.idle = false;
            }
            for block in buf.chunks_mut(COEF_BLOCK) {
                if b.gain_db.is_active() {
                    b.gain_db.advance(block.len());
                    b.update(fs);
                }
                b.filter.process(block);
            }
        }
    }

    fn reset(&mut self) {
        let fs = self.sample_rate;
        for b in &mut self.bands {
            b.filter.reset();
            b.gain_db.snap(b.gain_db.target());
            b.update(fs);
        }
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        for b in &mut self.bands {
            b.gain_db.set_sample_rate(sample_rate);
            b.update(sample_rate);
        }
    }
}

/// Çıkış kazancı ve sağ-sol denge. Mono girişten stereo üretir.
pub struct Output {
    gain: Smoother,
    left: Smoother,
    right: Smoother,
}

impl Output {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            gain: Smoother::new(1.0, sample_rate),
            left: Smoother::new(1.0, sample_rate),
            right: Smoother::new(1.0, sample_rate),
        }
    }

    /// `pan`: -100 (sol) .. +100 (sağ). Denge yasası: ortada iki kanal da 0 dB.
    pub fn set(&mut self, gain_db: f32, pan: f32) {
        let p = (pan / 100.0).clamp(-1.0, 1.0);
        self.gain.set_target(db_to_lin(gain_db));
        self.left.set_target((1.0 - p).min(1.0));
        self.right.set_target((1.0 + p).min(1.0));
    }

    /// Çıkış kazancını mono tampona uygular (limiter'dan önce çağrılır).
    pub fn apply_gain(&mut self, buf: &mut [f32]) {
        if !self.gain.is_active() {
            let g = self.gain.value();
            if g != 1.0 {
                buf.iter_mut().for_each(|s| *s *= g);
            }
            return;
        }
        for s in buf {
            *s *= self.gain.next_value();
        }
    }

    /// Bir mono örneği dengeyle (sol, sağ) çiftine çevirir. Kanal kazançları
    /// ≤ 1 olduğundan limiter tavanı aşılmaz.
    #[inline]
    pub fn tick(&mut self, x: f32) -> (f32, f32) {
        (x * self.left.next_value(), x * self.right.next_value())
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.gain.set_sample_rate(sample_rate);
        self.left.set_sample_rate(sample_rate);
        self.right.set_sample_rate(sample_rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FS: f32 = 48_000.0;

    fn sine(freq: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / FS).sin())
            .collect()
    }

    fn settle_peak_db(p: &mut impl Processor, freq: f32) -> f32 {
        let mut buf = sine(freq, 48_000);
        for chunk in buf.chunks_mut(256) {
            p.process(chunk);
        }
        let peak = buf[24_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        crate::lin_to_db(peak)
    }

    #[test]
    fn input_gain_and_invert() {
        let mut s = InputStage::new(FS);
        s.set(6.0, true);
        s.reset();
        let mut buf = [0.5f32; 8];
        s.process(&mut buf);
        assert!((buf[0] + 0.5 * db_to_lin(6.0)).abs() < 1e-5);
    }

    #[test]
    fn hpf_slopes() {
        for (slope, expect_half) in [(0usize, -12.3f32), (1, -24.1), (2, -48.2)] {
            let h = {
                let mut h = HighPass::new(FS, 100.0);
                h.set(100.0, slope);
                h
            };
            assert!((h.magnitude_db(100.0) + 3.01).abs() < 0.1, "slope {slope}");
            assert!(
                (h.magnitude_db(50.0) - expect_half).abs() < 0.5,
                "slope {slope}"
            );
            assert!(h.magnitude_db(5000.0).abs() < 0.01);
        }
    }

    #[test]
    fn hpf_slope_change_resets_all_stages() {
        // Eğim 12 -> 24 dB/okt değişince filtre, aynı eğimle yeni kurulmuş
        // bir filtreyle aynı çıktıyı vermeli (ilk kat durumu taşınmamalı).
        let mut a = HighPass::new(FS, 100.0);
        a.set(100.0, 0);
        let mut warm = sine(60.0, 4800);
        a.process(&mut warm);
        a.set(100.0, 1);
        let mut b = HighPass::new(FS, 100.0);
        b.set(100.0, 1);
        let mut x = sine(300.0, 1024);
        let mut y = x.clone();
        a.process(&mut x);
        b.process(&mut y);
        assert_eq!(x, y);
    }

    #[test]
    fn hpf_processes_signal() {
        let mut h = HighPass::new(FS, 200.0);
        h.set(200.0, 1);
        assert!(settle_peak_db(&mut h, 50.0) < -45.0);
        let mut h = HighPass::new(FS, 200.0);
        assert!(settle_peak_db(&mut h, 2000.0).abs() < 0.1);
    }

    #[test]
    fn tone_bands() {
        let mut t = Tone::new(FS);
        t.set(6.0, 0.0, 0.0);
        t.reset();
        assert!((settle_peak_db(&mut t, 30.0) - 6.0).abs() < 0.3);
        let mut t = Tone::new(FS);
        t.set(0.0, -6.0, 0.0);
        t.reset();
        assert!((settle_peak_db(&mut t, MID_FREQ) + 6.0).abs() < 0.1);
        assert!((t.magnitude_db(MID_FREQ) + 6.0).abs() < 0.01);
    }

    #[test]
    fn tone_flat_is_passthrough() {
        let mut t = Tone::new(FS);
        let mut buf = sine(440.0, 512);
        let orig = buf.clone();
        t.process(&mut buf);
        assert_eq!(buf, orig);
    }

    #[test]
    fn output_balance() {
        let mut o = Output::new(FS);
        o.set(0.0, -100.0);
        let mut last = (0.0, 0.0);
        for _ in 0..48_000 {
            last = o.tick(1.0);
        }
        assert!((last.0 - 1.0).abs() < 1e-4 && last.1.abs() < 1e-4);
    }

    #[test]
    fn parameter_sweep_has_no_jumps() {
        // Kazanç aniden +24 dB'ye çekilse de ardışık örnek farkı küçük kalmalı.
        let mut s = InputStage::new(FS);
        let mut buf = vec![0.1f32; 4800];
        s.set(24.0, false);
        s.process(&mut buf);
        let max_jump = buf
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(max_jump < 0.01, "zipper: {max_jump}");
    }
}
