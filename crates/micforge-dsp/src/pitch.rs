//! Zaman alanında perde kaydırıcı ve ses değiştirici.
//!
//! [`PitchShifter`]: "dönen bant kafası" (Doppler) yöntemi. Gecikme hattında
//! iki okuma noktası 40 ms'lik pencerede, faz (`p`) ile ters yönde kayar ve
//! sin² tabanlı kazançlarla çaprazlanır. Oran `r` için gecikme `τ = p·W` her
//! örnekte `(1 - r)` kadar değişir, böylece çıkış frekansı `r·f` olur.
//!
//! [`VoiceChanger`]: Robot (halka modülasyonu + metalik tarak filtresi),
//! Deep (perde düşürme + alçak raf), Radio (bant geçiren + yumuşak kırpma).
//!
//! Gerçek zamanlı kurallar: `process` ve `reset` heap ayırmaz, kilit almaz,
//! log yazmaz ve panic üretmez. Tüm tamponlar `new` içinde ayrılır.

use std::f32::consts::{PI, TAU};

use crate::Processor;
use crate::biquad::{Biquad, Coeffs, FilterType, butterworth_qs};
use crate::smooth::Smoother;

/// Gecikme hattı uzunluğu (2'nin kuvveti). 96 kHz'de 40 ms = 3840 örnek.
const LINE_LEN: usize = 4096;
const LINE_MASK: usize = LINE_LEN - 1;
/// Kaydırma penceresi (saniye).
const WINDOW_S: f32 = 0.040;
/// Robot modunda tarak filtresinin gecikmesi (saniye).
const COMB_DELAY_S: f32 = 0.006;
/// Tarak filtresi tamponu (2'nin kuvveti, 96 kHz'de 6 ms = 576 örnek).
const COMB_LEN: usize = 1024;
const COMB_MASK: usize = COMB_LEN - 1;
/// Tarak ve kayan nokta geri beslemelerinde bu eşiğin altı sıfıra çekilir.
const FLUSH_EPS: f32 = 1e-20;

/// Yüzdeyi `[0, 100]` aralığına sıkıştırır; sonlu olmayan değer 0 olur.
fn pct(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Geri beslemeli yollarda çok küçük değerleri sıfırlar (denormal önlemi).
#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < FLUSH_EPS { 0.0 } else { x }
}

/// Doppler yöntemiyle zaman alanında perde kaydırıcı (mono, `f32`).
///
/// Çıkış gecikmesi `latency_samples()` = W/2 örnektir. `mix` kuru sinyale
/// (gecikmesiz) karşı ıslak çıkış oranıdır; ara değerlerde iki yol arasında
/// tarak filtre etkisi oluşabilir.
pub struct PitchShifter {
    sample_rate: f32,
    line: Vec<f32>,
    /// En yeni yazılan örneğin indeksi.
    wp: usize,
    /// Pencere uzunluğu (örnek).
    wn: f32,
    /// Sawtooth faz, `[0, 1)`.
    phase: f32,
    /// Güncel perde oranı `2^(semitones/12 + cents/1200)`.
    ratio: f32,
    mix: Smoother,
    /// Oran tam 1 iken 1'e giden yumuşatıcı: tek okuma (sabit gecikme) yolu.
    bypass: Smoother,
}

