//! Gerçek zamanlı spektrum analizörü (UI thread'i için, ~30 FPS).
//!
//! Ses thread'inden gelen mono örnekler [`SpectrumAnalyzer::push`] ile
//! dairesel bir geçmişe yazılır. [`SpectrumAnalyzer::compute`] Hann pencereli
//! bir FFT alır, bin'leri logaritmik frekans eksenli ekran sütunlarına
//! eşler ve sütun başına dBFS değeri üretir. Düşen değerler zamanla
//! yumuşatılır (hızlı attack, sınırlı release).

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

/// dBFS taban değeri. Sessizlik ve geçersiz değerler bu seviyeye indirilir.
pub const FLOOR_DB: f32 = -120.0;

/// Varsayılan release hızı (dB/saniye): düşen değerler en fazla bu hızla iner.
pub const DEFAULT_RELEASE_DB_PER_S: f32 = 30.0;

/// Çıktının üst sınırı (dBFS). Tam ölçekli sinüs ≈ 0 dB okur; biraz pay bırakılır.
const CEIL_DB: f32 = 12.0;

/// Bir ekran sütununun FFT bin'lerine eşlenme biçimi (önceden hesaplanır).
#[derive(Clone, Copy)]
enum ColumnMap {
    /// Sütun bandındaki bin aralığı (dahil): bu bin'lerin dB maksimumu alınır.
    Range(usize, usize),
    /// Bandda bin yok: `(alt_bin, üst_bin, kesir)` ile dB'de doğrusal interpolasyon.
    Interp(usize, usize, f32),
}

/// Logaritmik frekans eksenli spektrum analizörü.
pub struct SpectrumAnalyzer {
    sample_rate: f32,
    fft_size: usize,
    fft: Arc<dyn Fft<f32>>,
    /// Hann penceresi (periyodik), `fft_size` uzunluğunda.
    window: Vec<f32>,
    /// Genlik kalibrasyonu: tek taraflı spektrum için `2 / sum(window)`.
    scale: f32,
    /// Dairesel örnek geçmişi (en eski örnek `write_pos`'ta).
    history: Vec<f32>,
    write_pos: usize,
    /// FFT çalışma tamponu ve scratch alanı.
    buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Her bin için dBFS (0..=fft_size/2).
    bin_db: Vec<f32>,
    /// Zaman yumuşatmalı sütun değerleri (dB).
    smoothed: Vec<f32>,
    /// Sütun → bin eşlemesi ve bunun hesaplandığı parametreler.
    map: Vec<ColumnMap>,
    map_key: Option<(usize, f32, f32)>,
    release_db_per_s: f32,
}

impl SpectrumAnalyzer {
    /// Yeni analizör oluşturur. `fft_size` ikinin kuvveti olmalıdır (örn. 4096).
    /// Tüm tamponlar burada ayrılır.
    pub fn new(sample_rate: f32, fft_size: usize) -> Self {
        assert!(
            fft_size >= 4 && fft_size.is_power_of_two(),
            "fft_size ikinin kuvveti olmalı"
        );
        let fft = FftPlanner::<f32>::new().plan_fft_forward(fft_size);
        let scratch_len = fft.get_inplace_scratch_len();

        let window: Vec<f32> = (0..fft_size)
            .map(|i| {
                let phase = std::f32::consts::TAU * i as f32 / fft_size as f32;
                0.5 - 0.5 * phase.cos()
            })
            .collect();
        let sum: f32 = window.iter().sum();

        Self {
            sample_rate,
            fft_size,
            fft,
            scale: 2.0 / sum,
            window,
            history: vec![0.0; fft_size],
            write_pos: 0,
            buf: vec![Complex::new(0.0, 0.0); fft_size],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            bin_db: vec![FLOOR_DB; fft_size / 2 + 1],
            smoothed: Vec::new(),
            map: Vec::new(),
            map_key: None,
            release_db_per_s: DEFAULT_RELEASE_DB_PER_S,
        }
    }

    /// Release hızını ayarlar (dB/saniye). Negatif/geçersiz değerler 0 sayılır.
    pub fn set_release(&mut self, db_per_s: f32) {
        self.release_db_per_s = if db_per_s.is_finite() {
            db_per_s.max(0.0)
        } else {
            0.0
        };
    }

