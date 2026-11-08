//! Dinamik işlemciler: noise gate, downward expander, kompresör, de-esser.
//!
//! Hepsi mono `f32` tamponlar üzerinde çalışır. `process` içinde heap
//! allocation, kilit ve I/O yoktur; zaman sabiti katsayıları ayar
//! fonksiyonlarında (blok başına bir kez) hesaplanır.

use crate::biquad::{Biquad, Coeffs, FilterType};
use crate::smooth::Smoother;
use crate::{Processor, db_to_lin, lin_to_db};

/// Zarf dedektörlerinde bu değerin altı sıfıra çekilir (denormal koruması).
const ENV_FLOOR: f32 = 1e-9;

/// Milisaniye cinsinden zaman sabitini örnek başına üstel çarpana çevirir.
/// `current = target + (current - target) * coef`
#[inline]
fn time_coef(ms: f32, sample_rate: f32) -> f32 {
    let n = (ms.max(0.01) * 0.001 * sample_rate).max(1.0);
    (-1.0 / n).exp()
}

/// Anlık atak, tek kutuplu release'li tepe zarfı. Yeni zarf değerini döndürür.
#[inline]
fn peak_env(env: f32, x_abs: f32, release_coef: f32) -> f32 {
    let e = if x_abs > env {
        x_abs
    } else {
        x_abs + (env - x_abs) * release_coef
    };
    if e < ENV_FLOOR { 0.0 } else { e }
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

/// Histerezisli noise gate.
///
/// Zarf eşiği aşınca açılır; zarf `eşik − histerezis` altında `hold`
/// süresi boyunca kalırsa kapanır. Kazanç açıkken 1.0'a `attack`, kapalıyken
/// `range`'e `release` zaman sabitiyle yaklaşır (tık yok).
pub struct Gate {
    sample_rate: f32,
    threshold_db: f32,
    hysteresis_db: f32,
    attack_ms: f32,
    hold_ms: f32,
    release_ms: f32,
    range_db: f32,

    open_thr: f32,
    close_thr: f32,
    attack_coef: f32,
    release_coef: f32,
    det_release_coef: f32,
    hold_samples: u32,
    range_lin: f32,

    env: f32,
    gain: f32,
    open: bool,
    hold_left: u32,
    min_gain: f32,
}

impl Gate {
    pub fn new(sample_rate: f32) -> Self {
        let mut g = Self {
            sample_rate,
            threshold_db: -50.0,
            hysteresis_db: 4.0,
            attack_ms: 2.0,
            hold_ms: 50.0,
            release_ms: 100.0,
            range_db: -40.0,
            open_thr: 0.0,
            close_thr: 0.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            det_release_coef: 0.0,
            hold_samples: 0,
            range_lin: 1.0,
            env: 0.0,
            gain: 1.0,
            open: false,
            hold_left: 0,
            min_gain: 1.0,
        };
        g.update_coeffs();
        // Kapalı başla: ilk örneklerde gürültü sızmasın.
        g.gain = g.range_lin;
        g.min_gain = g.gain;
        g
    }

    /// `range_db` ≤ 0 (ör. −40): kapalıyken uygulanan azami zayıflatma.
    pub fn set(
        &mut self,
        threshold_db: f32,
        hysteresis_db: f32,
        attack_ms: f32,
        hold_ms: f32,
        release_ms: f32,
        range_db: f32,
    ) {
        self.threshold_db = threshold_db;
        self.hysteresis_db = hysteresis_db.max(0.0);
        self.attack_ms = attack_ms;
        self.hold_ms = hold_ms.max(0.0);
        self.release_ms = release_ms;
        self.range_db = range_db.clamp(-120.0, 0.0);
        self.update_coeffs();
    }

    fn update_coeffs(&mut self) {
        let sr = self.sample_rate;
        self.open_thr = db_to_lin(self.threshold_db);
        self.close_thr = db_to_lin(self.threshold_db - self.hysteresis_db);
        self.attack_coef = time_coef(self.attack_ms, sr);
        self.release_coef = time_coef(self.release_ms, sr);
        self.det_release_coef = time_coef(10.0, sr);
        self.hold_samples = (self.hold_ms * 0.001 * sr) as u32;
        self.range_lin = db_to_lin(self.range_db);
    }

    /// Son çağrıdan beri uygulanan AZAMİ kazanç azaltması (pozitif dB,
    /// 0 = yok). Biriktiriciyi sıfırlar.
    pub fn gain_reduction_db(&mut self) -> f32 {
        let gr = (-lin_to_db(self.min_gain)).max(0.0);
        self.min_gain = self.gain;
        gr
    }
}

impl Processor for Gate {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf {
            self.env = peak_env(self.env, s.abs(), self.det_release_coef);

            if self.env > self.open_thr {
                self.open = true;
                self.hold_left = self.hold_samples;
            } else if self.open {
                if self.env < self.close_thr {
                    if self.hold_left == 0 {
                        self.open = false;
                    } else {
                        self.hold_left -= 1;
                    }
                } else {
                    // Histerezis bandı: açık kal, hold süresini yenile.
                    self.hold_left = self.hold_samples;
                }
            }

            let (target, coef) = if self.open {
                (1.0, self.attack_coef)
            } else {
                (self.range_lin, self.release_coef)
            };
            self.gain = target + (self.gain - target) * coef;
            if (self.gain - target).abs() < 1e-7 {
                self.gain = target;
            }
            self.min_gain = self.min_gain.min(self.gain);
            *s *= self.gain;
        }
    }

    fn reset(&mut self) {
        self.env = 0.0;
        self.open = false;
        self.hold_left = 0;
        self.gain = self.range_lin;
        self.min_gain = self.gain;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_coeffs();
    }
}

