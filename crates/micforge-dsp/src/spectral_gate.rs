//! Spektral gürültü kapısı / gürültü azaltma (öğrenilmiş gürültü profiliyle).
//!
//! Kullanıcı "gürültü profilini öğren"e basıp ~3 sn sessiz kalır; modül bu
//! sürede bin başına gürültü genliğini ölçer. Sonrasında her FFT bin'i için
//! genlik `profil · eşik` değerinin altındaysa bin gürültü sayılır ve
//! `reduction_db` kadar bastırılır; üstündeyse (konuşma) olduğu gibi geçer.
//!
//! # Yapı (WOLA)
//! - FFT boyutu 512, hop 128 (%75 örtüşme). Analiz ve sentez penceresi aynı
//!   **periyodik karekök-Hann** pencereridir: `w² = Hann`, Hann'ın 4 örtüşen
//!   kopyasının toplamı tam 2 olduğundan IFFT çıkışı `1 / (2·N)` ile
//!   ölçeklenince birim kazançta mükemmel yeniden yapılandırma elde edilir.
//! - Giriş/çıkış örnek bazlı çalışır (hop sınırlarında çerçeve işlenir), bu
//!   yüzden her blok boyutu desteklenir ve sonuç blok boyutundan bağımsızdır.
//! - Gecikme sabit ve tam olarak [`SpectralGate::FFT_SIZE`] örnektir: çıkış
//!   `y[i] = x[i - 512]` (kazançlar 1 iken).
//!
//! # Gerçek zamanlı güvenlik
//! Tüm tamponlar ve FFT planları `new` içinde ayrılır; `process` heap
//! allocation, kilit ya da I/O yapmaz.
//!
//! # Gürültü profili
//! Profil, öğrenme süresince bin başına `ortalama + 2·std` genliktir
//! (Rayleigh dağılımlı gürültüde bin genliği ortalamayı sık sık aşar; bu
//! yüzden salt ortalama "müzikal gürültü" üretirdi). `threshold_db` bu
//! profilin üstüne eklenir. Genlikler pencereli (karekök-Hann) ham FFT
//! genlikleridir; kaydedilen profil yalnızca aynı `FFT_SIZE` ile geçerlidir.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

use crate::{Processor, db_to_lin};

/// Tek taraflı spektrumdaki bin sayısı (`FFT_SIZE / 2 + 1`).
const BINS: usize = SpectralGate::FFT_SIZE / 2 + 1;

/// Öğrenmenin başında atlanan çerçeve sayısı: `start_learning` anında
/// çerçevenin içinde hâlâ öğrenme öncesi (örn. tuş sesi) örnekler bulunur.
const LEARN_SKIP_FRAMES: usize = SpectralGate::FFT_SIZE / SpectralGate::HOP;

/// Profil = ortalama + `PROFILE_STD_FACTOR`·std.
const PROFILE_STD_FACTOR: f64 = 2.0;

pub struct SpectralGate {
    sample_rate: f32,
    /// Bin genliği `profil · threshold_lin` altındaysa gürültü sayılır.
    threshold_lin: f32,
    /// Gürültü bin'lerinin hedef kazancı (`db_to_lin(-reduction_db)`).
    reduction_lin: f32,
    smoothing_ms: f32,
    /// Çerçeve başına one-pole katsayıları (kazanç yükselirken / düşerken).
    coef_attack: f32,
    coef_release: f32,

    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    /// Periyodik karekök-Hann penceresi.
    window: Vec<f32>,
    fft_buf: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,

    /// Son `FFT_SIZE` giriş örneği; `in_pos` bir sonraki yazma (= en eski) konum.
    in_ring: Vec<f32>,
    in_pos: usize,
    /// Mevcut hop içindeki konum (0..HOP).
    hop_pos: usize,
    /// Overlap-add birikimi (en eski örnek indeks 0).
    ola: Vec<f32>,
    /// Tamamlanmış, çıkışa sırayla verilecek HOP örnek.
    out_chunk: Vec<f32>,

    /// Çerçeve başına çalışma dizileri (BINS uzunluğunda).
    mag: Vec<f32>,
    target: Vec<f32>,
    target_smooth: Vec<f32>,
    gain: Vec<f32>,

