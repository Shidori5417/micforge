//! Lookahead brickwall limiter.
//!
//! Tasarım (Signalsmith tarzı):
//! 1. Giriş `L = lookahead · fs` örnek geciktirilir.
//! 2. Her örnek için gereken kazanç `r[n] = min(1, tavan / |x[n]|)` hesaplanır.
//! 3. `r` üzerinde `L + 1` örneklik kayan **minimum** alınır (monoton deque,
//!    amortize O(1)). Böylece bir tepe, gecikme hattından çıkmadan önce
//!    kazanç zaten o tepeye göre düşmüş olur.
//! 4. Bırakma (release): tutulan zarf yükselirken tek kutuplu süzgeçle yavaş
//!    izlenir, düşerken anında izlenir.
//! 5. Atak, zarf üzerinde `L` uzunluklu kutu (hareketli ortalama) süzgeciyle
//!    yumuşatılır: kazanç tepeye doğru doğrusal inmiş olur, tık/bozulma yok.
//! 6. Kazanç gecikmiş örneğe uygulanır. Son güvenlik olarak çıkış tavana
//!    kırpılır; bu kırpma pratikte hiç devreye girmemelidir.
//!
//! İspat taslağı: gecikmiş tepe `x[p]` çıkarken (`n = p + L`) kutu penceresi
//! `e[p+1..=p+L]` aralığıdır; bu aralıktaki her `e[k]`, penceresi `p`'yi
//! içerdiğinden `r[p]`'den küçüktür. Bırakma süzgeci zarfı hiçbir zaman
//! tutulan minimumun üstüne çıkarmaz, dolayısıyla ortalama da `r[p]`'yi aşmaz.
//!
//! Gerçek zamanlı güvenlik: tüm tamponlar `new` içinde ayrılır, `process`
//! içinde heap, kilit, I/O ve panik yolu yoktur.

use crate::smooth::Smoother;
use crate::{Processor, db_to_lin};

/// En yüksek desteklenen örnekleme hızı (tampon boyutu buna göre ayrılır).
const MAX_SAMPLE_RATE: f64 = 192_000.0;
/// En uzun lookahead (ms).
const MAX_LOOKAHEAD_MS: f32 = 10.0;
/// En büyük gecikme (örnek): 10 ms @ 192 kHz.
const MAX_LOOKAHEAD: usize = 1920;
/// Halka tampon boyutu (2'nin kuvveti, `MAX_LOOKAHEAD + 1`'den büyük).
const RING: usize = 2048;
const MASK: usize = RING - 1;
const _: () = assert!(RING > MAX_LOOKAHEAD + 1 && RING.is_power_of_two());

/// Hareketli toplamın kayan nokta kaymasına karşı yeniden hesaplanma aralığı.
const RESYNC_INTERVAL: usize = 4096;
/// Tavana ulaşırken yuvarlama hatasının tavanı aşmaması için emniyet payı.
const SAFETY_MARGIN: f32 = 4.0e-7;
/// Gerekli kazancın alt sınırı (denormal ve sonsuz girişe karşı).
const MIN_REQUIRED_GAIN: f32 = 1.0e-12;
/// Bırakma zarfı hedefe bu kadar yaklaşınca yapışır (f32'de takılmasın).
const RELEASE_SNAP: f64 = 1.0e-6;

const DEFAULT_CEILING_DB: f32 = -1.0;
const DEFAULT_RELEASE_MS: f32 = 100.0;
const DEFAULT_LOOKAHEAD_MS: f32 = 5.0;

/// Lookahead brickwall limiter (mono).
pub struct Limiter {
    sample_rate: f32,
    ceiling_db: f32,
    release_ms: f32,
    lookahead_ms: f32,
    /// Tavan (doğrusal). Yalnızca yükselirken yumuşatılır; düşüşte etkin
    /// tavan hemen hedefe iner (lookahead sayesinde tıksız).
    ceiling: Smoother,
    /// f64: yavaş bırakma sürelerinde f32 aritmetiği 1.0'ın altında takılır.
    release_coef: f64,
    /// Gecikme (örnek), `1..=MAX_LOOKAHEAD`.
    lookahead: usize,