impl PitchShifter {
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            sample_rate,
            line: vec![0.0; LINE_LEN],
            wp: 0,
            wn: 1.0,
            phase: 0.0,
            ratio: 1.0,
            mix: Smoother::new(1.0, sample_rate),
            bypass: Smoother::new(1.0, sample_rate),
        };
        s.set_sample_rate(sample_rate);
        s
    }

    /// Perde ayarı: `semitones` −12..12, `cents` −100..100, `mix_pct` 0..100
    /// (kuru/ıslak). Oran hemen uygulanır; karışım yumuşatılır.
    pub fn set(&mut self, semitones: f32, cents: f32, mix_pct: f32) {
        let st = if semitones.is_finite() {
            semitones.clamp(-12.0, 12.0)
        } else {
            0.0
        };
        let ct = if cents.is_finite() {
            cents.clamp(-100.0, 100.0)
        } else {
            0.0
        };
        self.mix.set_target(pct(mix_pct) / 100.0);
        self.set_ratio(2f32.powf(st / 12.0 + ct / 1200.0));
    }

    /// Gecikme (örnek) = pencerenin yarısı.
    pub fn latency_samples(&self) -> usize {
        (self.wn * 0.5).round() as usize
    }

    /// Doğrudan oran ayarı (iç kullanım: `VoiceChanger` her örnek ayarlayabilir).
    fn set_ratio(&mut self, ratio: f32) {
        self.ratio = ratio;
        self.bypass.set_target(if ratio == 1.0 { 1.0 } else { 0.0 });
    }

    /// `d` örnek geride okuma (4 noktalı Catmull-Rom). `d` ∈ [0, wn].
    #[inline]
    fn read(&self, d: f32) -> f32 {
        let di = d.floor();
        let frac = d - di;
        let u = di as usize;
        // Konum O = n-u-1 etrafında: p0 = O, p1 = O+1, pm1 = O-1, p2 = O+2.
        // Hedef, O'dan (1 - frac) uzaklıkta.
        let p0 = self.tap(u + 1);
        let p1 = self.tap(u);
        let pm1 = self.tap(u + 2);
        // u = 0 iken gelecek örnek okunur; bu okumanın kazancı ~0'dır.
        let p2 = self.tap(u.wrapping_sub(1));
        let t = 1.0 - frac;
        0.5 * (2.0 * p0
            + (p1 - pm1) * t
            + (2.0 * pm1 - 5.0 * p0 + 4.0 * p1 - p2) * t * t
            + (-pm1 + 3.0 * p0 - 3.0 * p1 + p2) * t * t * t)
    }

    /// `k` örnek geriden tam indeksli örnek.
    #[inline]
    fn tap(&self, k: usize) -> f32 {
        self.line[self.wp.wrapping_sub(k) & LINE_MASK]
    }
}

impl Processor for PitchShifter {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let dry = *s;
            self.wp = (self.wp + 1) & LINE_MASK;
            self.line[self.wp] = dry;

            // Faz: τ = p·W, dτ/dn = (1 - r) ⇒ dp/dn = (1 - r) / W.
            let mut p = self.phase + (1.0 - self.ratio) / self.wn;
            if p >= 1.0 {
                p -= 1.0;
            } else if p < 0.0 {
                p += 1.0;
            }
            if !(0.0..1.0).contains(&p) {
                p = 0.0;
            }
            self.phase = p;

            // İkinci okuma noktası yarım pencere ötede; kazançlar sin(π·konum)
            // olduğundan gA² + gB² = 1.
            let mut pb = p + 0.5;
            if pb >= 1.0 {
                pb -= 1.0;
            }
            let shifted =
                self.read(p * self.wn) * (PI * p).sin() + self.read(pb * self.wn) * (PI * pb).sin();

            let b = self.bypass.next_value();
            let wet = if b == 0.0 {
                shifted
            } else {
                let single = self.read(self.wn * 0.5);
                b * single + (1.0 - b) * shifted
            };

            let m = self.mix.next_value();
            *s = dry * (1.0 - m) + wet * m;
        }
    }

    fn reset(&mut self) {
        self.line.fill(0.0);
        self.wp = 0;
        self.phase = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.wn = (sample_rate * WINDOW_S).clamp(8.0, (LINE_LEN - 8) as f32);
        self.mix.set_sample_rate(sample_rate);
        self.bypass.set_sample_rate(sample_rate);
    }
}

/// Ses değiştirici modu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceMode {
    Robot,
    Deep,
    Radio,
}

impl VoiceMode {
    /// Arayüz indeksinden mod (0 Robot, 1 Deep, 2 Radio). Yuvarlanır;
    /// aralık dışı değerler Robot olur.
    pub fn from_index(i: f32) -> VoiceMode {
        let r = i.round();
        if r == 1.0 {
            VoiceMode::Deep
        } else if r == 2.0 {
            VoiceMode::Radio
        } else {
            VoiceMode::Robot
        }
    }
}

