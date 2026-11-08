//! Akustik geri besleme (howling) dedektörü.
//!
//! "Dinle" (monitor) özelliği işlenmiş sesi hoparlörden çalarken mikrofon
//! bunu tekrar yakalayabilir; kazanç yeterince yüksekse tek bir frekansta
//! kendini besleyen dar bir ton oluşur.
//!
//! [`FeedbackDetector::push`] mikrofon örneklerini dairesel bir geçmişe yazar
//! ve her [`HOP`] örnekte bir Hann pencereli FFT alır. Bir kare, şu üç koşulun
//! hepsi sağlanırsa "aday" sayılır:
//!
//! - tepe seviyesi −35 dBFS'in üstünde;
//! - tepe, 150–8 kHz bandının ortalamasının en az 30 dB üstünde (baskın ton);
//! - tepe, 6–12 bin uzaklığındaki komşularının en az 20 dB üstünde (dar bant;
//!   konuşma formantları ve harmonikler bu ölçütte elenir).
//!
//! Aynı tepe bin'inde (±2 bin) aday kareler [`HOLD_S`] saniye sürerse howling
//! kabul edilir. Analiz UI thread'inde çalışır; tüm tamponlar
//! [`FeedbackDetector::new`] içinde ayrılır, `push` allocation yapmaz.

use std::f32::consts::TAU;
use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::spectrum::FLOOR_DB;

/// FFT boyutu (örnek).
const FFT_SIZE: usize = 2048;
/// Ardışık analizler arası adım (örnek).
const HOP: usize = 1024;
/// Analiz bandı (Hz, dahil).
const BAND_LO_HZ: f32 = 150.0;
const BAND_HI_HZ: f32 = 8000.0;
/// Tepenin mutlak alt sınırı (dBFS).
const MIN_PEAK_DB: f32 = -35.0;
/// Tepenin bant ortalamasının üstünde olması gereken miktar (dB).
const MIN_ABOVE_MEAN_DB: f32 = 30.0;
/// Tepenin komşularının üstünde olması gereken miktar (dB).
const MIN_ABOVE_SIDES_DB: f32 = 20.0;
/// Komşu bin aralığı: tepeden bu uzaklıktan başlayıp...
const SIDE_NEAR: usize = 6;
/// ...bu uzaklığa kadar olan bin'ler karşılaştırılır.
const SIDE_FAR: usize = 12;
/// Aynı tepenin sürmesi gereken süre (saniye).
const HOLD_S: f32 = 0.6;
/// Ardışık karelerde tepe bin'inin kayabileceği en fazla bin farkı.
const BIN_TOLERANCE: usize = 2;