    /// Yeni mono örnekleri ekler; yalnızca en son `fft_size` örnek tutulur.
    /// Allocation yapmaz.
    pub fn push(&mut self, samples: &[f32]) {
        let n = self.fft_size;
        // Gelen veri geçmişten uzunsa yalnızca son n örnek önemlidir.
        let samples = if samples.len() > n {
            &samples[samples.len() - n..]
        } else {
            samples
        };
        let first = (n - self.write_pos).min(samples.len());
        self.history[self.write_pos..self.write_pos + first].copy_from_slice(&samples[..first]);
        let rest = samples.len() - first;
        self.history[..rest].copy_from_slice(&samples[first..]);
        self.write_pos = (self.write_pos + samples.len()) % n;
    }

    /// Spektrumu `out`'a yazar: sütun başına bir dBFS değeri; sütunlar
    /// `f_min..f_max` arasında logaritmik dağılır (bkz. [`column_freq`]).
    /// Attack anlıktır; düşüşler en fazla `release * dt` dB olur.
    /// `out.len()` değişmediği sürece allocation yapmaz.
    pub fn compute(&mut self, out: &mut [f32], f_min: f32, f_max: f32, dt: f32) {
        let n_cols = out.len();
        if n_cols == 0 {
            return;
        }

        // Geçmişi zaman sırasına diz (en eski → en yeni) ve pencerele.
        let n = self.fft_size;
        let (old, new) = self.history.split_at(self.write_pos);
        for ((b, &s), &w) in self
            .buf
            .iter_mut()
            .zip(new.iter().chain(old.iter()))
            .zip(self.window.iter())
        {
            *b = Complex::new(s * w, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.buf, &mut self.scratch);

        // Bin başına dBFS (kalibre edilmiş tek taraflı genlik).
        for (db, c) in self.bin_db.iter_mut().zip(self.buf.iter()) {
            let mag = c.norm() * self.scale;
            *db = if mag.is_finite() {
                (20.0 * mag.max(1e-6).log10()).clamp(FLOOR_DB, CEIL_DB)
            } else {
                FLOOR_DB
            };
        }

        // Sütun eşlemesi ve yumuşatma durumu gerekirse yeniden kurulur.
        let (f_lo, f_hi) = sanitize_range(f_min, f_max);
        if self.map_key != Some((n_cols, f_lo, f_hi)) {
            self.rebuild_map(n_cols, f_lo, f_hi);
        }
        if self.smoothed.len() != n_cols {
            self.smoothed.clear();
            self.smoothed.resize(n_cols, FLOOR_DB);
        }

        let max_drop = if dt.is_finite() {
            self.release_db_per_s * dt.max(0.0)
        } else {
            0.0
        };
        let last_bin = n / 2;
        for ((o, s), m) in out
            .iter_mut()
            .zip(self.smoothed.iter_mut())
            .zip(self.map.iter())
        {
            let raw = match *m {
                ColumnMap::Range(lo, hi) => self.bin_db[lo..=hi.min(last_bin)]
                    .iter()
                    .fold(FLOOR_DB, |a, &b| a.max(b)),
                ColumnMap::Interp(k0, k1, frac) => {
                    let (a, b) = (self.bin_db[k0], self.bin_db[k1]);
                    a + (b - a) * frac
                }
            };
            // Yükselme anında izlenir, düşüş release ile sınırlanır.
            let v = raw.max(*s - max_drop).clamp(FLOOR_DB, CEIL_DB);
            *s = v;
            *o = v;
        }
    }

    /// Geçmişi ve yumuşatma durumunu temizler (örn. cihaz değişince).
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.write_pos = 0;
        self.smoothed.fill(FLOOR_DB);
        self.bin_db.fill(FLOOR_DB);
    }

