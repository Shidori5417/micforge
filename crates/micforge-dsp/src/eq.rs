//! 16 bantlı parametrik EQ ve Bass Boost. RBJ biquad'ları, parametre
//! yumuşatma ile; `process` gerçek zamanlı güvenlidir.

use crate::Processor;
use crate::biquad::{Biquad, Coeffs, FilterType, butterworth_qs};
use crate::params::EQ_BANDS;
use crate::smooth::Smoother;

/// Yumuşatma sırasında katsayıların yeniden hesaplanma aralığı (örnek).
const COEF_BLOCK: usize = 32;
/// Bir bantta en fazla 2. derece kat sayısı (48 dB/okt).
pub const MAX_STAGES: usize = 4;

/// `params::EQ_KIND_CHOICES` sırasıyla.
pub fn kind_from_index(i: f32) -> FilterType {
    match i.round() as i32 {
        1 => FilterType::LowShelf,
        2 => FilterType::HighShelf,
        3 => FilterType::LowPass,
        4 => FilterType::HighPass,
        5 => FilterType::Notch,
        6 => FilterType::BandPass,
        _ => FilterType::Peak,
    }
}

/// Kazanç parametresi kullanan tipler (0 dB'de birim filtredir).
fn uses_gain(kind: FilterType) -> bool {
    matches!(
        kind,
        FilterType::Peak | FilterType::LowShelf | FilterType::HighShelf
    )
}

/// Bir bandın 2. derece katlarını hesaplar; kat sayısını döndürür.
///
/// Low-pass/High-pass: 12 dB/okt'ta tek kat ve kullanıcının Q'su (rezonans);
/// 24/48 dB/okt'ta Butterworth kaskadı (Q yok sayılır). Diğer tipler tek kat.
pub fn band_coeffs(
    kind: FilterType,
    sample_rate: f32,
    freq: f32,
    q: f32,
    gain_db: f32,
    slope_index: usize,
    out: &mut [Coeffs; MAX_STAGES],
) -> usize {
    let pass = matches!(kind, FilterType::LowPass | FilterType::HighPass);
    if !pass || slope_index == 0 {
        out[0] = Coeffs::new(kind, sample_rate, freq, q, gain_db);
        return 1;
    }
    let order = if slope_index == 1 { 4 } else { 8 };
    for (c, bq) in out.iter_mut().zip(butterworth_qs(order)) {
        *c = Coeffs::new(kind, sample_rate, freq, bq, 0.0);
    }
    order / 2
}

/// Bir bandın hedef (yumuşatılmamış) toplam genlik tepkisi (dB). UI eğrisi için.
pub fn band_response_db(
    kind: FilterType,
    sample_rate: f32,
    freq: f32,
    q: f32,
    gain_db: f32,
    slope_index: usize,
    at: f32,
) -> f32 {
    let mut c = [Coeffs::IDENTITY; MAX_STAGES];
    let n = band_coeffs(kind, sample_rate, freq, q, gain_db, slope_index, &mut c);
    c[..n].iter().map(|c| c.magnitude_db(at, sample_rate)).sum()
}

/// Tek bir yumuşatılmış EQ bandı.
pub struct Band {
    sample_rate: f32,
    kind: FilterType,
    slope: usize,
    log_freq: Smoother,
    gain: Smoother,
    log_q: Smoother,
    stages: [Biquad; MAX_STAGES],
    n: usize,
    /// Atlanmış (kapalı/birim) bant; tekrar devreye girerken sıfırlanır.
    idle: bool,
}

impl Band {
    pub fn new(sample_rate: f32, kind: FilterType, freq: f32, q: f32, gain_db: f32) -> Self {
        let mut b = Self {
            sample_rate,
            kind,
            slope: 0,
            log_freq: Smoother::new(freq.ln(), sample_rate),
            gain: Smoother::new(gain_db, sample_rate),
            log_q: Smoother::new(q.ln(), sample_rate),
            stages: Default::default(),
            n: 1,
            idle: true,
        };
        b.update();
        b
    }

    /// Hedef değerleri ayarlar. Tip veya eğim değişirse filtre durumu sıfırlanır.
    pub fn set(&mut self, kind: FilterType, freq: f32, q: f32, gain_db: f32, slope: usize) {
        self.log_freq.set_target(freq.ln());
        self.log_q.set_target(q.ln());
        self.gain.set_target(gain_db);
        if kind != self.kind || slope != self.slope {
            self.kind = kind;
            self.slope = slope;
            self.stages.iter_mut().for_each(Biquad::reset);
            self.update();
        }
    }

    fn smoothing(&self) -> bool {
        self.log_freq.is_active() || self.gain.is_active() || self.log_q.is_active()
    }

    /// Kazancı 0 dB olan peak/shelf bandı birim filtredir; işlenmesine gerek yok.
    fn is_identity(&self) -> bool {
        uses_gain(self.kind) && !self.smoothing() && self.gain.value() == 0.0
    }

    fn update(&mut self) {
        let mut c = [Coeffs::IDENTITY; MAX_STAGES];
        self.n = band_coeffs(
            self.kind,
            self.sample_rate,
            self.log_freq.value().exp(),
            self.log_q.value().exp(),
            self.gain.value(),
            self.slope,
            &mut c,
        );
        for (s, c) in self.stages.iter_mut().zip(c) {
            s.set_coeffs(c);
        }
    }

    /// Bandı işler; `active` false ise veya bant birim ise atlar (CPU harcamaz).
    pub fn run(&mut self, buf: &mut [f32], active: bool) {
        if !active || self.is_identity() {
            self.idle = true;
            return;
        }
        if self.idle {
            self.idle = false;
            self.stages.iter_mut().for_each(Biquad::reset);
            self.update();
        }
        for block in buf.chunks_mut(COEF_BLOCK) {
            if self.smoothing() {
                let n = block.len();
                self.log_freq.advance(n);
                self.gain.advance(n);
                self.log_q.advance(n);
                self.update();
            }
            for s in &mut self.stages[..self.n] {
                s.process(block);
            }
        }
    }