/// Mikrofon girişinde kendini besleyen tek frekanslı tonu (howling) izler.
pub struct FeedbackDetector {
    sample_rate: f32,
    fft: Arc<dyn Fft<f32>>,
    /// Hann penceresi (periyodik), `FFT_SIZE` uzunluğunda.
    window: Vec<f32>,
    /// Genlik kalibrasyonu: tek taraflı spektrum için `2 / sum(window)`.
    scale: f32,
    /// FFT çalışma tamponu ve scratch alanı.
    buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Dairesel örnek geçmişi (en eski örnek `write_pos`'ta).
    history: Vec<f32>,
    write_pos: usize,
    /// Toplam alınan örnek sayısı (ilk analiz için `FFT_SIZE` gerekir).
    total: usize,
    /// Her bin için dBFS (0..=FFT_SIZE/2).
    bin_db: Vec<f32>,
    /// Analiz bandının bin aralığı (dahil).
    band: (usize, usize),
    /// Howling için gereken ardışık aday kare sayısı.
    hold_frames: usize,
    /// Mevcut aday serisinin uzunluğu, tepe bin'i ve iyileştirilmiş frekansı.
    run_len: usize,
    run_bin: usize,
    run_hz: f32,
}

impl FeedbackDetector {
    /// Yeni dedektör oluşturur. Tüm tamponlar burada ayrılır.
    pub fn new(sample_rate: f32) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex::new(0.0, 0.0); fft.get_inplace_scratch_len()];

        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (TAU * i as f32 / FFT_SIZE as f32).cos())
            .collect();
        let sum: f32 = window.iter().sum();

        let bin_hz = sample_rate / FFT_SIZE as f32;
        let lo = (BAND_LO_HZ / bin_hz).ceil() as usize;
        let hi = ((BAND_HI_HZ / bin_hz).floor() as usize).min(FFT_SIZE / 2);
        let hold_frames = ((HOLD_S * sample_rate / HOP as f32).ceil() as usize).max(1);

        Self {
            sample_rate,
            fft,
            scale: 2.0 / sum,
            window,
            buf: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            scratch,
            history: vec![0.0; FFT_SIZE],
            write_pos: 0,
            total: 0,
            bin_db: vec![FLOOR_DB; FFT_SIZE / 2 + 1],
            band: (lo, hi),
            hold_frames,
            run_len: 0,
            run_bin: 0,
            run_hz: 0.0,
        }
    }

    /// Mono mikrofon örneklerini ekler (uzunluk serbest). Her [`HOP`] örnekte
    /// bir analiz çalışır. Allocation yapmaz.
    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            self.history[self.write_pos] = if s.is_finite() { s } else { 0.0 };
            self.write_pos = (self.write_pos + 1) % FFT_SIZE;
            self.total = self.total.saturating_add(1);
            if self.total >= FFT_SIZE && (self.total - FFT_SIZE).is_multiple_of(HOP) {
                self.analyze();
            }
        }
    }

    /// Howling şu an algılanıyorsa tepe frekansını (Hz) döndürür, aksi halde `None`.
    pub fn howling(&self) -> Option<f32> {
        (self.run_len >= self.hold_frames).then_some(self.run_hz)
    }

    /// Geçmişi ve algılama durumunu sıfırlar.
    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.write_pos = 0;
        self.total = 0;
        self.run_len = 0;
    }

    /// Son `FFT_SIZE` örnek üzerinde bir kare analizi yapar ve aday serisini günceller.
    fn analyze(&mut self) {
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

        for (db, c) in self.bin_db.iter_mut().zip(self.buf.iter()) {
            let mag = c.norm() * self.scale;
            *db = if mag.is_finite() {
                (20.0 * mag.max(1e-6).log10()).max(FLOOR_DB)
            } else {
                FLOOR_DB
            };
        }

        let (lo, hi) = self.band;
        let band = &self.bin_db[lo..=hi];
        let (mut k, mut peak) = (lo, f32::NEG_INFINITY);
        for (i, &db) in band.iter().enumerate() {
            if db > peak {
                peak = db;
                k = lo + i;
            }
        }
        let mean = band.iter().sum::<f32>() / band.len() as f32;
        let side = side_max(&self.bin_db, k);

        let candidate = peak > MIN_PEAK_DB
            && peak - mean >= MIN_ABOVE_MEAN_DB
            && peak - side >= MIN_ABOVE_SIDES_DB;
        if !candidate {
            self.run_len = 0;
            return;
        }

        self.run_len = if self.run_len > 0 && k.abs_diff(self.run_bin) <= BIN_TOLERANCE {
            self.run_len + 1
        } else {
            1
        };
        self.run_bin = k;
        self.run_hz = refine_hz(&self.bin_db, k, self.sample_rate);
    }
}

/// `k` tepesinden 6–12 bin uzaktaki bin'lerin dB maksimumu (iki taraf birlikte).
fn side_max(bin_db: &[f32], k: usize) -> f32 {
    let last = bin_db.len() - 1;
    let mut m = f32::NEG_INFINITY;
    for d in SIDE_NEAR..=SIDE_FAR {
        if let Some(i) = k.checked_sub(d) {
            m = m.max(bin_db[i]);
        }
        if k + d <= last {
            m = m.max(bin_db[k + d]);
        }
    }
    m
}