    /// Sütun → bin eşlemesini hesaplar. Yalnızca parametreler değişince çalışır.
    fn rebuild_map(&mut self, n_cols: usize, f_min: f32, f_max: f32) {
        let bin_hz = self.sample_rate / self.fft_size as f32;
        let last_bin = self.fft_size / 2;
        // Komşu sütunlar arası geometrik oran; band sınırı oranın karekökü kadar.
        let half_step = if n_cols > 1 {
            (f_max / f_min).powf(0.5 / (n_cols - 1) as f32)
        } else {
            (f_max / f_min).sqrt()
        };

        self.map.clear();
        self.map.reserve(n_cols);
        for col in 0..n_cols {
            let fc = column_freq(col, n_cols, f_min, f_max);
            let (band_lo, band_hi) = if n_cols > 1 {
                (fc / half_step, fc * half_step)
            } else {
                (f_min, f_max)
            };
            let k_lo = (band_lo / bin_hz).ceil().max(0.0) as usize;
            let k_hi = ((band_hi / bin_hz).floor().max(0.0) as usize).min(last_bin);
            if k_lo <= k_hi {
                self.map.push(ColumnMap::Range(k_lo, k_hi));
            } else {
                let pos = (fc / bin_hz).clamp(0.0, last_bin as f32);
                let k0 = (pos.floor() as usize).min(last_bin);
                let k1 = (k0 + 1).min(last_bin);
                self.map.push(ColumnMap::Interp(k0, k1, pos - k0 as f32));
            }
        }
        self.map_key = Some((n_cols, f_min, f_max));
    }
}

/// Geçersiz/ters frekans aralıklarını güvenli hale getirir (`0 < f_min <= f_max`).
fn sanitize_range(f_min: f32, f_max: f32) -> (f32, f32) {
    let lo = if f_min.is_finite() {
        f_min.max(1e-3)
    } else {
        20.0
    };
    let hi = if f_max.is_finite() { f_max.max(lo) } else { lo };
    (lo, hi)
}

