//! RNNoise gürültü engelleme (`nnnoiseless`, RNNoise'un saf Rust portu).
//!
//! `nnnoiseless` 48 kHz'de ve 480 örneklik çerçevelerle çalışır. Bu modül,
//! keyfi boyutlu blok akışını sabit gecikmeli FIFO'larla çerçevelere çevirir
//! ve kuru/ıslak (wet/dry) karışımı aynı gecikmeyle hizalar.
//!
//! Gecikme yapısı (testlerde ölçülerek doğrulanmıştır):
//! - Giriş FIFO'su: bir çerçeve (480 örnek) dolana kadar beklenir.
//! - RNNoise çıkışı bir çerçeve geriden gelir (çerçeveler %50 örtüşür), yani
//!   çıkış örneği `i`, girişin `i - (2 * FRAME_SIZE)` örneğine karşılık gelir.
//! - Ölçüm: çapraz korelasyon wet çıkışın girişi tam `958` örnek geciktirdiğini
//!   gösterdi (2 örnek erken, RNNoise'un pencere hizalaması). Kuru yol bu
//!   değerle geciktirilir: [`LATENCY`] = 958 örnek (≈ 20 ms @ 48 kHz).
//!
//! Örnekleme hızı her zaman 48 kHz kabul edilir; [`Processor::set_sample_rate`]
//! bu yüzden no-op'tur.

use nnnoiseless::DenoiseState;

use crate::Processor;
use crate::smooth::Smoother;

/// RNNoise çerçeve boyutu (örnek).
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// `nnnoiseless` 16-bit ölçek bekler: ±1.0 float ↔ ±32768.
const SCALE: f32 = 32768.0;
/// Wet FIFO ön doldurması: giriş çerçevesi (FRAME) + RNNoise'un bir çerçevelik
/// çıkış gecikmesi (FRAME). Zamanlama için gereklidir (nedensellik).
const WET_PREFILL: usize = 2 * FRAME;
/// Wet çıkışın girişe göre öndeliği (örnek, ölçüldü): wet[j] ≈ x[j + 2].
const RNNOISE_LEAD: usize = 2;
/// Ölçülmüş sabit toplam gecikme; kuru yol bu kadar geciktirilir.
const LATENCY: usize = WET_PREFILL - RNNOISE_LEAD;
/// FIFO kapasitesi (2'nin kuvveti). Gerekli en büyük doluluk ~ WET_PREFILL + FRAME.
const FIFO_CAP: usize = 2048;
const FIFO_MASK: usize = FIFO_CAP - 1;
/// Örnekleme hızı (Hz). Sabit: bu modül her zaman 48 kHz'de çalışır.
const SAMPLE_RATE: f32 = 48_000.0;

/// Sabit kapasiteli dairesel FIFO. Heap kullanmaz.
struct Fifo {
    buf: [f32; FIFO_CAP],
    head: usize,
    len: usize,
}

impl Fifo {
    /// `n` sıfırla dolu bir FIFO üretir (gecikme hattı ön doldurması).
    fn zeros(n: usize) -> Self {
        Self {
            buf: [0.0; FIFO_CAP],
            head: 0,
            len: n,
        }
    }

    /// FIFO'yu `n` sıfırla yeniden doldurur.
    fn refill_zeros(&mut self, n: usize) {
        self.buf.fill(0.0);
        self.head = 0;
        self.len = n;
    }

    /// Sona ekler. Kapasite dolarsa en eski örneği atar (panic yok).
    #[inline]
    fn push(&mut self, v: f32) {
        let idx = (self.head + self.len) & FIFO_MASK;
        self.buf[idx] = v;
        if self.len == FIFO_CAP {
            self.head = (self.head + 1) & FIFO_MASK;
        } else {
            self.len += 1;
        }
    }

    /// Baştan bir örnek çıkarır. Boşsa 0 döner (panic yok).
    #[inline]
    fn pop(&mut self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        let v = self.buf[self.head];
        self.head = (self.head + 1) & FIFO_MASK;
        self.len -= 1;
        v
    }
}