    /// Örnek sayacı (sarmalı); tüm halka indeksleri buradan türer.
    n: usize,
    /// Gecikme hattı.
    delay: Box<[f32]>,
    /// Her gecikmiş örneğin giriş anındaki tavanı (güvenlik kırpması için).
    ceil_ring: Box<[f32]>,
    /// Kayan minimum deque'i: örnek indeksi ve değer halkaları.
    dq_idx: Box<[usize]>,
    dq_val: Box<[f32]>,
    dq_head: usize,
    dq_tail: usize,
    /// Bırakma süzgecinden çıkan zarfın son `L` değeri (kutu süzgeci için).
    env_ring: Box<[f32]>,
    env: f64,
    /// Zarfın son `L` değerinin toplamı (f64: kayma önlemi).
    box_sum: f64,
    resync_count: usize,

    /// `gain_reduction_db` için son çağrıdan beri en düşük kazanç.
    min_gain: f32,
    /// Güvenlik kırpmasının devreye girdiği örnek sayısı.
    #[cfg_attr(not(test), allow(dead_code))]
    clamp_count: u64,
}

impl Limiter {
    pub fn new(sample_rate: f32) -> Self {
        let sample_rate = sanitize_rate(sample_rate);
        let mut l = Self {
            sample_rate,
            ceiling_db: DEFAULT_CEILING_DB,
            release_ms: DEFAULT_RELEASE_MS,
            lookahead_ms: DEFAULT_LOOKAHEAD_MS,
            ceiling: Smoother::new(db_to_lin(DEFAULT_CEILING_DB), sample_rate),
            release_coef: 0.0,
            lookahead: lookahead_samples(DEFAULT_LOOKAHEAD_MS, sample_rate),
            n: 0,
            delay: vec![0.0; RING].into_boxed_slice(),
            ceil_ring: vec![1.0; RING].into_boxed_slice(),
            dq_idx: vec![0; RING].into_boxed_slice(),
            dq_val: vec![1.0; RING].into_boxed_slice(),
            dq_head: 0,
            dq_tail: 0,
            env_ring: vec![1.0; RING].into_boxed_slice(),
            env: 1.0,
            box_sum: 0.0,
            resync_count: 0,
            min_gain: 1.0,
            clamp_count: 0,
        };
        l.update_release_coef();
        l.clear_state();
        l
    }

    /// `ceiling_db`: −24..0 dBFS; `release_ms`: 5..1000; `lookahead_ms`: 0.5..10.
    ///
    /// Blok başına çağrılabilir: değer değişmediyse neredeyse bedavadır ve
    /// durumu sıfırlamaz. Yalnızca lookahead (örnek cinsinden) değişirse
    /// gecikme hattı ve zarf sıfırlanır.
    pub fn set(&mut self, ceiling_db: f32, release_ms: f32, lookahead_ms: f32) {
        if ceiling_db.is_finite() {
            let db = ceiling_db.clamp(-24.0, 0.0);
            if db != self.ceiling_db {
                self.ceiling_db = db;
                self.ceiling.set_target(db_to_lin(db));
            }
        }
        if release_ms.is_finite() {
            let ms = release_ms.clamp(5.0, 1000.0);
            if ms != self.release_ms {
                self.release_ms = ms;
                self.update_release_coef();
            }
        }
        if lookahead_ms.is_finite() {
            self.lookahead_ms = lookahead_ms.clamp(0.5, MAX_LOOKAHEAD_MS);
            let l = lookahead_samples(self.lookahead_ms, self.sample_rate);
            if l != self.lookahead {
                self.lookahead = l;
                self.clear_state();
            }
        }
    }

    /// Eklenen gecikme (örnek) = lookahead gecikmesi.
    pub fn latency_samples(&self) -> usize {
        self.lookahead
    }

    /// Son çağrıdan beri en büyük kazanç düşüşü (pozitif dB); sayacı sıfırlar.
    pub fn gain_reduction_db(&mut self) -> f32 {
        let g = self.min_gain;
        self.min_gain = 1.0;
        if g < 1.0 {
            -20.0 * g.max(1.0e-6).log10()
        } else {
            0.0
        }
    }

    /// Güvenlik kırpmasının devreye girdiği örnek sayısı (yalnızca testler).
    #[cfg(test)]
    fn clamp_count(&self) -> u64 {
        self.clamp_count
    }