/// `col` numaralı sütunun merkez frekansı: `f_min`'den `f_max`'a logaritmik
/// dağılım (col 0 → `f_min`, col `n_cols-1` → `f_max`).
pub fn column_freq(col: usize, n_cols: usize, f_min: f32, f_max: f32) -> f32 {
    let (f_min, f_max) = sanitize_range(f_min, f_max);
    if n_cols <= 1 {
        return f_min;
    }
    let t = col.min(n_cols - 1) as f32 / (n_cols - 1) as f32;
    f_min * (f_max / f_min).powf(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;
    const N: usize = 4096;

    fn sine(freq: f32, amp: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / SR).sin())
            .collect()
    }

    fn nearest_col(n_cols: usize, f_min: f32, f_max: f32, freq: f32) -> usize {
        (0..n_cols)
            .min_by(|&a, &b| {
                let da = (column_freq(a, n_cols, f_min, f_max) - freq).abs();
                let db = (column_freq(b, n_cols, f_min, f_max) - freq).abs();
                da.total_cmp(&db)
            })
            .unwrap()
    }

    fn argmax(v: &[f32]) -> usize {
        (0..v.len()).max_by(|&a, &b| v[a].total_cmp(&v[b])).unwrap()
    }

    #[test]
    fn sine_1khz_minus_6db() {
        let mut an = SpectrumAnalyzer::new(SR, N);
        an.push(&sine(1000.0, 0.5, N));
        let n_cols = 120;
        let mut out = vec![0.0; n_cols];
        an.compute(&mut out, 20.0, 20_000.0, 0.033);

        let c = nearest_col(n_cols, 20.0, 20_000.0, 1000.0);
        assert!((out[c] + 6.0).abs() <= 1.0, "1 kHz sütunu: {} dB", out[c]);
        for (i, &v) in out.iter().enumerate() {
            let f = column_freq(i, n_cols, 20.0, 20_000.0);
            if !(500.0..=2000.0).contains(&f) {
                assert!(v < out[c] - 40.0, "{f} Hz: {v} dB çok yüksek");
            }
        }
    }

    #[test]
    fn sine_100hz_full_scale() {
        let mut an = SpectrumAnalyzer::new(SR, N);
        an.push(&sine(100.0, 1.0, N));
        let n_cols = 200;
        let mut out = vec![0.0; n_cols];
        an.compute(&mut out, 20.0, 20_000.0, 0.033);

        let p = argmax(&out);
        let f = column_freq(p, n_cols, 20.0, 20_000.0);
        assert!((f - 100.0).abs() <= 10.0, "tepe frekansı {f} Hz");
        assert!(out[p].abs() <= 1.5, "tepe seviyesi {} dB", out[p]);
    }

    #[test]
    fn silence_is_floor() {
        let mut an = SpectrumAnalyzer::new(SR, N);
        an.push(&vec![0.0; N]);
        let mut out = vec![0.0; 64];
        an.compute(&mut out, 20.0, 20_000.0, 0.033);
        assert!(out.iter().all(|&v| v == FLOOR_DB));
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn release_limits_fall() {
        let mut an = SpectrumAnalyzer::new(SR, N);
        let n_cols = 100;
        let mut out = vec![0.0; n_cols];
        an.push(&sine(1000.0, 1.0, N));
        an.compute(&mut out, 20.0, 20_000.0, 0.1);
        let before = out.clone();

        an.push(&vec![0.0; N]);
        an.compute(&mut out, 20.0, 20_000.0, 0.1);

        let max_drop = DEFAULT_RELEASE_DB_PER_S * 0.1;
        let p = argmax(&before);
        assert!(before[p] > -3.0);
        for (b, a) in before.iter().zip(out.iter()) {
            assert!(b - a <= max_drop + 1e-4, "düşüş {} dB", b - a);
        }
        // Tepe sütunu tam release kadar düşmüş olmalı (sessizlik hedef).
        assert!((before[p] - out[p] - max_drop).abs() < 1e-3);
    }

    #[test]
    fn set_release_changes_rate() {
        let mut an = SpectrumAnalyzer::new(SR, N);
        an.set_release(60.0);
        let mut out = vec![0.0; 50];
        an.push(&sine(1000.0, 1.0, N));
        an.compute(&mut out, 20.0, 20_000.0, 0.1);
        let peak = out[argmax(&out)];
        an.push(&vec![0.0; N]);
        an.compute(&mut out, 20.0, 20_000.0, 0.1);
        assert!((peak - out[argmax(&out)] - 6.0).abs() < 0.5);
    }

    #[test]
    fn column_freq_endpoints_and_monotonic() {
        let n = 50;
        assert!((column_freq(0, n, 20.0, 20_000.0) - 20.0).abs() < 1e-3);
        assert!((column_freq(n - 1, n, 20.0, 20_000.0) - 20_000.0).abs() < 1.0);
        let mut prev = 0.0;
        for i in 0..n {
            let f = column_freq(i, n, 20.0, 20_000.0);
            assert!(f > prev);
            prev = f;
        }
        assert_eq!(column_freq(0, 1, 20.0, 20_000.0), 20.0);
    }

    #[test]
    fn partial_history_zero_padded() {
        let mut an = SpectrumAnalyzer::new(SR, N);
        let mut out = vec![0.0; 80];
        // Hiç örnek yokken de panik olmamalı.
        an.compute(&mut out, 20.0, 20_000.0, 0.033);
        assert!(out.iter().all(|&v| v == FLOOR_DB));

        an.push(&sine(1000.0, 0.5, 300));
        an.compute(&mut out, 20.0, 20_000.0, 0.033);
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(out.iter().any(|&v| v > FLOOR_DB + 20.0));
    }

    #[test]
    fn wraparound_push_and_resize_and_reset() {
        let mut an = SpectrumAnalyzer::new(SR, N);
        let tone = sine(1000.0, 0.5, N * 3 + 123);
        // Parçalı ve uzun push'lar; dairesel tampon sarmalanır.
        for chunk in tone.chunks(777) {
            an.push(chunk);
        }
        let mut out = vec![0.0; 120];
        an.compute(&mut out, 20.0, 20_000.0, 0.033);
        let c = nearest_col(120, 20.0, 20_000.0, 1000.0);
        assert!((out[c] + 6.0).abs() <= 1.5, "{} dB", out[c]);

        // Sütun sayısı değişince durum sıfırlanır, panik olmaz.
        let mut out2 = vec![0.0; 33];
        an.compute(&mut out2, 20.0, 20_000.0, 0.033);
        assert!(out2.iter().all(|v| v.is_finite()));

        an.reset();
        an.compute(&mut out2, 20.0, 20_000.0, 0.033);
        assert!(out2.iter().all(|&v| v == FLOOR_DB));
    }
}