// ---------------------------------------------------------------------------
// Expander
// ---------------------------------------------------------------------------

/// Downward expander: eşiğin altında `(seviye − eşik)·(oran − 1)` dB kazanç.
///
/// Kazanç dB alanında yumuşatılır: yukarı (açılma) `attack`, aşağı `release`.
pub struct Expander {
    sample_rate: f32,
    threshold_db: f32,
    ratio: f32,
    attack_ms: f32,
    release_ms: f32,
    range_db: f32,

    threshold_lin: f32,
    attack_coef: f32,
    release_coef: f32,
    det_release_coef: f32,

    env: f32,
    /// Geçerli kazanç (dB, ≤ 0).
    gain_db: f32,
    max_gr: f32,
}

impl Expander {
    pub fn new(sample_rate: f32) -> Self {
        let mut e = Self {
            sample_rate,
            threshold_db: -50.0,
            ratio: 1.0,
            attack_ms: 5.0,
            release_ms: 100.0,
            range_db: -40.0,
            threshold_lin: 0.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            det_release_coef: 0.0,
            env: 0.0,
            gain_db: 0.0,
            max_gr: 0.0,
        };
        e.update_coeffs();
        e
    }

    /// `range_db` ≤ 0: azami zayıflatma. `ratio` = 1 → şeffaf.
    pub fn set(
        &mut self,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        range_db: f32,
    ) {
        self.threshold_db = threshold_db;
        self.ratio = ratio.max(1.0);
        self.attack_ms = attack_ms;
        self.release_ms = release_ms;
        self.range_db = range_db.clamp(-120.0, 0.0);
        self.update_coeffs();
    }

    fn update_coeffs(&mut self) {
        let sr = self.sample_rate;
        self.threshold_lin = db_to_lin(self.threshold_db);
        self.attack_coef = time_coef(self.attack_ms, sr);
        self.release_coef = time_coef(self.release_ms, sr);
        self.det_release_coef = time_coef(20.0, sr);
    }

    /// Son çağrıdan beri uygulanan AZAMİ kazanç azaltması (pozitif dB).
    pub fn gain_reduction_db(&mut self) -> f32 {
        let gr = self.max_gr;
        self.max_gr = (-self.gain_db).max(0.0);
        gr
    }
}

impl Processor for Expander {
    fn process(&mut self, buf: &mut [f32]) {
        let slope = self.ratio - 1.0;
        for s in buf {
            self.env = peak_env(self.env, s.abs(), self.det_release_coef);

            let target_db = if self.env >= self.threshold_lin || slope == 0.0 {
                0.0
            } else {
                ((lin_to_db(self.env) - self.threshold_db) * slope).max(self.range_db)
            };
            // Aşağı gidiş = release, yukarı (açılma) = attack.
            let coef = if target_db < self.gain_db {
                self.release_coef
            } else {
                self.attack_coef
            };
            self.gain_db = target_db + (self.gain_db - target_db) * coef;
            if (self.gain_db - target_db).abs() < 1e-5 {
                self.gain_db = target_db;
            }
            self.max_gr = self.max_gr.max(-self.gain_db);

            if self.gain_db != 0.0 {
                *s *= db_to_lin(self.gain_db);
            }
        }
    }