    fn reset(&mut self) {
        self.stages.iter_mut().for_each(Biquad::reset);
        self.log_freq.snap(self.log_freq.target());
        self.gain.snap(self.gain.target());
        self.log_q.snap(self.log_q.target());
        self.update();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.log_freq.set_sample_rate(sample_rate);
        self.gain.set_sample_rate(sample_rate);
        self.log_q.set_sample_rate(sample_rate);
        self.update();
    }
}

/// 16 bantlı parametrik EQ.
pub struct ParametricEq {
    bands: [Band; EQ_BANDS],
    on: [bool; EQ_BANDS],
}

impl ParametricEq {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            bands: std::array::from_fn(|i| {
                Band::new(
                    sample_rate,
                    FilterType::Peak,
                    crate::params::eq_default_freq(i),
                    1.0,
                    0.0,
                )
            }),
            on: [true; EQ_BANDS],
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_band(
        &mut self,
        i: usize,
        on: bool,
        kind: FilterType,
        freq: f32,
        q: f32,
        gain_db: f32,
        slope: usize,
    ) {
        self.on[i] = on;
        self.bands[i].set(kind, freq, q, gain_db, slope);
    }
}

impl Processor for ParametricEq {
    fn process(&mut self, buf: &mut [f32]) {
        for (b, &on) in self.bands.iter_mut().zip(&self.on) {
            b.run(buf, on);
        }
    }

    fn reset(&mut self) {
        self.bands.iter_mut().for_each(Band::reset);
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.bands
            .iter_mut()
            .for_each(|b| b.set_sample_rate(sample_rate));
    }
}

/// Bass Boost: ayarlanabilir frekansta low shelf (0..+18 dB).
pub struct BassBoost {
    band: Band,
}

/// Bass Boost shelf eğimi (Q).
pub const BASS_BOOST_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

impl BassBoost {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            band: Band::new(sample_rate, FilterType::LowShelf, 80.0, BASS_BOOST_Q, 0.0),
        }
    }

    pub fn set(&mut self, freq: f32, amount_db: f32) {
        self.band
            .set(FilterType::LowShelf, freq, BASS_BOOST_Q, amount_db, 0);
    }
}

impl Processor for BassBoost {
    fn process(&mut self, buf: &mut [f32]) {
        self.band.run(buf, true);
    }

    fn reset(&mut self) {
        self.band.reset();
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.band.set_sample_rate(sample_rate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FS: f32 = 48_000.0;

    fn tone_gain_db(p: &mut impl Processor, freq: f32) -> f32 {
        let mut buf: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / FS).sin())
            .collect();
        for c in buf.chunks_mut(256) {
            p.process(c);
        }
        let peak = buf[24_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        crate::lin_to_db(peak)
    }

    #[test]
    fn default_eq_is_transparent_and_idle() {
        let mut eq = ParametricEq::new(FS);
        let mut buf: Vec<f32> = (0..1024).map(|i| (i as f32 * 0.01).sin()).collect();
        let orig = buf.clone();
        eq.process(&mut buf);
        assert_eq!(buf, orig);
    }

    #[test]
    fn band_peak_and_types() {
        let mut eq = ParametricEq::new(FS);
        eq.set_band(5, true, FilterType::Peak, 2000.0, 2.0, 9.0, 0);
        eq.reset();
        assert!((tone_gain_db(&mut eq, 2000.0) - 9.0).abs() < 0.2);

        let mut eq = ParametricEq::new(FS);
        eq.set_band(0, true, FilterType::Notch, 1000.0, 4.0, 0.0, 0);
        eq.reset();
        assert!(tone_gain_db(&mut eq, 1000.0) < -30.0);

        // Kapalı bant hiçbir şey yapmaz.
        let mut eq = ParametricEq::new(FS);
        eq.set_band(0, false, FilterType::Notch, 1000.0, 4.0, 0.0, 0);
        eq.reset();
        assert!(tone_gain_db(&mut eq, 1000.0).abs() < 0.01);
    }

    #[test]
    fn lowpass_slopes() {
        for (slope, at_2x) in [(0usize, -12.3f32), (1, -24.1), (2, -48.2)] {
            let r = band_response_db(
                FilterType::LowPass,
                FS,
                1000.0,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                slope,
                2000.0,
            );
            assert!((r - at_2x).abs() < 0.6, "eğim {slope}: {r}");
            let r0 = band_response_db(
                FilterType::LowPass,
                FS,
                1000.0,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                slope,
                1000.0,
            );
            assert!((r0 + 3.01).abs() < 0.1, "eğim {slope} -3 dB: {r0}");
        }
    }

    #[test]
    fn bass_boost_raises_lows() {
        let mut bb = BassBoost::new(FS);
        bb.set(100.0, 12.0);
        bb.reset();
        assert!((tone_gain_db(&mut bb, 20.0) - 12.0).abs() < 0.5);
        assert!(tone_gain_db(&mut bb, 5000.0).abs() < 0.1);
    }

    #[test]
    fn type_change_does_not_blow_up() {
        let mut eq = ParametricEq::new(FS);
        let mut buf = vec![0.5f32; 256];
        for k in 0..7 {
            eq.set_band(3, true, kind_from_index(k as f32), 300.0, 18.0, 24.0, 2);
            eq.process(&mut buf);
            assert!(buf.iter().all(|s| s.is_finite()));
        }
    }
}