    profile: Vec<f32>,
    has_profile: bool,

    learning: bool,
    learn_total: usize,
    learn_done: usize,
    learn_count: usize,
    sum: Vec<f64>,
    sum_sq: Vec<f64>,

    max_reduction_db: f32,
}

impl SpectralGate {
    pub const FFT_SIZE: usize = 512;
    pub const HOP: usize = 128;

    /// Yeni kapı oluşturur; tüm tamponlar burada ayrılır.
    /// Varsayılanlar: eşik 3 dB, azaltma 15 dB, yumuşatma 40 ms.
    pub fn new(sample_rate: f32) -> Self {
        let n = Self::FFT_SIZE;
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        let scratch_len = fft
            .get_inplace_scratch_len()
            .max(ifft.get_inplace_scratch_len());

        let window: Vec<f32> = (0..n)
            .map(|i| {
                let phase = std::f64::consts::TAU * i as f64 / n as f64;
                (0.5 - 0.5 * phase.cos()).sqrt() as f32
            })
            .collect();

        let mut gate = Self {
            sample_rate: 48_000.0,
            threshold_lin: 1.0,
            reduction_lin: 1.0,
            smoothing_ms: 40.0,
            coef_attack: 0.0,
            coef_release: 0.0,
            fft,
            ifft,
            window,
            fft_buf: vec![Complex::new(0.0, 0.0); n],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            in_ring: vec![0.0; n],
            in_pos: 0,
            hop_pos: 0,
            ola: vec![0.0; n],
            out_chunk: vec![0.0; Self::HOP],
            mag: vec![0.0; BINS],
            target: vec![1.0; BINS],
            target_smooth: vec![1.0; BINS],
            gain: vec![1.0; BINS],
            profile: vec![0.0; BINS],
            has_profile: false,
            learning: false,
            learn_total: 0,
            learn_done: 0,
            learn_count: 0,
            sum: vec![0.0; BINS],
            sum_sq: vec![0.0; BINS],
            max_reduction_db: 0.0,
        };
        gate.set_sample_rate(sample_rate);
        gate.set(3.0, 15.0, 40.0);
        gate
    }

    /// `threshold_db` (0..20): bin'in geçmesi için profilin kaç dB üstünde
    /// olması gerektiği; `reduction_db` (0..40): gürültü bin'lerine uygulanan
    /// bastırma; `smoothing_ms` (5..200): kazançların zaman yumuşatması
    /// (düşüş bu sürede, yükseliş ~10 kat daha hızlı). Aralık dışı değerler
    /// kırpılır, sonlu olmayanlar yok sayılır. Anlık geçişler güvenlidir.
    pub fn set(&mut self, threshold_db: f32, reduction_db: f32, smoothing_ms: f32) {
        if threshold_db.is_finite() {
            self.threshold_lin = db_to_lin(threshold_db.clamp(0.0, 20.0));
        }
        if reduction_db.is_finite() {
            self.reduction_lin = db_to_lin(-reduction_db.clamp(0.0, 40.0));
        }
        if smoothing_ms.is_finite() {
            self.smoothing_ms = smoothing_ms.clamp(5.0, 200.0);
            self.update_coefs();
        }
    }

    /// Öğrenmeyi başlatır: sonraki `seconds` saniyelik ses boyunca bin
    /// genlikleri ortalanır. Bu sürede ses değişmeden (gecikmeli) geçer.
    /// Önceki profil (varsa) öğrenme bitene kadar saklanır ama kullanılmaz.
    pub fn start_learning(&mut self, seconds: f32) {
        let secs = if seconds.is_finite() {
            seconds.clamp(0.0, 600.0)
        } else {
            0.0
        };
        let frames = (secs * self.sample_rate / Self::HOP as f32).round() as usize;
        self.learn_total = frames.max(1);
        self.learn_done = 0;
        self.learn_count = 0;
        self.sum.fill(0.0);
        self.sum_sq.fill(0.0);
        self.learning = true;
    }

    pub fn is_learning(&self) -> bool {
        self.learning
    }

    pub fn has_profile(&self) -> bool {
        self.has_profile
    }