    fn reset(&mut self) {
        self.env = 0.0;
        self.gain_db = 0.0;
        self.max_gr = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_coeffs();
    }
}

// ---------------------------------------------------------------------------
// Compressor
// ---------------------------------------------------------------------------

/// Sidechain high-pass bu frekansın altında devre dışı bırakılır.
const SIDECHAIN_BYPASS_HZ: f32 = 20.0;

/// Soft-knee statik kompresör eğrisi (Giannoulis/Massberg/Reiss 2012).
/// Makeup'sız çıkış seviyesini dB olarak döndürür.
pub fn static_curve_db(threshold_db: f32, ratio: f32, knee_db: f32, input_db: f32) -> f32 {
    let ratio = ratio.max(1.0);
    let w = knee_db.max(0.0);
    let d = input_db - threshold_db;
    if 2.0 * d < -w {
        input_db
    } else if w > 0.0 && 2.0 * d.abs() <= w {
        let t = d + w * 0.5;
        input_db + (1.0 / ratio - 1.0) * t * t / (2.0 * w)
    } else {
        threshold_db + d / ratio
    }
}

/// Feed-forward kompresör: tepe dedektör → statik eğri → log alanında
/// atak/release yumuşatma → makeup. Sidechain high-pass yalnızca dedektörde.
pub struct Compressor {
    sample_rate: f32,
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    attack_ms: f32,
    release_ms: f32,
    sidechain_hpf_hz: f32,

    attack_coef: f32,
    release_coef: f32,
    det_release_coef: f32,
    /// Bu doğrusal seviyenin altında kazanç azaltması kesinlikle 0'dır.
    knee_floor_lin: f32,

    hpf: Biquad,
    hpf_active: bool,
    makeup: Smoother,

    env: f32,
    /// Geçerli kazanç azaltması (pozitif dB).
    gr_db: f32,
    max_gr: f32,
}

impl Compressor {
    pub fn new(sample_rate: f32) -> Self {
        let mut c = Self {
            sample_rate,
            threshold_db: -20.0,
            ratio: 1.0,
            knee_db: 6.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            sidechain_hpf_hz: SIDECHAIN_BYPASS_HZ,
            attack_coef: 0.0,
            release_coef: 0.0,
            det_release_coef: 0.0,
            knee_floor_lin: 0.0,
            hpf: Biquad::default(),
            hpf_active: false,
            makeup: Smoother::new(1.0, sample_rate),
            env: 0.0,
            gr_db: 0.0,
            max_gr: 0.0,
        };
        c.update_coeffs();
        c
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set(
        &mut self,
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
        sidechain_hpf_hz: f32,
    ) {
        let hpf_changed = sidechain_hpf_hz != self.sidechain_hpf_hz;
        self.threshold_db = threshold_db;
        self.ratio = ratio.max(1.0);
        self.knee_db = knee_db.max(0.0);
        self.attack_ms = attack_ms;
        self.release_ms = release_ms;
        self.sidechain_hpf_hz = sidechain_hpf_hz;
        self.makeup.set_target(db_to_lin(makeup_db));
        self.update_coeffs();
        if hpf_changed {
            self.update_hpf();
        }
    }

    fn update_coeffs(&mut self) {
        let sr = self.sample_rate;
        self.attack_coef = time_coef(self.attack_ms, sr);
        self.release_coef = time_coef(self.release_ms, sr);
        self.det_release_coef = time_coef(5.0, sr);
        self.knee_floor_lin = db_to_lin(self.threshold_db - self.knee_db * 0.5);
    }

    fn update_hpf(&mut self) {
        let active = self.sidechain_hpf_hz > SIDECHAIN_BYPASS_HZ;
        if active {
            self.hpf.set_coeffs(Coeffs::new(
                FilterType::HighPass,
                self.sample_rate,
                self.sidechain_hpf_hz,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
            ));
            if !self.hpf_active {
                // Bypass sırasında bayat kalan durumu at.
                self.hpf.reset();
            }
        }
        self.hpf_active = active;
    }

    /// Son çağrıdan beri uygulanan AZAMİ kazanç azaltması (pozitif dB,
    /// makeup hariç). Biriktiriciyi sıfırlar.
    pub fn gain_reduction_db(&mut self) -> f32 {
        let gr = self.max_gr;
        self.max_gr = self.gr_db;
        gr
    }
}

impl Processor for Compressor {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf {
            let x = *s;
            let sc = if self.hpf_active { self.hpf.tick(x) } else { x };
            self.env = peak_env(self.env, sc.abs(), self.det_release_coef);