    fn update_release_coef(&mut self) {
        let tau = (f64::from(self.release_ms) * 1.0e-3 * f64::from(self.sample_rate)).max(1.0);
        self.release_coef = (-1.0 / tau).exp();
    }

    /// Gecikme hattı, deque ve zarf durumunu temizler (kazanç sayacına dokunmaz).
    fn clear_state(&mut self) {
        self.delay.fill(0.0);
        self.ceil_ring.fill(1.0);
        self.env_ring.fill(1.0);
        self.dq_head = 0;
        self.dq_tail = 0;
        self.n = 0;
        self.env = 1.0;
        self.box_sum = self.lookahead as f64;
        self.resync_count = 0;
    }
}

/// Geçersiz örnekleme hızlarını makul bir aralığa çeker.
fn sanitize_rate(sample_rate: f32) -> f32 {
    if sample_rate.is_finite() {
        sample_rate.clamp(1_000.0, MAX_SAMPLE_RATE as f32)
    } else {
        48_000.0
    }
}

/// Lookahead süresini (ms) gecikme örneğine çevirir: `1..=MAX_LOOKAHEAD`.
fn lookahead_samples(lookahead_ms: f32, sample_rate: f32) -> usize {
    let n = (f64::from(lookahead_ms) * 1.0e-3 * f64::from(sample_rate)).round() as usize;
    n.clamp(1, MAX_LOOKAHEAD)
}

impl Processor for Limiter {
    fn process(&mut self, buf: &mut [f32]) {
        let l = self.lookahead;
        let inv_l = 1.0 / l as f64;
        let target = self.ceiling.target();
        let release_coef = self.release_coef;

        for s in buf.iter_mut() {
            let n = self.n;

            // Etkin tavan: düşüşte anında, yükselişte yumuşatılmış.
            let smoothed = if self.ceiling.is_active() {
                self.ceiling.next_value()
            } else {
                self.ceiling.value()
            };
            let c = smoothed.min(target);

            // NaN/Inf girişi sessizliğe çevir (kazanç hesabını zehirlemesin).
            let x = if s.is_finite() { *s } else { 0.0 };
            let a = x.abs();

            // Bu örnek için gereken kazanç.
            let r = if a > c {
                (c * (1.0 - SAFETY_MARGIN) / a).max(MIN_REQUIRED_GAIN)
            } else {
                1.0
            };

            // Kayan minimum (monoton deque, artan değerler).
            while self.dq_tail != self.dq_head
                && self.dq_val[self.dq_tail.wrapping_sub(1) & MASK] >= r
            {
                self.dq_tail = self.dq_tail.wrapping_sub(1);
            }
            self.dq_idx[self.dq_tail & MASK] = n;
            self.dq_val[self.dq_tail & MASK] = r;
            self.dq_tail = self.dq_tail.wrapping_add(1);
            while n.wrapping_sub(self.dq_idx[self.dq_head & MASK]) > l {
                self.dq_head = self.dq_head.wrapping_add(1);
            }
            let held = f64::from(self.dq_val[self.dq_head & MASK]);

            // Bırakma: düşüş anında, yükseliş tek kutuplu.
            self.env = if held < self.env {
                held
            } else {
                let e = held + release_coef * (self.env - held);
                if held - e < RELEASE_SNAP { held } else { e }
            };

            // Atak yumuşatma: son `L` zarf değerinin ortalaması.
            let env_f32 = self.env as f32;
            let old = self.env_ring[n.wrapping_sub(l) & MASK];
            self.env_ring[n & MASK] = env_f32;
            self.box_sum += f64::from(env_f32) - f64::from(old);
            self.resync_count += 1;
            if self.resync_count >= RESYNC_INTERVAL {
                self.resync_count = 0;
                let mut sum = 0.0f64;
                for k in 0..l {
                    sum += f64::from(self.env_ring[n.wrapping_sub(k) & MASK]);
                }
                self.box_sum = sum;
            }
            let g = ((self.box_sum * inv_l) as f32).clamp(0.0, 1.0);
            if g < self.min_gain {
                self.min_gain = g;
            }

            // Gecikmiş örneği oku, yenisini yaz, kazancı uygula.
            let slot = n.wrapping_sub(l) & MASK;
            let delayed = self.delay[slot];
            let delayed_ceil = self.ceil_ring[slot];
            self.delay[n & MASK] = x;
            self.ceil_ring[n & MASK] = c;

            let mut y = delayed * g;
            if y.abs() > delayed_ceil {
                // Güvenlik kırpması: normalde asla çalışmamalı.
                self.clamp_count += 1;
                y = y.min(delayed_ceil).max(-delayed_ceil);
            }
            *s = y;
            self.n = n.wrapping_add(1);
        }
    }