    /// Mevcut profilin kopyası (`FFT_SIZE / 2 + 1` uzunlukta). RT dışı yardımcı.
    pub fn profile(&self) -> Option<Vec<f32>> {
        self.has_profile.then(|| self.profile.clone())
    }

    /// Profili `out`'a kopyalar; allocation yapmaz (ses thread'inden çağrılabilir).
    /// Profil yoksa veya uzunluk `FFT_SIZE / 2 + 1` değilse `false`.
    pub fn profile_into(&self, out: &mut [f32]) -> bool {
        if !self.has_profile || out.len() != BINS {
            return false;
        }
        out.copy_from_slice(&self.profile);
        true
    }

    /// Kaydedilmiş profili yükler. Uzunluk `FFT_SIZE / 2 + 1` değilse ya da
    /// değerlerden biri sonlu/negatif değilse `false` döner ve hiçbir şey
    /// değişmez. Başarıda süren bir öğrenme iptal edilir.
    pub fn set_profile(&mut self, profile: &[f32]) -> bool {
        if profile.len() != BINS || profile.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return false;
        }
        self.profile.copy_from_slice(profile);
        self.has_profile = true;
        self.learning = false;
        true
    }

    /// Profili siler (ve süren öğrenmeyi iptal eder); ses tekrar saf geçer.
    pub fn clear_profile(&mut self) {
        self.profile.fill(0.0);
        self.has_profile = false;
        self.learning = false;
    }

    /// Eklenen sabit gecikme (örnek): tam olarak `FFT_SIZE`.
    pub fn latency_samples(&self) -> usize {
        Self::FFT_SIZE
    }

    /// Son çağrıdan beri çerçeve başına `10·log10(giriş_enerji / çıkış_enerji)`
    /// değerlerinin maksimumu (≥ 0 dB). UI göstergesi için; birikimi sıfırlar.
    pub fn gain_reduction_db(&mut self) -> f32 {
        std::mem::take(&mut self.max_reduction_db)
    }

    fn update_coefs(&mut self) {
        let dt = Self::HOP as f32 / self.sample_rate;
        let tau_release = self.smoothing_ms * 1e-3;
        // Konuşma başlangıçları kırpılmasın: yükseliş çok daha hızlı.
        let tau_attack = (tau_release * 0.1).clamp(0.0005, 0.005);
        self.coef_release = (-dt / tau_release).exp();
        self.coef_attack = (-dt / tau_attack).exp();
    }

    /// Bir hop dolduğunda çağrılır: son `FFT_SIZE` örneği işleyip overlap-add
    /// birikimine ekler ve çıkışa verilecek `HOP` örneği hazırlar.
    fn process_frame(&mut self) {
        let n = Self::FFT_SIZE;

        // Pencerele (en eski örnek in_pos'ta) ve ileri FFT.
        let (newer, older) = self.in_ring.split_at(self.in_pos);
        for ((c, &x), &w) in self
            .fft_buf
            .iter_mut()
            .zip(older.iter().chain(newer.iter()))
            .zip(self.window.iter())
        {
            *c = Complex::new(x * w, 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.fft_buf, &mut self.scratch);

        for (m, c) in self.mag.iter_mut().zip(self.fft_buf.iter()) {
            *m = c.norm_sqr().sqrt();
        }

        // Hedef kazançlar.
        if self.learning {
            self.learn_step();
            self.target.fill(1.0);
        } else if self.has_profile {
            let (thr, red) = (self.threshold_lin, self.reduction_lin);
            for ((t, &m), &p) in self
                .target
                .iter_mut()
                .zip(self.mag.iter())
                .zip(self.profile.iter())
            {
                *t = if m < p * thr { red } else { 1.0 };
            }
        } else {
            self.target.fill(1.0);
        }

        // Frekans ekseninde hafif yumuşatma (1-2-1): müzikal gürültüyü azaltır.
        // Hepsi 1.0 iken sonuç tam 1.0'dır (mükemmel yeniden yapılandırma).
        let last = BINS - 1;
        for k in 0..BINS {
            let left = self.target[k.saturating_sub(1)];
            let right = self.target[(k + 1).min(last)];
            self.target_smooth[k] = (left + right) * 0.25 + self.target[k] * 0.5;
        }

        // Zaman yumuşatması, spektrumu kazançla çarpma, enerji ölçümü.
        let (ca, cr) = (self.coef_attack, self.coef_release);
        let mut in_energy = 0.0f32;
        let mut out_energy = 0.0f32;
        for k in 0..BINS {
            let tgt = self.target_smooth[k];
            let g = self.gain[k];
            let coef = if tgt > g { ca } else { cr };
            let g = (tgt + (g - tgt) * coef).clamp(0.0, 1.0);
            self.gain[k] = g;

            let m = self.mag[k];
            in_energy += m * m;
            out_energy += m * m * g * g;

            self.fft_buf[k] *= g;
            if k > 0 && k < last {
                // Eşlenik simetri korunur → gerçek çıkış.
                self.fft_buf[n - k] *= g;
            }
        }
        if in_energy > 1e-20 {
            let red = 10.0 * (in_energy / out_energy.max(1e-30)).log10();
            if red.is_finite() {
                self.max_reduction_db = self.max_reduction_db.max(red.max(0.0));
            }
        }

        // Ters FFT, sentez penceresi ve overlap-add (ölçek: 1 / (2·N)).
        self.ifft
            .process_with_scratch(&mut self.fft_buf, &mut self.scratch);
        let scale = 1.0 / (2.0 * n as f32);
        for ((o, c), &w) in self
            .ola
            .iter_mut()
            .zip(self.fft_buf.iter())
            .zip(self.window.iter())
        {
            *o += c.re * w * scale;
        }

        // İlk HOP örnek artık 4 çerçeveden toplandı → tamamlandı.
        self.out_chunk.copy_from_slice(&self.ola[..Self::HOP]);
        self.ola.copy_within(Self::HOP.., 0);
        self.ola[n - Self::HOP..].fill(0.0);
    }

    /// Öğrenme çerçevesi: genlikleri biriktirir, süre dolunca profili kurar.
    fn learn_step(&mut self) {
        // Çok kısa öğrenmelerde atlama yapılmaz.
        let skip = if self.learn_total > 2 * LEARN_SKIP_FRAMES {
            LEARN_SKIP_FRAMES
        } else {
            0
        };
        if self.learn_done >= skip {
            for ((s, s2), &m) in self
                .sum
                .iter_mut()
                .zip(self.sum_sq.iter_mut())
                .zip(self.mag.iter())
            {
                let m = f64::from(m);
                *s += m;
                *s2 += m * m;
            }
            self.learn_count += 1;
        }
        self.learn_done += 1;

        if self.learn_done >= self.learn_total {
            let count = self.learn_count.max(1) as f64;
            for ((p, &s), &s2) in self
                .profile
                .iter_mut()
                .zip(self.sum.iter())
                .zip(self.sum_sq.iter())
            {
                let mean = s / count;
                let var = (s2 / count - mean * mean).max(0.0);
                *p = (mean + PROFILE_STD_FACTOR * var.sqrt()) as f32;
            }
            self.has_profile = true;
            self.learning = false;
        }
    }
}