            let target = if self.env > self.knee_floor_lin {
                let level = lin_to_db(self.env);
                (level - static_curve_db(self.threshold_db, self.ratio, self.knee_db, level))
                    .max(0.0)
            } else {
                0.0
            };
            // GR artıyorsa atak, azalıyorsa release.
            let coef = if target > self.gr_db {
                self.attack_coef
            } else {
                self.release_coef
            };
            self.gr_db = target + (self.gr_db - target) * coef;
            if self.gr_db < 1e-6 {
                self.gr_db = 0.0;
            }
            self.max_gr = self.max_gr.max(self.gr_db);

            let g = if self.gr_db > 0.0 {
                db_to_lin(-self.gr_db)
            } else {
                1.0
            };
            *s = x * g * self.makeup.next_value();
        }
    }

    fn reset(&mut self) {
        self.hpf.reset();
        self.env = 0.0;
        self.gr_db = 0.0;
        self.max_gr = 0.0;
        let target = self.makeup.target();
        self.makeup.snap(target);
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.makeup.set_sample_rate(sample_rate);
        self.update_coeffs();
        self.update_hpf();
    }
}

// ---------------------------------------------------------------------------
// DeEsser
// ---------------------------------------------------------------------------

/// De-esser'da sabit sıkıştırma oranı (4:1).
const DEESSER_RATIO: f32 = 4.0;

/// Split-band de-esser: yalnızca sibilant bandı azaltılır.
/// `out = x − (1 − g)·bp(x)`; eşik altında `g = 1` ve çıkış girişe eşittir.
pub struct DeEsser {
    sample_rate: f32,
    freq_hz: f32,
    bandwidth_oct: f32,
    threshold_db: f32,
    range_db: f32,

    threshold_lin: f32,
    attack_coef: f32,
    release_coef: f32,
    gain_coef: f32,

    bp: Biquad,
    env: f32,
    gain: f32,
    min_gain: f32,
}

impl DeEsser {
    pub fn new(sample_rate: f32) -> Self {
        let mut d = Self {
            sample_rate,
            freq_hz: 6500.0,
            bandwidth_oct: 1.0,
            threshold_db: -30.0,
            range_db: 6.0,
            threshold_lin: 0.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            gain_coef: 0.0,
            bp: Biquad::default(),
            env: 0.0,
            gain: 1.0,
            min_gain: 1.0,
        };
        d.update_coeffs();
        d
    }

    /// `range_db` ≥ 0: azami azaltma (dB).
    pub fn set(&mut self, freq_hz: f32, bandwidth_oct: f32, threshold_db: f32, range_db: f32) {
        let band_changed = freq_hz != self.freq_hz || bandwidth_oct != self.bandwidth_oct;
        self.freq_hz = freq_hz;
        self.bandwidth_oct = bandwidth_oct;
        self.threshold_db = threshold_db;
        self.range_db = range_db.clamp(0.0, 60.0);
        self.threshold_lin = db_to_lin(threshold_db);
        if band_changed {
            self.update_band();
        }
    }

    fn update_coeffs(&mut self) {
        let sr = self.sample_rate;
        self.attack_coef = time_coef(1.0, sr);
        self.release_coef = time_coef(60.0, sr);
        self.gain_coef = time_coef(2.0, sr);
        self.threshold_lin = db_to_lin(self.threshold_db);
        self.update_band();
    }