/// Üç modlu ses değiştirici (mono, `f32`).
pub struct VoiceChanger {
    sample_rate: f32,
    mode: VoiceMode,
    intensity: Smoother,
    mix: Smoother,
    /// Robot: taşıyıcı fazı `[0, 1)`.
    carrier_phase: f32,
    /// Robot: tarak filtresi tamponu.
    comb: Vec<f32>,
    comb_idx: usize,
    comb_delay: usize,
    /// Deep: perde kaydırıcı (her örnekte tek örnek olarak çağrılır).
    pitch: PitchShifter,
    /// Deep: alçak raf (+4 dB, 150 Hz).
    low_shelf: Biquad,
    /// Radio: 24 dB/okt yüksek geçiren (iki kat).
    hp: [Biquad; 2],
    /// Radio: 24 dB/okt alçak geçiren (iki kat).
    lp: [Biquad; 2],
    /// Radio: 1500 Hz tepe.
    peak: Biquad,
}

impl VoiceChanger {
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            sample_rate,
            mode: VoiceMode::Robot,
            intensity: Smoother::new(0.5, sample_rate),
            mix: Smoother::new(1.0, sample_rate),
            carrier_phase: 0.0,
            comb: vec![0.0; COMB_LEN],
            comb_idx: 0,
            comb_delay: 1,
            pitch: PitchShifter::new(sample_rate),
            low_shelf: Biquad::default(),
            hp: Default::default(),
            lp: Default::default(),
            peak: Biquad::default(),
        };
        s.set_sample_rate(sample_rate);
        s
    }

    /// Mod, yoğunluk (0..100) ve karışım (0..100, kuru/ıslak). Yoğunluk ve
    /// karışım yumuşatılır. Mod değişince iç durumlar sıfırlanır.
    pub fn set(&mut self, mode: VoiceMode, intensity_pct: f32, mix_pct: f32) {
        self.intensity.set_target(pct(intensity_pct) / 100.0);
        self.mix.set_target(pct(mix_pct) / 100.0);
        if mode != self.mode {
            self.mode = mode;
            self.clear_state();
        }
    }

    /// Gecikme (örnek): Deep modunda perde kaydırıcının gecikmesi, diğerleri 0.
    pub fn latency_samples(&self) -> usize {
        match self.mode {
            VoiceMode::Deep => self.pitch.latency_samples(),
            _ => 0,
        }
    }

    fn clear_state(&mut self) {
        self.carrier_phase = 0.0;
        self.comb.fill(0.0);
        self.comb_idx = 0;
        self.pitch.reset();
        self.low_shelf.reset();
        for b in self.hp.iter_mut().chain(self.lp.iter_mut()) {
            b.reset();
        }
        self.peak.reset();
    }

    #[inline]
    fn robot(&mut self, x: f32, inten: f32) -> f32 {
        // Halka modülasyonu: 30..150 Hz sinüs taşıyıcı.
        let freq = 30.0 + 120.0 * inten;
        self.carrier_phase += freq / self.sample_rate;
        if self.carrier_phase >= 1.0 {
            self.carrier_phase -= 1.0;
        }
        let ring = x * (TAU * self.carrier_phase).sin();

        // Geri beslemeli tarak: y[n] = in + g·y[n-d].
        let g = 0.3 + 0.5 * inten;
        let rd = (self.comb_idx + COMB_LEN - self.comb_delay) & COMB_MASK;
        let y = flush(ring + g * self.comb[rd]);
        self.comb[self.comb_idx] = y;
        self.comb_idx = (self.comb_idx + 1) & COMB_MASK;
        1.2 * y
    }

    #[inline]
    fn deep(&mut self, x: f32, inten: f32) -> f32 {
        let st = -(3.0 + 4.0 * inten);
        self.pitch.set_ratio(2f32.powf(st / 12.0));
        let mut one = [x];
        self.pitch.process(&mut one);
        self.low_shelf.tick(one[0])
    }

    #[inline]
    fn radio(&mut self, x: f32, inten: f32) -> f32 {
        let mut y = x;
        for b in self.hp.iter_mut().chain(self.lp.iter_mut()) {
            y = b.tick(y);
        }
        y = self.peak.tick(y);
        let drive = 1.0 + 9.0 * inten;
        // Küçük sinyalde birim kazanç (seviye sıçramaz); büyükte yumuşak kırpma.
        (drive * y).tanh() / drive
    }
}