/// RNNoise tabanlı gürültü engelleyici. Mono, 48 kHz.
pub struct Rnnoise {
    state: Box<DenoiseState<'static>>,
    /// Çerçeve girişi (RNNoise ölçeğinde, ±32768).
    frame_in: [f32; FRAME],
    frame_fill: usize,
    frame_out: [f32; FRAME],
    /// İşlenmiş (wet) örnekler; ön doldurma `WET_PREFILL` sıfırdır.
    wet: Fifo,
    /// Kuru sinyal; `wet` ile aynı gecikmede okunur.
    dry: Fifo,
    /// Wet oranı (0..1), zipper gürültüsünü önlemek için yumuşatılır.
    mix: Smoother,
    vad: f32,
    /// İlk çerçevenin çıkışı RNNoise'un ısınma artefaktıdır; atılır.
    warmup_pending: bool,
}

impl Default for Rnnoise {
    fn default() -> Self {
        Self::new()
    }
}

impl Rnnoise {
    /// Tüm tamponları ayırır. Gerçek zamanlı thread'de değil, kurulumda çağrılmalı.
    /// Varsayılan karışım %100 (tam gürültü engelleme).
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            frame_in: [0.0; FRAME],
            frame_fill: 0,
            frame_out: [0.0; FRAME],
            wet: Fifo::zeros(WET_PREFILL),
            dry: Fifo::zeros(LATENCY),
            mix: Smoother::new(1.0, SAMPLE_RATE),
            vad: 0.0,
            warmup_pending: true,
        }
    }

    /// Wet/dry karışımı: 0 = kuru (gecikmeli giriş), 100 = tam gürültü engelleme.
    /// Değişim yumuşatılır; sonlu olmayan değerler yok sayılır.
    pub fn set_amount(&mut self, amount_percent: f32) {
        if amount_percent.is_finite() {
            self.mix
                .set_target(amount_percent.clamp(0.0, 100.0) / 100.0);
        }
    }

    /// Sabit ekleme gecikmesi (örnek). Arayüzde göstermek için.
    pub fn latency_samples(&self) -> usize {
        LATENCY
    }

    /// Son ses-aktivite olasılığı (0..1). Arayüz göstergesi için.
    pub fn vad(&self) -> f32 {
        self.vad
    }

    /// Dolu çerçeveyi RNNoise'a verir ve çıkışını wet FIFO'ya ekler.
    fn run_frame(&mut self) {
        self.frame_fill = 0;
        let vad = self
            .state
            .process_frame(&mut self.frame_out, &self.frame_in);
        self.vad = if vad.is_finite() {
            vad.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if self.warmup_pending {
            // İlk çıkış ısınma artefaktı; ön dolduruluşla hizalı tutmak için atılır.
            self.warmup_pending = false;
            return;
        }
        for &v in &self.frame_out {
            self.wet.push(if v.is_finite() { v / SCALE } else { 0.0 });
        }
    }
}

impl Processor for Rnnoise {
    /// Her çağrıda tam olarak `buf.len()` örnek üretir. Heap kullanmaz.
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let x = *s;

            self.dry.push(x);
            let dry = self.dry.pop();