    fn update_band(&mut self) {
        // Oktav cinsinden bant genişliğinden Q: sqrt(2^N) / (2^N − 1).
        let p = 2f32.powf(self.bandwidth_oct.clamp(0.1, 4.0));
        let q = p.sqrt() / (p - 1.0);
        self.bp.set_coeffs(Coeffs::new(
            FilterType::BandPass,
            self.sample_rate,
            self.freq_hz,
            q,
            0.0,
        ));
    }

    /// Son çağrıdan beri uygulanan AZAMİ kazanç azaltması (pozitif dB).
    pub fn gain_reduction_db(&mut self) -> f32 {
        let gr = (-lin_to_db(self.min_gain)).max(0.0);
        self.min_gain = self.gain;
        gr
    }
}

impl Processor for DeEsser {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf {
            let x = *s;
            let bp = self.bp.tick(x);

            // Tek kutuplu tepe dedektörü: hızlı atak, yavaş release.
            let a = bp.abs();
            let coef = if a > self.env {
                self.attack_coef
            } else {
                self.release_coef
            };
            self.env = a + (self.env - a) * coef;
            if self.env < ENV_FLOOR {
                self.env = 0.0;
            }

            let target = if self.env > self.threshold_lin {
                let over = lin_to_db(self.env) - self.threshold_db;
                let reduction = (over * (1.0 - 1.0 / DEESSER_RATIO)).min(self.range_db);
                db_to_lin(-reduction)
            } else {
                1.0
            };
            self.gain = target + (self.gain - target) * self.gain_coef;
            if (self.gain - target).abs() < 1e-7 {
                self.gain = target;
            }
            self.min_gain = self.min_gain.min(self.gain);