impl Processor for VoiceChanger {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let dry = *s;
            let inten = self.intensity.next_value();
            let m = self.mix.next_value();
            let wet = match self.mode {
                VoiceMode::Robot => self.robot(dry, inten),
                VoiceMode::Deep => self.deep(dry, inten),
                VoiceMode::Radio => self.radio(dry, inten),
            };
            *s = dry * (1.0 - m) + wet * m;
        }
    }

    fn reset(&mut self) {
        self.clear_state();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.intensity.set_sample_rate(sample_rate);
        self.mix.set_sample_rate(sample_rate);
        self.comb_delay = ((COMB_DELAY_S * sample_rate).round() as usize).clamp(1, COMB_MASK);
        self.pitch.set_sample_rate(sample_rate);
        self.low_shelf.set_coeffs(Coeffs::new(
            FilterType::LowShelf,
            sample_rate,
            150.0,
            std::f32::consts::FRAC_1_SQRT_2,
            4.0,
        ));
        for (i, q) in butterworth_qs(4).enumerate() {
            let hp = Coeffs::new(FilterType::HighPass, sample_rate, 400.0, q, 0.0);
            let lp = Coeffs::new(FilterType::LowPass, sample_rate, 3000.0, q, 0.0);
            self.hp[i].set_coeffs(hp);
            self.lp[i].set_coeffs(lp);
        }
        self.peak
            .set_coeffs(Coeffs::new(FilterType::Peak, sample_rate, 1500.0, 1.0, 6.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (TAU * freq * i as f32 / SR).sin())
            .collect()
    }

    /// Son `tail` örnekte pozitif yönlü sıfır geçişlerinden frekans.
    fn est_freq(x: &[f32], tail: usize) -> f32 {
        let seg = &x[x.len() - tail..];
        let crossings = seg.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        crossings as f32 / (tail as f32 / SR)
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn run_shift(st: f32, ct: f32, freq: f32) -> f32 {
        let mut ps = PitchShifter::new(SR);
        ps.set(st, ct, 100.0);
        let mut x = sine(freq, 0.5, 48_000);
        ps.process(&mut x);
        est_freq(&x, 24_000)
    }

    fn assert_near(actual: f32, expected: f32, tol_frac: f32) {
        let err = (actual - expected).abs() / expected;
        assert!(
            err <= tol_frac,
            "{actual} Hz ≉ {expected} Hz (tolerans %{})",
            tol_frac * 100.0
        );
    }

    #[test]
    fn shift_up_octave() {
        assert_near(run_shift(12.0, 0.0, 440.0), 880.0, 0.03);
    }

    #[test]
    fn shift_down_octave() {
        assert_near(run_shift(-12.0, 0.0, 440.0), 220.0, 0.03);
    }

    #[test]
    fn shift_fifth() {
        assert_near(run_shift(7.0, 0.0, 440.0), 659.0, 0.03);
    }

    #[test]
    fn shift_cents() {
        assert_near(run_shift(0.0, 100.0, 440.0), 466.0, 0.03);
    }

    #[test]
    fn unity_shift_is_transparent_with_latency() {
        let mut ps = PitchShifter::new(SR);
        ps.set(0.0, 0.0, 100.0);
        assert_eq!(ps.latency_samples(), 960);
        let input = sine(440.0, 0.5, 48_000);
        let mut out = input.clone();
        ps.process(&mut out);
        let tail = 24_000..48_000;
        let r_in = rms(&input[tail.clone()]);
        let r_out = rms(&out[tail]);
        let db = 20.0 * (r_out / r_in).log10();
        assert!(db.abs() <= 1.5, "RMS farkı {db} dB");
    }

    #[test]
    fn mix_zero_passes_dry_exactly() {
        let mut ps = PitchShifter::new(SR);
        ps.set(7.0, 0.0, 0.0);
        let input = sine(440.0, 0.5, 48_000);
        let mut out = input.clone();
        ps.process(&mut out);
        // Karışım yumuşatıcısı ~0.3 s'de yerleşir.
        for (a, b) in input.iter().zip(out.iter()).skip(24_000) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    #[test]
    fn voice_modes_are_finite_and_level_sane() {
        for mode in [VoiceMode::Robot, VoiceMode::Deep, VoiceMode::Radio] {
            let mut vc = VoiceChanger::new(SR);
            vc.set(mode, 50.0, 100.0);
            let input = sine(440.0, 0.5, 48_000);
            let mut out = input.clone();
            vc.process(&mut out);
            assert!(out.iter().all(|v| v.is_finite()), "{mode:?} sonlu değil");
            let db = 20.0 * (rms(&out[24_000..]) / rms(&input[24_000..])).log10();
            assert!((-20.0..=6.0).contains(&db), "{mode:?}: {db} dB");
            let diff: f32 = input
                .iter()
                .zip(out.iter())
                .map(|(a, b)| (a - b).abs())
                .sum();
            assert!(diff > 1.0, "{mode:?} girişten ayırt edilemiyor");
        }
    }

    #[test]
    fn radio_cuts_bass() {
        let level = |freq: f32| {
            let mut vc = VoiceChanger::new(SR);
            vc.set(VoiceMode::Radio, 50.0, 100.0);
            let mut x = sine(freq, 0.5, 48_000);
            vc.process(&mut x);
            rms(&x[24_000..])
        };
        let low = level(100.0);
        let mid = level(1000.0);
        let db = 20.0 * (low / mid).log10();
        assert!(db < -15.0, "100 Hz 1 kHz'e göre {db} dB");
    }

    #[test]
    fn deep_lowers_pitch() {
        let mut vc = VoiceChanger::new(SR);
        vc.set(VoiceMode::Deep, 50.0, 100.0);
        assert_eq!(vc.latency_samples(), 960);
        let mut x = sine(300.0, 0.5, 48_000);
        vc.process(&mut x);
        let f = est_freq(&x, 24_000);
        assert!(f < 250.0 && f > 180.0, "tahmini perde {f} Hz");
    }

    #[test]
    fn extreme_params_and_switching_stay_bounded() {
        let mut vc = VoiceChanger::new(SR);
        let mut ps = PitchShifter::new(SR);
        // Basit deterministik ±1 gürültü.
        let mut seed: u32 = 12345;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if seed & 0x8000_0000 != 0 { 1.0 } else { -1.0 }
        };
        let mut buf = vec![0.0f32; 480];
        for block in 0..200 {
            let mode = VoiceMode::from_index((block % 4) as f32);
            let inten = [0.0, 100.0, 37.0, 250.0][block % 4];
            let mix = [100.0, 0.0, 50.0, -20.0][block % 3];
            vc.set(mode, inten, mix);
            ps.set(if block % 2 == 0 { 12.0 } else { -12.0 }, 100.0, mix);
            for s in buf.iter_mut() {
                *s = noise();
            }
            vc.process(&mut buf);
            assert!(buf.iter().all(|v| v.is_finite() && v.abs() < 10.0));
            ps.process(&mut buf);
            assert!(buf.iter().all(|v| v.is_finite() && v.abs() < 10.0));
        }
    }

    #[test]
    fn odd_buffer_lengths_work() {
        for n in [1usize, 256, 1000] {
            let mut ps = PitchShifter::new(SR);
            ps.set(5.0, 20.0, 100.0);
            let mut vc = VoiceChanger::new(SR);
            vc.set(VoiceMode::Robot, 80.0, 100.0);
            let mut b = vec![0.3f32; n];
            ps.process(&mut b);
            vc.process(&mut b);
            assert!(b.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn reset_clears_state() {
        let mut vc = VoiceChanger::new(SR);
        vc.set(VoiceMode::Deep, 50.0, 100.0);
        let mut x = sine(300.0, 0.5, 4_800);
        vc.process(&mut x);
        vc.reset();
        let mut silence = vec![0.0f32; 4_800];
        vc.process(&mut silence);
        assert!(silence.iter().all(|v| *v == 0.0));

        let mut ps = PitchShifter::new(SR);
        ps.set(7.0, 0.0, 100.0);
        let mut x = sine(440.0, 0.5, 4_800);
        ps.process(&mut x);
        ps.reset();
        let mut silence = vec![0.0f32; 4_800];
        ps.process(&mut silence);
        assert!(silence.iter().all(|v| *v == 0.0));
    }

    #[test]
    fn from_index_maps_modes() {
        assert_eq!(VoiceMode::from_index(0.0), VoiceMode::Robot);
        assert_eq!(VoiceMode::from_index(0.6), VoiceMode::Deep);
        assert_eq!(VoiceMode::from_index(2.0), VoiceMode::Radio);
        assert_eq!(VoiceMode::from_index(9.0), VoiceMode::Robot);
        assert_eq!(VoiceMode::from_index(-3.0), VoiceMode::Robot);
    }
}