            let input = if x.is_finite() {
                x.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            self.frame_in[self.frame_fill] = input * SCALE;
            self.frame_fill += 1;
            if self.frame_fill == FRAME {
                self.run_frame();
            }

            let wet = self.wet.pop();
            let a = self.mix.next_value();
            let out = dry * (1.0 - a) + wet * a;
            *s = if out.is_finite() { out } else { 0.0 };
        }
    }

    /// İç durumu sıfırlar (RNNoise geçmişi, FIFO'lar, VAD). Karışım oranı korunur.
    /// Not: RNNoise durumunu yeniden ayırdığı için bu çağrı heap kullanır;
    /// ses thread'inin dışında çağırın.
    fn reset(&mut self) {
        self.state = DenoiseState::new();
        self.frame_in = [0.0; FRAME];
        self.frame_fill = 0;
        self.wet.refill_zeros(WET_PREFILL);
        self.dry.refill_zeros(LATENCY);
        self.vad = 0.0;
        self.warmup_pending = true;
    }

    /// No-op: modül her zaman 48 kHz'de çalışır (RNNoise'un yerel hızı).
    fn set_sample_rate(&mut self, _sample_rate: f32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministik, hızlı sahte rastgele sayı üreteci (xorshift32). `[-1, 1)` aralığında.
    struct Rng(u32);

    impl Rng {
        fn next_unit(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    /// `n` örneklik beyaz gürültü, tepe genliği `amp`.
    fn white_noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut rng = Rng(seed);
        (0..n).map(|_| rng.next_unit() * amp).collect()
    }

    /// Sesli benzeri sinyal: 150 Hz harmonikleri, yavaş zarf ve küçük gürültü.
    fn voiced(n: usize) -> Vec<f32> {
        let mut rng = Rng(12345);
        (0..n)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE;
                let mut s = 0.0;
                for k in 1..=8 {
                    s += (2.0 * std::f32::consts::PI * 150.0 * k as f32 * t).sin() / k as f32;
                }
                let env = 0.6 + 0.4 * (2.0 * std::f32::consts::PI * 3.0 * t).sin();
                0.12 * env * s + 0.005 * rng.next_unit()
            })
            .collect()
    }

    /// `input`'u `block` boyutlu parçalar halinde işler ve sonucu döndürür.
    fn run(rn: &mut Rnnoise, input: &[f32], block: usize) -> Vec<f32> {
        let mut out = input.to_vec();
        for chunk in out.chunks_mut(block) {
            rn.process(chunk);
        }
        out
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn max_step(x: &[f32]) -> f32 {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn amount_zero_is_pure_delay() {
        let input = white_noise(48_000, 0.5, 7);
        let lat = Rnnoise::new().latency_samples();
        for block in [64, 256, 1000, 37] {
            let mut rn = Rnnoise::new();
            rn.set_amount(0.0);
            // Karışım oranı 1.0'dan 0'a yumuşar; sonrasını karşılaştır.
            let out = run(&mut rn, &input, block);
            let settled = input.len() / 2;
            let max_diff = (settled..input.len())
                .map(|i| (out[i] - input[i - lat]).abs())
                .fold(0.0, f32::max);
            assert!(
                max_diff < 1e-6,
                "blok {block}: gecikmeli kopyadan sapma {max_diff}"
            );
        }
    }

    #[test]
    fn latency_matches_measured_alignment() {
        let n = 48_000;
        let input = voiced(n);
        let mut rn = Rnnoise::new();
        let out = run(&mut rn, &input, 256);
        let lat = rn.latency_samples();
        let from = n / 2;

        // Çapraz korelasyonla wet çıkışın girişle en iyi hizalandığı gecikmeyi bul.
        let mut best = (0usize, f32::MIN);
        for cand in (lat - 40)..=(lat + 40) {
            let corr: f32 = (from..n).map(|i| out[i] * input[i - cand]).sum();
            if corr > best.1 {
                best = (cand, corr);
            }
        }
        assert_eq!(best.0, lat, "ölçülen hizalama ≠ latency_samples()");
    }

    /// Renkli (pembe benzeri) sabit gürültü, RMS `target_rms` (−30 dBFS ≈ 0.0316).
    fn pink_noise(n: usize, target_rms: f32, seed: u32) -> Vec<f32> {
        let mut rng = Rng(seed);
        let mut y = 0.0;
        let raw: Vec<f32> = (0..n)
            .map(|_| {
                y = 0.95 * y + rng.next_unit();
                y
            })
            .collect();
        let scale = target_rms / rms(&raw);
        raw.iter().map(|v| v * scale).collect()
    }

    #[test]
    fn stationary_noise_is_attenuated() {
        let n = 72_000; // 1.5 s
        let input = pink_noise(n, 0.0316, 99); // −30 dBFS
        let mut rn = Rnnoise::new();
        rn.set_amount(100.0);
        let out = run(&mut rn, &input, 256);
        let from = 24_000; // ilk 0.5 s hariç
        let in_db = 20.0 * rms(&input[from..]).log10();
        let out_db = 20.0 * rms(&out[from..]).log10();
        assert!(
            out_db <= in_db - 10.0,
            "çıkış {out_db:.1} dBFS, giriş {in_db:.1} dBFS: en az 10 dB düşüş bekleniyor"
        );
    }

    /// Beyaz gürültüde RNNoise belirgin şekilde zayıflatmaz: ölçülen düşüş
    /// yaklaşık 4–5 dB (VAD ≈ 0.38). Bu, davranışı kilitleyen bir regresyon testidir.
    #[test]
    fn white_noise_attenuation_is_modest() {
        let n = 72_000;
        let amp = 0.0548; // uniform [-a, a] → RMS a/√3 ≈ 0.0316 (−30 dBFS)
        let input = white_noise(n, amp, 99);
        let mut rn = Rnnoise::new();
        let out = run(&mut rn, &input, 256);
        let from = 24_000;
        let drop_db = 20.0 * (rms(&out[from..]) / rms(&input[from..])).log10();
        assert!(
            (-8.0..=-3.0).contains(&drop_db),
            "beyaz gürültü düşüşü {drop_db:.1} dB (beklenen ≈ −4..−5 dB)"
        );
    }

    #[test]
    fn no_nan_or_inf_and_vad_in_range() {
        let mut rng = Rng(4242);
        let n = 48_000;
        let input: Vec<f32> = (0..n)
            .map(|i| match i % 7 {
                0 => 1.0,
                1 => -1.0,
                _ => rng.next_unit(),
            })
            .collect();
        let mut rn = Rnnoise::new();
        for chunk in input.chunks(256) {
            let mut buf = chunk.to_vec();
            rn.process(&mut buf);
            assert!(buf.iter().all(|v| v.is_finite()), "NaN/Inf çıkış");
            assert!(
                (0.0..=1.0).contains(&rn.vad()),
                "vad aralık dışı: {}",
                rn.vad()
            );
        }
    }

    #[test]
    fn mid_stream_amount_change_is_smooth() {
        let n = 96_000; // 2 s
        let input = voiced(n);
        let mut rn = Rnnoise::new();
        let mut out = Vec::with_capacity(n);
        let mut buf = input.clone();
        for (i, chunk) in buf.chunks_mut(256).enumerate() {
            // Geçiş: 1.0 s'de %100 → %0, sonra %50 → %100.
            let t = i * 256;
            if t == 48_000 {
                rn.set_amount(0.0);
            }
            if t == 72_000 {
                rn.set_amount(50.0);
            }
            if t == 84_000 {
                rn.set_amount(100.0);
            }
            rn.process(chunk);
            out.extend_from_slice(chunk);
        }
        let in_step = max_step(&input);
        let out_step = max_step(&out[47_000..]);
        assert!(
            out_step <= in_step,
            "karışım geçişinde sıçrama: çıkış {out_step} > giriş {in_step}"
        );
    }

    #[test]
    fn output_length_and_block_size_independent() {
        let input = voiced(20_000);
        let reference = run(&mut Rnnoise::new(), &input, input.len());
        assert_eq!(reference.len(), input.len());
        for block in [64, 256, 37] {
            let out = run(&mut Rnnoise::new(), &input, block);
            assert_eq!(out.len(), input.len(), "blok {block}: uzunluk");
            assert!(
                out.iter().zip(&reference).all(|(a, b)| a == b),
                "blok {block}: tek tampon ile farklı çıkış"
            );
        }
    }
}