            *s = x - (1.0 - self.gain) * bp;
        }
    }

    fn reset(&mut self) {
        self.bp.reset();
        self.env = 0.0;
        self.gain = 1.0;
        self.min_gain = 1.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.update_coeffs();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const FS: f32 = 48_000.0;
    const BLOCK: usize = 512;

    fn sine(freq: f32, level_db: f32, secs: f32) -> Vec<f32> {
        let amp = db_to_lin(level_db);
        let n = (secs * FS) as usize;
        (0..n)
            .map(|i| amp * (2.0 * PI * freq * i as f32 / FS).sin())
            .collect()
    }

    fn run<P: Processor>(p: &mut P, input: &[f32]) -> Vec<f32> {
        let mut out = input.to_vec();
        for chunk in out.chunks_mut(BLOCK) {
            p.process(chunk);
        }
        out
    }

    /// Tepe genlik eşdeğeri (RMS·√2) dBFS; sinüs için tepe seviyesi.
    fn amp_db(x: &[f32]) -> f32 {
        let ms: f64 = x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64;
        (10.0 * (2.0 * ms).max(1e-30).log10()) as f32
    }

    /// Son `secs` saniyenin seviyesi.
    fn tail_db(x: &[f32], secs: f32) -> f32 {
        let n = (secs * FS) as usize;
        amp_db(&x[x.len() - n..])
    }

    fn approx(a: f32, b: f32, tol: f32) {
        assert!((a - b).abs() <= tol, "{a} ≉ {b} (±{tol})");
    }

    // --- Kompresör: statik eğri ---------------------------------------

    #[test]
    fn curve_regions() {
        let (t, r, k) = (-20.0, 4.0, 10.0);
        // Diz altı: birebir.
        for x in [-80.0, -40.0, -25.5] {
            approx(static_curve_db(t, r, k, x), x, 1e-5);
        }
        // Diz üstü: eşik + (x − eşik)/oran.
        for x in [-14.0, -5.0, 0.0] {
            approx(static_curve_db(t, r, k, x), t + (x - t) / r, 1e-4);
        }
        // Diz uçlarında süreklilik.
        approx(static_curve_db(t, r, k, -25.0), -25.0, 1e-4);
        approx(static_curve_db(t, r, k, -15.0), -18.75, 1e-4);
        // Knee 0 → sert diz.
        approx(static_curve_db(t, r, 0.0, -20.0), -20.0, 1e-5);
        approx(static_curve_db(t, r, 0.0, -10.0), -17.5, 1e-5);
    }

    #[test]
    fn curve_knee_is_continuous_monotonic_and_bounded() {
        let (t, r, k) = (-20.0, 4.0, 10.0);
        let mut prev = static_curve_db(t, r, k, -40.0);
        let mut x = -40.0f32;
        while x < 0.0 {
            x += 0.01;
            let y = static_curve_db(t, r, k, x);
            assert!(y >= prev - 1e-5, "monoton değil @ {x}");
            assert!((y - prev).abs() < 0.02, "süreksiz @ {x}: {prev} → {y}");
            // Sert diz eğrisi: yumuşak diz onun altında kalır, sapma ≤ W·(1−1/R)/8.
            let hard = if x < t { x } else { t + (x - t) / r };
            assert!(y <= hard + 1e-4, "sert dizin üstünde @ {x}");
            assert!(
                y >= hard - k * (1.0 - 1.0 / r) / 8.0 - 1e-4,
                "çok aşağıda @ {x}"
            );
            prev = y;
        }
    }

    // --- Kompresör: işleme --------------------------------------------

    fn comp(thr: f32, ratio: f32, knee: f32, makeup: f32, hpf: f32) -> Compressor {
        let mut c = Compressor::new(FS);
        c.set(thr, ratio, knee, 5.0, 100.0, makeup, hpf);
        c
    }

    #[test]
    fn compressor_static_level_and_gr() {
        let x = sine(1000.0, -6.0, 3.0);
        let mut c = comp(-20.0, 4.0, 0.0, 0.0, 20.0);
        let mut out = x.clone();
        let split = (2.5 * FS) as usize;
        for chunk in out[..split].chunks_mut(BLOCK) {
            c.process(chunk);
        }
        c.gain_reduction_db(); // oturmayı at
        for chunk in out[split..].chunks_mut(BLOCK) {
            c.process(chunk);
        }
        let level = tail_db(&out, 0.25);
        approx(level, -16.5, 0.5);
        approx(c.gain_reduction_db(), 10.5, 0.7);
    }

    #[test]
    fn compressor_makeup_and_unity_ratio() {
        let x = sine(1000.0, -6.0, 2.0);
        let base = tail_db(&run(&mut comp(-20.0, 4.0, 0.0, 0.0, 20.0), &x), 0.25);
        let up = tail_db(&run(&mut comp(-20.0, 4.0, 0.0, 6.0, 20.0), &x), 0.25);
        approx(up - base, 6.0, 0.2);

        // Oran 1 → şeffaf.
        let y = run(&mut comp(-30.0, 1.0, 6.0, 0.0, 20.0), &x);
        let max_diff = x
            .iter()
            .zip(&y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(max_diff < 1e-4, "ratio 1 şeffaf değil: {max_diff}");
    }

    #[test]
    fn compressor_sidechain_hpf_ignores_lows() {
        let x = sine(60.0, -6.0, 2.0);
        let mut low = comp(-20.0, 4.0, 0.0, 0.0, 20.0);
        let mut high = comp(-20.0, 4.0, 0.0, 0.0, 300.0);
        let y_low = run(&mut low, &x);
        let y_high = run(&mut high, &x);
        let (a, b) = (tail_db(&y_low, 0.5), tail_db(&y_high, 0.5));
        assert!(b > a + 3.0, "HPF 300 daha az sıkıştırmalı: {a} vs {b}");
        assert!(low.gain_reduction_db() > 3.0);
        // Kaçan GR'nin tam sıfır olduğu yer: dedektör 60 Hz'i görmez.
        let mut high2 = comp(-20.0, 4.0, 0.0, 0.0, 300.0);
        run(&mut high2, &x);
        assert!(high2.gain_reduction_db() < 0.5);
    }

    #[test]
    fn compressor_makeup_has_no_zipper() {
        let x = sine(1000.0, -20.0, 0.5);
        let mut c = comp(0.0, 4.0, 0.0, 0.0, 20.0);
        let mut out = x.clone();
        let half = out.len() / 3;
        for chunk in out[..half].chunks_mut(256) {
            c.process(chunk);
        }
        c.set(0.0, 4.0, 0.0, 5.0, 100.0, 12.0, 20.0);
        for chunk in out[half..].chunks_mut(256) {
            c.process(chunk);
        }
        let step = |v: &[f32]| {
            v.windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max)
        };
        // Nihai kazanç 4×; yumuşatma yoksa tek örnekte ~3×tepe zıplar.
        assert!(step(&out) <= step(&x) * 4.0 * 1.05, "zipper");
        // Ve gerçekten +12 dB'ye ulaşmış olmalı.
        approx(tail_db(&out, 0.1) - tail_db(&x, 0.1), 12.0, 0.2);
    }

    // --- Gate -----------------------------------------------------------

    fn gate(hyst: f32, hold: f32) -> Gate {
        let mut g = Gate::new(FS);
        g.set(-50.0, hyst, 1.0, hold, 50.0, -40.0);
        g
    }

    #[test]
    fn gate_attenuates_quiet_and_passes_loud() {
        let quiet = sine(1000.0, -60.0, 1.0);
        let mut g = gate(4.0, 50.0);
        approx(tail_db(&run(&mut g, &quiet), 0.25), -100.0, 1.0);

        let loud = sine(1000.0, -30.0, 1.0);
        let mut g = gate(4.0, 50.0);
        approx(tail_db(&run(&mut g, &loud), 0.25), -30.0, 0.1);

        // Açıkken sessizleşen sinyal de release sonrası -100'e iner.
        let mut both = sine(1000.0, -30.0, 0.3);
        both.extend(sine(1000.0, -60.0, 1.0));
        let mut g = gate(4.0, 50.0);
        approx(tail_db(&run(&mut g, &both), 0.25), -100.0, 1.0);
    }

    #[test]
    fn gate_hysteresis_keeps_open() {
        let mut sig = sine(1000.0, -30.0, 0.3);
        sig.extend(sine(1000.0, -52.0, 1.0));
        // Histerezis 4 dB: kapanma eşiği -54 → -52 açık kalır.
        let y = run(&mut gate(4.0, 50.0), &sig);
        approx(tail_db(&y, 0.25), -52.0, 0.2);
        // Histerezis 0: -52 < -50 → kapanır.
        let y = run(&mut gate(0.0, 50.0), &sig);
        assert!(tail_db(&y, 0.25) < -85.0);
    }

    #[test]
    fn gate_respects_hold() {
        let mut g = gate(0.0, 100.0);
        run(&mut g, &sine(1000.0, -30.0, 0.3));
        g.gain_reduction_db();
        // Zarf düşüşü (~25 ms) + hold (100 ms) dolmadan açık kalmalı.
        run(&mut g, &vec![0.0; (0.05 * FS) as usize]);
        assert!(g.gain_reduction_db() < 1.0);
        // Sonra kapanır.
        run(&mut g, &vec![0.0; (0.4 * FS) as usize]);
        assert!(g.gain_reduction_db() > 30.0);
    }

    // --- Expander -------------------------------------------------------

    fn expander(ratio: f32, range: f32) -> Expander {
        let mut e = Expander::new(FS);
        e.set(-30.0, ratio, 5.0, 50.0, range);
        e
    }

    #[test]
    fn expander_extra_attenuation_and_range() {
        let x = sine(1000.0, -40.0, 2.0);
        let y = run(&mut expander(2.0, -40.0), &x);
        approx(tail_db(&y, 0.25), -50.0, 1.0);

        let mut e = expander(2.0, -6.0);
        let y = run(&mut e, &x);
        approx(tail_db(&y, 0.25), -46.0, 0.5);
        approx(e.gain_reduction_db(), 6.0, 0.5);

        // Eşik üstü ve oran 1 → şeffaf.
        let loud = sine(1000.0, -10.0, 1.0);
        let y = run(&mut expander(2.0, -40.0), &loud);
        approx(tail_db(&y, 0.25), -10.0, 0.05);
        let y = run(&mut expander(1.0, -40.0), &x);
        approx(tail_db(&y, 0.25), -40.0, 0.05);
    }

    // --- De-esser -------------------------------------------------------

    fn deesser() -> DeEsser {
        let mut d = DeEsser::new(FS);
        d.set(7000.0, 1.0, -30.0, 12.0);
        d
    }

    #[test]
    fn deesser_reduces_only_sibilant_band() {
        let hi = sine(7000.0, -10.0, 1.0);
        let mut d = deesser();
        let red = -10.0 - tail_db(&run(&mut d, &hi), 0.25);
        assert!((6.0..=12.3).contains(&red), "azaltma {red} dB");
        approx(red, 12.0, 0.5); // 20 dB aşım × 0.75 = 15 → range 12'ye kıskaç
        assert!(d.gain_reduction_db() > 11.0);

        // Aşım küçükse range'e değil orana göre: (−10 − (−18))·0.75 ≈ 6.
        let mut d = DeEsser::new(FS);
        d.set(7000.0, 1.0, -18.0, 12.0);
        let red = -10.0 - tail_db(&run(&mut d, &hi), 0.25);
        assert!((5.0..=8.0).contains(&red), "azaltma {red} dB");

        let lo = sine(300.0, -10.0, 1.0);
        let red = -10.0 - tail_db(&run(&mut deesser(), &lo), 0.25);
        assert!(red.abs() < 0.5, "300 Hz etkilendi: {red}");
    }

    #[test]
    fn deesser_transparent_below_threshold() {
        let x = sine(7000.0, -40.0, 0.5);
        let y = run(&mut deesser(), &x);
        let max_diff = x
            .iter()
            .zip(&y)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(max_diff < 1e-4, "{max_diff}");
    }

    // --- Dayanıklılık ---------------------------------------------------

    struct Rng(u32);
    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    /// Rastgele gürültü + her 50 blokta uç parametre atlaması.
    fn stress<P: Processor>(mut p: P, mut apply: impl FnMut(&mut P, bool) -> f32) {
        let sizes = [64usize, 512, 8192, 128, 1024];
        let mut rng = Rng(0x1234_5678);
        let mut buf = vec![0.0f32; 8192];
        for block in 0..400 {
            let hi = (block / 50) % 2 == 1;
            let gr = apply(&mut p, hi);
            assert!(gr.is_finite() && gr >= 0.0, "gr = {gr}");
            let n = sizes[block % sizes.len()];
            // Bazı bloklar sessiz, bazılar yüksek genlikli.
            let amp = match block % 4 {
                0 => 1.0,
                1 => 0.001,
                2 => 0.0,
                _ => 0.1,
            };
            for s in &mut buf[..n] {
                *s = rng.next() * amp;
            }
            p.process(&mut buf[..n]);
            assert!(
                buf[..n].iter().all(|v| v.is_finite() && v.abs() < 100.0),
                "geçersiz çıkış (blok {block})"
            );
        }
    }

    #[test]
    fn stress_gate() {
        stress(Gate::new(FS), |g, hi| {
            if hi {
                g.set(0.0, 20.0, 100.0, 500.0, 2000.0, -120.0);
            } else {
                g.set(-80.0, 0.0, 0.0, 0.0, 0.0, 0.0);
            }
            g.gain_reduction_db()
        });
    }

    #[test]
    fn stress_expander() {
        stress(Expander::new(FS), |e, hi| {
            if hi {
                e.set(0.0, 20.0, 0.0, 0.0, -120.0);
            } else {
                e.set(-80.0, 1.0, 100.0, 2000.0, 0.0);
            }
            e.gain_reduction_db()
        });
    }

    #[test]
    fn stress_compressor() {
        stress(Compressor::new(FS), |c, hi| {
            if hi {
                c.set(-80.0, 100.0, 0.0, 0.0, 0.0, 24.0, 1000.0);
            } else {
                c.set(0.0, 1.0, 24.0, 100.0, 2000.0, -24.0, 5.0);
            }
            c.gain_reduction_db()
        });
    }

    #[test]
    fn stress_deesser() {
        stress(DeEsser::new(FS), |d, hi| {
            if hi {
                d.set(16_000.0, 0.1, -80.0, 40.0);
            } else {
                d.set(2_000.0, 4.0, 0.0, 0.0);
            }
            d.gain_reduction_db()
        });
    }

    #[test]
    fn reset_and_sample_rate_change_are_safe() {
        let x = sine(1000.0, -6.0, 0.2);
        let mut c = comp(-20.0, 4.0, 6.0, 3.0, 100.0);
        run(&mut c, &x);
        c.reset();
        assert_eq!(c.gain_reduction_db(), 0.0);
        c.set_sample_rate(44_100.0);
        assert!(run(&mut c, &x).iter().all(|v| v.is_finite()));

        let mut g = gate(4.0, 50.0);
        run(&mut g, &x);
        g.reset();
        g.set_sample_rate(44_100.0);
        assert!(run(&mut g, &x).iter().all(|v| v.is_finite()));
    }
}