/// Tepe bin'inin üç noktalı parabolik interpolasyonla Hz cinsinden konumu.
fn refine_hz(bin_db: &[f32], k: usize, sample_rate: f32) -> f32 {
    let delta = if k >= 1 && k + 1 < bin_db.len() {
        let (a, b, c) = (bin_db[k - 1], bin_db[k], bin_db[k + 1]);
        let den = a - 2.0 * b + c;
        if den.abs() > 1e-9 {
            (0.5 * (a - c) / den).clamp(-0.5, 0.5)
        } else {
            0.0
        }
    } else {
        0.0
    };
    (k as f32 + delta) * sample_rate / FFT_SIZE as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;
    const CHUNK: usize = 256;

    /// `amp_db` dBFS genlikli sinüs (`len` örnek).
    fn sine(freq: f32, amp_db: f32, len: usize) -> Vec<f32> {
        let amp = 10f32.powf(amp_db / 20.0);
        (0..len)
            .map(|i| amp * (TAU * freq * i as f32 / SR).sin())
            .collect()
    }

    /// Deterministik beyaz gürültü (RMS `rms_db` dBFS), basit xorshift ile.
    fn white_noise(rms_db: f32, len: usize) -> Vec<f32> {
        // Tekdüze [-a, a] dağılımın RMS'i a/√3 olduğundan a = √3 · RMS.
        let a = 3f32.sqrt() * 10f32.powf(rms_db / 20.0);
        let mut state: u32 = 0x1234_5678;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let u = state as f32 / u32::MAX as f32;
                a * (2.0 * u - 1.0)
            })
            .collect()
    }

    /// 120→220 Hz'e 1 sn'de kayan temel frekanslı, 20 harmonikli (1/n genlik)
    /// konuşma benzeri sinyal; 1 sn'lik kayma tekrarlanır. Tepe −6 dBFS'e normalize.
    fn gliding_harmonics(len: usize) -> Vec<f32> {
        let mut x: Vec<f32> = (0..len)
            .map(|i| {
                let u = (i as f64 / SR as f64).fract(); // sn içindeki konum (0..1)
                let phase = TAU as f64 * (120.0 * u + 50.0 * u * u);
                (1..=20)
                    .map(|n| (phase * n as f64).sin() as f32 / n as f32)
                    .sum()
            })
            .collect();
        let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let g = 0.5 / peak;
        x.iter_mut().for_each(|v| *v *= g);
        x
    }

    #[test]
    fn steady_sine_minus10_detected_after_hold() {
        let x = sine(2000.0, -10.0, 72_000); // 1.5 sn
        let mut det = FeedbackDetector::new(SR);
        let mut pushed = 0;
        let mut at_03 = None;
        for chunk in x.chunks(CHUNK) {
            det.push(chunk);
            pushed += chunk.len();
            if at_03.is_none() && pushed >= 14_400 {
                at_03 = Some(det.howling());
            }
        }
        assert!(at_03.unwrap().is_none(), "0.3 sn'de howling algılanmamalı");
        let f = det.howling().expect("1.5 sn sonunda howling algılanmalı");
        assert!((f - 2000.0).abs() < 30.0, "algılanan frekans {f} Hz");
    }

    #[test]
    fn quiet_sine_minus50_not_detected() {
        let x = sine(2000.0, -50.0, 72_000);
        let mut det = FeedbackDetector::new(SR);
        for chunk in x.chunks(CHUNK) {
            det.push(chunk);
            assert!(det.howling().is_none());
        }
    }

    #[test]
    fn white_noise_minus10_not_detected() {
        let x = white_noise(-10.0, 96_000); // 2 sn
        let mut det = FeedbackDetector::new(SR);
        for chunk in x.chunks(CHUNK) {
            det.push(chunk);
            assert!(det.howling().is_none());
        }
    }

    #[test]
    fn speech_like_harmonics_not_detected() {
        let x = gliding_harmonics(96_000); // 2 sn, iki kez 120→220 Hz kayma
        let mut det = FeedbackDetector::new(SR);
        for chunk in x.chunks(CHUNK) {
            det.push(chunk);
            assert!(
                det.howling().is_none(),
                "konuşma benzeri sinyal howling sayıldı"
            );
        }
    }

    #[test]
    fn sine_then_silence_clears_and_reset_works() {
        let mut x = sine(2000.0, -10.0, 48_000); // 1 sn sinüs
        x.extend(std::iter::repeat_n(0.0, 24_000)); // 0.5 sn sessizlik
        let mut det = FeedbackDetector::new(SR);
        let mut pushed = 0;
        let mut during = None;
        for chunk in x.chunks(CHUNK) {
            det.push(chunk);
            pushed += chunk.len();
            if pushed >= 43_200 {
                during.get_or_insert(det.howling());
            }
        }
        assert!(during.unwrap().is_some(), "sinüs sırasında howling olmalı");
        assert!(det.howling().is_none(), "sessizlikte howling bitmeli");

        // reset: tepe sürse bile durum sıfırlanır; yeniden 0.6 sn beklenir.
        det.push(&sine(2000.0, -10.0, 48_000));
        assert!(det.howling().is_some());
        det.reset();
        assert!(det.howling().is_none());
        det.push(&sine(2000.0, -10.0, 9_600)); // 0.2 sn
        assert!(det.howling().is_none());
    }
}