impl Processor for SpectralGate {
    fn process(&mut self, buf: &mut [f32]) {
        let mut pos = 0;
        while pos < buf.len() {
            // Hop sınırına (ya da tampon sonuna) kadar olan parça. HOP, FFT_SIZE'ı
            // böldüğünden `in_pos + n` halka sonunu aşmaz.
            let n = (Self::HOP - self.hop_pos).min(buf.len() - pos);
            let seg = &mut buf[pos..pos + n];
            let ring = &mut self.in_ring[self.in_pos..self.in_pos + n];
            let out = &self.out_chunk[self.hop_pos..self.hop_pos + n];
            for ((s, r), &o) in seg.iter_mut().zip(ring.iter_mut()).zip(out.iter()) {
                // NaN/Inf girişi FFT'yi zehirlemesin.
                *r = if s.is_finite() { *s } else { 0.0 };
                *s = o;
            }

            pos += n;
            self.hop_pos += n;
            self.in_pos += n;
            if self.in_pos == Self::FFT_SIZE {
                self.in_pos = 0;
            }
            if self.hop_pos == Self::HOP {
                self.hop_pos = 0;
                self.process_frame();
            }
        }
    }

    /// Ses durumunu (halkalar, overlap-add, kazançlar, ölçer) sıfırlar.
    /// Profil ve süren öğrenme korunur.
    fn reset(&mut self) {
        self.in_ring.fill(0.0);
        self.ola.fill(0.0);
        self.out_chunk.fill(0.0);
        self.gain.fill(1.0);
        self.in_pos = 0;
        self.hop_pos = 0;
        self.max_reduction_db = 0.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        if sample_rate.is_finite() && sample_rate > 0.0 {
            self.sample_rate = sample_rate;
            self.update_coefs();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;
    const LAT: usize = SpectralGate::FFT_SIZE;

    /// Küçük xorshift64* üretecinin (tekrarlanabilir testler için).
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        /// (0, 1) aralığında düzgün dağılım.
        fn uniform(&mut self) -> f64 {
            ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }

        /// Box-Muller ile standart normal.
        fn gauss(&mut self) -> f32 {
            let (u1, u2) = (self.uniform(), self.uniform());
            ((-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()) as f32
        }
    }

    fn noise(seed: u64, len: usize, sigma: f32) -> Vec<f32> {
        let mut rng = Rng(seed);
        (0..len).map(|_| rng.gauss() * sigma).collect()
    }

    fn sine(freq: f32, amp: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                amp * (std::f64::consts::TAU * f64::from(freq) * i as f64 / 48_000.0).sin() as f32
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn run_blocks(gate: &mut SpectralGate, input: &[f32], block: usize) -> Vec<f32> {
        let mut out = input.to_vec();
        for chunk in out.chunks_mut(block) {
            gate.process(chunk);
        }
        out
    }

    /// 1 kHz'deki sinüs genliği (tam sayıda periyot: `x.len()` 48'in katı olmalı).
    fn goertzel_amp(x: &[f32], freq: f64) -> f64 {
        let w = std::f64::consts::TAU * freq / f64::from(SR);
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            re += f64::from(v) * (w * i as f64).cos();
            im -= f64::from(v) * (w * i as f64).sin();
        }
        2.0 * (re * re + im * im).sqrt() / x.len() as f64
    }

    /// `center ± exclude_hz` dışındaki (100 Hz–20 kHz) bantta Hann pencereli
    /// güç toplamı; sinüsün kendisini dışarıda bırakarak gürültüyü ölçer.
    fn band_power_excluding(x: &[f32], center: f64, exclude_hz: f64) -> f64 {
        const LEN: usize = 16_384;
        assert!(x.len() >= LEN);
        let x = &x[x.len() - LEN..];
        let mut buf: Vec<Complex<f32>> = x
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / LEN as f32).cos();
                Complex::new(v * w, 0.0)
            })
            .collect();
        FftPlanner::<f32>::new()
            .plan_fft_forward(LEN)
            .process(&mut buf);
        let bin_hz = f64::from(SR) / LEN as f64;
        (0..LEN / 2)
            .filter(|&k| {
                let f = k as f64 * bin_hz;
                (100.0..20_000.0).contains(&f) && (f - center).abs() > exclude_hz
            })
            .map(|k| f64::from(buf[k].norm_sqr()))
            .sum()
    }

    /// `seconds` saniyelik sabit tohumlu gürültüyle öğrenmiş kapı.
    fn learned_gate(seconds: f32, sigma: f32) -> SpectralGate {
        let mut gate = SpectralGate::new(SR);
        gate.start_learning(seconds);
        let mut learn = noise(1, (seconds * SR) as usize, sigma);
        for chunk in learn.chunks_mut(256) {
            gate.process(chunk);
        }
        assert!(gate.has_profile() && !gate.is_learning());
        gate
    }

    #[test]
    fn no_profile_is_delayed_passthrough() {
        let mut rng = Rng(7);
        let input: Vec<f32> = (0..20_000)
            .map(|_| (rng.uniform() as f32 - 0.5) * 1.8)
            .collect();
        for block in [64usize, 256, 1000, 37] {
            let mut gate = SpectralGate::new(SR);
            assert_eq!(gate.latency_samples(), LAT);
            let out = run_blocks(&mut gate, &input, block);
            assert_eq!(gate.latency_samples(), LAT, "gecikme sabit olmalı");
            for (i, &o) in out.iter().enumerate().take(LAT) {
                assert!(
                    o.abs() < 1e-6,
                    "blok {block}: ilk gecikme sessiz olmalı, i={i}"
                );
            }
            let max_diff = out[LAT..]
                .iter()
                .zip(input.iter())
                .map(|(o, x)| (o - x).abs())
                .fold(0.0f32, f32::max);
            assert!(max_diff < 1e-4, "blok {block}: maks fark {max_diff}");
            assert_eq!(gate.gain_reduction_db(), 0.0);
        }
    }

    #[test]
    fn learned_noise_is_reduced_by_reduction_db() {
        let mut gate = learned_gate(3.0, 0.01);
        gate.set(6.0, 20.0, 30.0);

        let input = noise(2, 4 * 48_000, 0.01);
        let out = run_blocks(&mut gate, &input, 256);
        // Gecikmeyi ve yumuşatmanın oturmasını atla.
        let start = 48_000;
        let r_in = rms(&input[start..input.len() - LAT]);
        let r_out = rms(&out[start + LAT..]);
        let reduction = 20.0 * (r_in / r_out).log10();
        assert!(
            (15.0..=25.0).contains(&reduction),
            "azalma {reduction} dB (beklenen ≈ 20)"
        );
        let meter = gate.gain_reduction_db();
        assert!((10.0..=45.0).contains(&meter), "ölçer {meter} dB");
        assert_eq!(gate.gain_reduction_db(), 0.0, "okuma birikimi sıfırlar");
    }

    #[test]
    fn sine_passes_while_noise_is_reduced() {
        let mut gate = learned_gate(3.0, 0.01);
        gate.set(6.0, 20.0, 30.0);

        let len = 4 * 48_000;
        let tone = sine(1000.0, 0.316, len);
        let noise_part = noise(3, len, 0.01);
        let input: Vec<f32> = tone.iter().zip(&noise_part).map(|(a, b)| a + b).collect();
        let out = run_blocks(&mut gate, &input, 256);

        // Son 1 sn (48'in katı → tam periyot), gecikme telafili.
        let n = 48_000;
        let a_in = goertzel_amp(&input[len - LAT - n..len - LAT], 1000.0);
        let a_out = goertzel_amp(&out[len - n..], 1000.0);
        let loss_db = 20.0 * (a_in / a_out).log10();
        assert!(loss_db.abs() < 1.0, "1 kHz kaybı {loss_db} dB");

        let p_in = band_power_excluding(&input[..len - LAT], 1000.0, 1500.0);
        let p_out = band_power_excluding(&out[LAT..], 1000.0, 1500.0);
        let noise_red = 10.0 * (p_in / p_out).log10();
        assert!(
            noise_red > 10.0,
            "1 kHz dışı gürültü azalması {noise_red} dB"
        );
    }

    #[test]
    fn learning_state_and_profile_roundtrip() {
        let mut gate = SpectralGate::new(SR);
        assert!(!gate.is_learning() && !gate.has_profile());
        assert!(gate.profile().is_none());

        gate.start_learning(3.0);
        assert!(gate.is_learning());
        let mut input = noise(5, 3 * 48_000, 0.01);
        let (first, rest) = input.split_at_mut(48_000);
        for chunk in first.chunks_mut(256) {
            gate.process(chunk);
        }
        assert!(gate.is_learning(), "1 sn sonra hâlâ öğreniyor olmalı");
        assert!(!gate.has_profile());
        for chunk in rest.chunks_mut(256) {
            gate.process(chunk);
        }
        assert!(!gate.is_learning(), "3 sn sonra öğrenme bitmeli");
        assert!(gate.has_profile());

        let profile = gate.profile().expect("profil olmalı");
        assert_eq!(profile.len(), SpectralGate::FFT_SIZE / 2 + 1);
        assert!(profile.iter().all(|v| v.is_finite() && *v > 0.0));

        let mut other = SpectralGate::new(SR);
        assert!(other.set_profile(&profile));
        assert!(other.has_profile());
        assert_eq!(other.profile().as_deref(), Some(profile.as_slice()));

        // Geçersiz profiller reddedilir ve mevcut profili bozmaz.
        assert!(!other.set_profile(&profile[1..]));
        let mut bad = profile.clone();
        bad[10] = f32::NAN;
        assert!(!other.set_profile(&bad));
        bad[10] = f32::INFINITY;
        assert!(!other.set_profile(&bad));
        bad[10] = -1.0;
        assert!(!other.set_profile(&bad));
        assert_eq!(other.profile().as_deref(), Some(profile.as_slice()));

        other.clear_profile();
        assert!(!other.has_profile());
        assert!(other.profile().is_none());
    }

    #[test]
    fn learning_passes_audio_through() {
        let mut gate = SpectralGate::new(SR);
        gate.start_learning(1.0);
        let input = noise(9, 24_000, 0.1);
        let out = run_blocks(&mut gate, &input, 100);
        assert!(gate.is_learning());
        let max_diff = out[LAT..]
            .iter()
            .zip(input.iter())
            .map(|(o, x)| (o - x).abs())
            .fold(0.0f32, f32::max);
        assert!(max_diff < 1e-4, "öğrenirken ses değişmemeli: {max_diff}");
    }

    #[test]
    fn robust_with_extreme_input_and_abrupt_changes() {
        let mut gate = learned_gate(1.0, 0.01);
        let mut rng = Rng(11);
        for round in 0..400 {
            let len = 64 + (rng.next_u64() % 1000) as usize;
            let mut buf: Vec<f32> = (0..len)
                .map(|_| match rng.next_u64() % 4 {
                    0 => 1.0,
                    1 => -1.0,
                    _ => (rng.uniform() as f32 - 0.5) * 2.0,
                })
                .collect();
            if round % 50 == 7 {
                buf[len / 2] = f32::NAN;
                buf[len / 3] = f32::INFINITY;
            }
            gate.set(
                rng.uniform() as f32 * 25.0 - 2.0,
                rng.uniform() as f32 * 50.0 - 5.0,
                rng.uniform() as f32 * 250.0,
            );
            if round == 100 {
                gate.start_learning(0.2);
            }
            if round == 200 {
                gate.clear_profile();
            }
            if round == 300 {
                gate.set_sample_rate(44_100.0);
                gate.reset();
            }
            gate.process(&mut buf);
            assert!(buf.iter().all(|v| v.is_finite()), "tur {round}: NaN/Inf");
            let red = gate.gain_reduction_db();
            assert!(red.is_finite() && red >= 0.0, "tur {round}: ölçer {red}");
        }
        gate.set(f32::NAN, f32::INFINITY, f32::NAN);
        let mut buf = [0.5f32; 300];
        gate.process(&mut buf);
        assert!(buf.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn output_is_independent_of_block_size() {
        let len = 30_000;
        let tone = sine(700.0, 0.2, len);
        let input: Vec<f32> = noise(21, len, 0.02)
            .iter()
            .zip(&tone)
            .map(|(a, b)| a + b)
            .collect();

        let mut reference = learned_gate(1.0, 0.01);
        reference.set(4.0, 25.0, 20.0);
        let mut whole = input.clone();
        reference.process(&mut whole);
        let ref_meter = reference.gain_reduction_db();

        for block in [1usize, 7, 64, 127, 128, 129, 256, 1000, 8192] {
            let mut gate = learned_gate(1.0, 0.01);
            gate.set(4.0, 25.0, 20.0);
            let out = run_blocks(&mut gate, &input, block);
            assert_eq!(out, whole, "blok {block}: çıkış farklı");
            assert_eq!(gate.gain_reduction_db(), ref_meter, "blok {block}: ölçer");
        }
    }

    #[test]
    fn reset_clears_audio_state_but_keeps_profile() {
        let mut gate = learned_gate(1.0, 0.01);
        let mut loud = noise(4, 5_000, 0.5);
        gate.process(&mut loud);
        gate.reset();
        let mut silence = vec![0.0f32; 2_000];
        gate.process(&mut silence);
        assert!(silence.iter().all(|&v| v == 0.0));
        assert!(gate.has_profile());
    }
}