    fn reset(&mut self) {
        self.clear_state();
        self.ceiling.snap(self.ceiling.target());
        self.min_gain = 1.0;
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sanitize_rate(sample_rate);
        self.ceiling.set_sample_rate(self.sample_rate);
        self.update_release_coef();
        let l = lookahead_samples(self.lookahead_ms, self.sample_rate);
        if l != self.lookahead {
            self.lookahead = l;
            self.clear_state();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    const FS: f32 = 48_000.0;

    /// Deterministik [-1, 1) rastgele sayı üreteci (xorshift32).
    struct Rng(u32);

    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    fn sine(amp: f32, freq: f32, len: usize, start: usize) -> Vec<f32> {
        (start..start + len)
            .map(|i| amp * (TAU * freq * i as f32 / FS).sin())
            .collect()
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn run(lim: &mut Limiter, input: &[f32], block: usize) -> Vec<f32> {
        let mut out = input.to_vec();
        for chunk in out.chunks_mut(block) {
            lim.process(chunk);
        }
        out
    }

    /// Tavanı aşması gereken üç test işareti.
    fn stress_signals() -> Vec<(&'static str, Vec<f32>)> {
        let mut rng = Rng(0x1234_5678);
        let noise: Vec<f32> = (0..24_000).map(|_| rng.next() * 8.0).collect();

        // Aralarında +20 dB sıçramalar olan sinüs patlamaları.
        let mut bursts = Vec::new();
        let mut pos = 0;
        for amp in [0.05f32, 0.5, 5.0, 0.05, 0.5, 5.0, 0.005, 0.5, 5.0] {
            bursts.extend(sine(amp, 1000.0, 2400, pos));
            pos += 2400;
        }

        // Tek örneklik, genliği 10 olan darbeler (iki işaret).
        let mut impulses = vec![0.0f32; 12_000];
        for (k, i) in (0..12_000).step_by(997).enumerate() {
            impulses[i] = if k % 2 == 0 { 10.0 } else { -10.0 };
        }
        // Zayıf arka plan gürültüsü de olsun.
        for v in impulses.iter_mut() {
            *v += rng.next() * 0.01;
        }

        vec![("noise", noise), ("bursts", bursts), ("impulses", impulses)]
    }

    #[test]
    fn never_exceeds_ceiling() {
        let mut total_clamps = 0u64;
        let mut total_samples = 0u64;
        for ceiling_db in [-1.0f32, -6.0, -12.0] {
            let ceiling = db_to_lin(ceiling_db);
            for lookahead_ms in [0.5f32, 2.0, 10.0] {
                for (name, sig) in stress_signals() {
                    let mut lim = Limiter::new(FS);
                    lim.set(ceiling_db, 100.0, lookahead_ms);
                    let out = run(&mut lim, &sig, 480);
                    let p = peak(&out);
                    assert!(
                        p <= ceiling + 1e-6,
                        "{name} tavan {ceiling_db} dB, lookahead {lookahead_ms} ms: \
                         tepe {p} > {ceiling}"
                    );
                    let clamps = lim.clamp_count();
                    assert!(
                        clamps * 1000 < out.len() as u64,
                        "{name}: kırpma çok sık: {clamps}/{}",
                        out.len()
                    );
                    total_clamps += clamps;
                    total_samples += out.len() as u64;
                }
            }
        }
        println!("güvenlik kırpması: {total_clamps} / {total_samples} örnek");
        assert_eq!(total_clamps, 0);
    }

    #[test]
    fn transparent_below_ceiling() {
        let mut lim = Limiter::new(FS);
        lim.set(-6.0, 100.0, 5.0);
        let input = sine(0.1, 1000.0, 48_000, 0); // −20 dBFS
        let out = run(&mut lim, &input, 256);
        let lat = lim.latency_samples();
        let mut max_diff = 0.0f32;
        for (n, &y) in out.iter().enumerate() {
            let expected = if n >= lat { input[n - lat] } else { 0.0 };
            max_diff = max_diff.max((y - expected).abs());
        }
        assert!(max_diff < 1e-5, "fark {max_diff}");
        assert_eq!(lim.gain_reduction_db(), 0.0);
    }

    #[test]
    fn steady_sine_hits_ceiling_with_low_distortion() {
        let mut lim = Limiter::new(FS);
        lim.set(-6.0, 200.0, 5.0);
        let input = sine(1.0, 440.0, 96_000, 0);
        let settle = 24_000;
        let mut out = input.clone();
        let (head, tail) = out.split_at_mut(settle);
        lim.process(head);
        lim.gain_reduction_db(); // yerleşme dönemini at
        lim.process(tail);

        let gr = lim.gain_reduction_db();
        assert!((gr - 6.0).abs() < 0.3, "gain reduction {gr}");

        let out_tail = &out[settle..];
        let p_db = 20.0 * peak(out_tail).log10();
        assert!((p_db + 6.0).abs() < 0.1, "tepe {p_db} dBFS");

        // Çıkış = sabit kazanç · (gecikmiş giriş) olmalı.
        let lat = lim.latency_samples();
        let delayed: Vec<f32> = (settle..input.len()).map(|n| input[n - lat]).collect();
        let k: f32 = out_tail
            .iter()
            .zip(&delayed)
            .map(|(o, d)| o * d)
            .sum::<f32>()
            / delayed.iter().map(|d| d * d).sum::<f32>();
        let err: Vec<f32> = out_tail
            .iter()
            .zip(&delayed)
            .map(|(o, d)| o - k * d)
            .collect();
        let rel = rms(&err) / rms(out_tail);
        assert!(rel < 0.01, "bağıl RMS hatası {rel}");
    }

    #[test]
    fn latency_matches_lookahead() {
        let mut lim = Limiter::new(FS);
        for ms in [0.5f32, 1.0, 2.0, 5.0, 7.3, 10.0] {
            lim.set(-6.0, 100.0, ms);
            let expected = (f64::from(ms) * 1e-3 * f64::from(FS)).round() as usize;
            assert_eq!(lim.latency_samples(), expected, "{ms} ms");
        }
        // Sınır dışı değerler kenetlenir.
        lim.set(-6.0, 100.0, 100.0);
        assert_eq!(lim.latency_samples(), 480);
        lim.set(-6.0, 100.0, 0.0);
        assert_eq!(lim.latency_samples(), 24);
        // 192 kHz'de tampon taşmaz.
        let mut hi = Limiter::new(192_000.0);
        hi.set(-6.0, 100.0, 10.0);
        assert_eq!(hi.latency_samples(), 1920);
        let mut buf = vec![5.0f32; 8192];
        hi.process(&mut buf);
        assert!(
            buf.iter()
                .all(|v| v.is_finite() && v.abs() <= db_to_lin(-6.0) + 1e-6)
        );
    }

    #[test]
    fn release_recovers_gain() {
        let release_ms = 50.0f32;
        let look_ms = 2.0f32;
        let mut lim = Limiter::new(FS);
        lim.set(-6.0, release_ms, look_ms);
        let lat = lim.latency_samples();

        let burst_len = 4800;
        let quiet_len = 48_000;
        let mut input = sine(4.0, 1000.0, burst_len, 0);
        input.extend(sine(0.1, 1000.0, quiet_len, burst_len));
        let out = run(&mut lim, &input, 128);

        // Çıkıştaki anlık kazanç = çıkış / gecikmiş giriş.
        let gain_at = |n: usize| -> f32 {
            // |giriş| küçük olan sıfır geçişlerinden kaçınmak için pencere ortalaması.
            let (mut num, mut den) = (0.0f32, 0.0f32);
            for m in n..n + 48 {
                num += out[m] * input[m - lat];
                den += input[m - lat] * input[m - lat];
            }
            num / den
        };
        let rel = (release_ms * 1e-3 * FS) as usize;
        let after = burst_len + lat;
        assert!(gain_at(after + rel / 5) < 0.9, "bırakma çok hızlı");
        let g = gain_at(after + 5 * rel);
        assert!(g >= 0.9, "5×release sonrası kazanç {g}");

        // Uzun sessizlikten sonra kazanç tam 1.0'a oturmalı.
        let mut tail = sine(0.1, 1000.0, 4800, burst_len + quiet_len);
        lim.gain_reduction_db();
        lim.process(&mut tail);
        assert_eq!(lim.gain_reduction_db(), 0.0);
    }

    #[test]
    fn robust_under_abrupt_parameter_changes() {
        let mut rng = Rng(0xDEAD_BEEF);
        let mut lim = Limiter::new(FS);
        let ceilings = [-24.0f32, -12.0, -6.0, -1.0, 0.0];
        let releases = [5.0f32, 50.0, 300.0, 1000.0];
        let lookaheads = [0.5f32, 2.0, 5.0, 10.0];
        let mut buf = vec![0.0f32; 2048];
        for block in 0..400 {
            if block % 3 == 0 {
                let pick = |len: usize, r: f32| ((r * 0.5 + 0.5) * len as f32) as usize % len;
                lim.set(
                    ceilings[pick(ceilings.len(), rng.next())],
                    releases[pick(releases.len(), rng.next())],
                    lookaheads[pick(lookaheads.len(), rng.next())],
                );
            }
            let len = 1 + ((rng.next() * 0.5 + 0.5) * 2047.0) as usize;
            let chunk = &mut buf[..len];
            for v in chunk.iter_mut() {
                *v = rng.next() * 3.0;
            }
            lim.process(chunk);
            for &v in chunk.iter() {
                assert!(v.is_finite(), "blok {block}: sonlu olmayan çıkış");
                assert!(v.abs() <= 1.0 + 1e-6, "blok {block}: |y| = {v}");
            }
            assert!(lim.gain_reduction_db().is_finite());
        }
    }

    #[test]
    fn non_finite_input_is_neutralised() {
        let mut lim = Limiter::new(FS);
        lim.set(-6.0, 100.0, 2.0);
        let mut buf = vec![0.5f32; 1000];
        buf[10] = f32::NAN;
        buf[20] = f32::INFINITY;
        buf[30] = f32::NEG_INFINITY;
        lim.process(&mut buf);
        assert!(buf.iter().all(|v| v.is_finite()));
    }

    /// `input`'u `chunk` boyutlu bloklarla işler; her blokta `set` çağrılır,
    /// ortada tavan yükseltilir (yumuşatma yolunu da kapsasın).
    fn run_with_sets(input: &[f32], chunk: usize) -> Vec<f32> {
        let mid = input.len() / 2;
        let mut lim = Limiter::new(FS);
        let mut out = input.to_vec();
        let mut pos = 0;
        while pos < out.len() {
            let end = if pos < mid {
                (pos + chunk).min(mid)
            } else {
                (pos + chunk).min(out.len())
            };
            let ceiling_db = if pos < mid { -12.0 } else { -3.0 };
            lim.set(ceiling_db, 80.0, 3.0);
            lim.process(&mut out[pos..end]);
            pos = end;
        }
        out
    }

    #[test]
    fn block_size_independent() {
        let mut rng = Rng(42);
        let input: Vec<f32> = (0..48_000).map(|_| rng.next() * 2.0).collect();
        let whole = run_with_sets(&input, 24_000);
        for chunk in [64usize, 1000, 3] {
            let blocks = run_with_sets(&input, chunk);
            assert_eq!(whole, blocks, "blok boyutu {chunk}");
        }
    }

    #[test]
    fn lookahead_change_resets_but_same_values_do_not() {
        let mut lim = Limiter::new(FS);
        lim.set(-6.0, 100.0, 2.0);
        let mut a = vec![0.1f32; 200];
        lim.process(&mut a);
        // Aynı değerler: gecikme hattındaki 0.1'ler korunmalı.
        lim.set(-6.0, 100.0, 2.0);
        let mut b = vec![0.1f32; 10];
        lim.process(&mut b);
        assert!(b.iter().all(|&v| (v - 0.1).abs() < 1e-6));
        // Lookahead değişirse gecikme hattı temizlenir (ilk çıkışlar 0).
        lim.set(-6.0, 100.0, 4.0);
        let mut c = vec![0.1f32; 10];
        lim.process(&mut c);
        assert!(c.iter().all(|&v| v == 0.0));
    }
}
