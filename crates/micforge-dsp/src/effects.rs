//! Zaman tabanlı efektler: reverb (Freeverb), echo (delay) ve chorus.
//!
//! Üçü de mono çalışır ve [`Processor`] trait'ini uygular. Tamponlar `new`
//! içinde 96 kHz'e göre boyutlandırılır; `process` ve `reset` heap
//! ayırmaz, kilit almaz, log yazmaz ve panik üretmez. `set` çağrıları blok
//! başına bir kez, ses thread'inden yapılabilir: yalnızca skaler hesap yapar.

use std::f32::consts::TAU;

use crate::Processor;
use crate::smooth::Smoother;

/// Tamponların boyutlandırıldığı en yüksek örnekleme hızı (Hz).
const MAX_SR: f32 = 96_000.0;
/// Daha düşük / yüksek örnekleme hızları bu aralığa kenetlenir.
const MIN_SR: f32 = 8_000.0;

#[inline]
fn clamp_sr(sample_rate: f32) -> f32 {
    sample_rate.clamp(MIN_SR, MAX_SR)
}

/// Çok küçük değerleri sıfıra çeker; geri besleme döngülerinde denormal
/// sayıların CPU'yu yavaşlatmasını önler.
#[inline]
fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

/// Dairesel gecikme hattı. Kesirli gecikmeler doğrusal enterpolasyonla
/// okunur; böylece gecikme süresi değişirken tık sesi oluşmaz.
struct DelayLine {
    buf: Vec<f32>,
    write: usize,
}

impl DelayLine {
    fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0.0; capacity],
            write: 0,
        }
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.write = 0;
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.buf[self.write] = x;
        self.write += 1;
        if self.write == self.buf.len() {
            self.write = 0;
        }
    }

    /// Son yazılan örnekten `k` örnek önceki değer (`k = 0` → son örnek).
    /// `k` en fazla `len - 2` olmalıdır.
    #[inline]
    fn tap(&self, k: usize) -> f32 {
        let n = self.buf.len();
        let mut j = self.write + n - 1 - k;
        if j >= n {
            j -= n;
        }
        self.buf[j]
    }

    /// Son yazılan örneğe göre `d` örnek gecikmeli değeri döndürür
    /// (`d` kesirli olabilir, `[0, len - 2]` aralığına kenetlenir).
    #[inline]
    fn read(&self, d: f32) -> f32 {
        let d = d.clamp(0.0, (self.buf.len() - 2) as f32);
        let i = d as usize;
        let f = d - i as f32;
        (1.0 - f) * self.tap(i) + f * self.tap(i + 1)
    }
}

/// Freeverb tamponu: sabit bir boyda dairesel bellek. Uzunluk örnekleme
/// hızına göre `set_rate` ile ayarlanır; bellek hiç yeniden ayrılmaz.
struct Ring {
    buf: Vec<f32>,
    /// 44.1 kHz'deki (Freeverb orijinal) uzunluk.
    tuning: usize,
    len: usize,
    idx: usize,
}

/// Freeverb orijinal tasarımının referans örnekleme hızı.
const REF_SR: f32 = 44_100.0;

impl Ring {
    fn new(tuning: usize) -> Self {
        let cap = (tuning as f32 * MAX_SR / REF_SR).ceil() as usize + 1;
        Self {
            buf: vec![0.0; cap],
            tuning,
            len: cap,
            idx: 0,
        }
    }

    fn set_rate(&mut self, sample_rate: f32) {
        let len = (self.tuning as f32 * sample_rate / REF_SR).round() as usize;
        self.len = len.clamp(1, self.buf.len());
        if self.idx >= self.len {
            self.idx = 0;
        }
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
        self.idx = 0;
    }

    #[inline]
    fn current(&self) -> f32 {
        self.buf[self.idx]
    }

    #[inline]
    fn write_advance(&mut self, v: f32) {
        self.buf[self.idx] = v;
        self.idx += 1;
        if self.idx >= self.len {
            self.idx = 0;
        }
    }
}

/// Freeverb karşılaştırmalı filtresi: düşük geçiren geri beslemeli gecikme.
struct Comb {
    ring: Ring,
    /// Geri beslemedeki tek kutuplu alçak geçiren filtrenin durumu.
    store: f32,
}

impl Comb {
    fn new(tuning: usize) -> Self {
        Self {
            ring: Ring::new(tuning),
            store: 0.0,
        }
    }

    #[inline]
    fn tick(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.ring.current();
        self.store = flush(out * (1.0 - damp) + self.store * damp);
        self.ring.write_advance(flush(x + self.store * feedback));
        out
    }
}

/// Freeverb tümleşik geçiren (all-pass) filtresi; geri besleme sabit 0.5.
const ALLPASS_FEEDBACK: f32 = 0.5;

struct Allpass {
    ring: Ring,
}

impl Allpass {
    fn new(tuning: usize) -> Self {
        Self {
            ring: Ring::new(tuning),
        }
    }

    #[inline]
    fn tick(&mut self, x: f32) -> f32 {
        let buf_out = self.ring.current();
        self.ring
            .write_advance(flush(x + buf_out * ALLPASS_FEEDBACK));
        buf_out - x
    }
}

/// Freeverb'in orijinal comb uzunlukları (44.1 kHz).
const COMB_TUNINGS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// Freeverb'in orijinal all-pass uzunlukları (44.1 kHz).
const ALLPASS_TUNINGS: [usize; 4] = [556, 441, 341, 225];
/// Giriş kazancı (Freeverb `fixedgain`): comb'lara giden sinyal küçük kalsın.
const INPUT_GAIN: f32 = 0.015;
/// Islak çıkış kazancı. Freeverb'in `scalewet` değeri olan 3.0 yerine,
/// size 50'de ıslak RMS'in kuru RMS'e yakın kalması için ayarlandı
/// (bkz. `wet_level_is_comparable_to_dry_at_default_size` testi).
const WET_GAIN: f32 = 3.0;
const MAX_PREDELAY_MS: f32 = 200.0;

/// Oda boyutu (0..100) → comb geri besleme katsayısı (0.70..0.98).
#[inline]
fn room_feedback(size_pct: f32) -> f32 {
    0.7 + 0.28 * size_pct / 100.0
}

/// Sönümleme (0..100) → comb içindeki düşük geçiren katsayısı (0..0.4).
#[inline]
fn damp_coef(damping_pct: f32) -> f32 {
    0.4 * damping_pct / 100.0
}

/// Mono Freeverb reverb'ü: 8 paralel comb + 4 seri all-pass.
///
/// `set(size_pct, damping_pct, mix_pct, predelay_ms)`:
/// - `size_pct` (0..100): oda boyutu, kuyruğun uzunluğu.
/// - `damping_pct` (0..100): yüksek frekansların kuyrukta ne kadar çabuk sönümleneceği.
/// - `mix_pct` (0..100): kuru/ıslak oranı. Çıkış `dry*(1-m) + wet*m*WET_GAIN`.
/// - `predelay_ms` (0..200): ıslak yola giren gecikme.
pub struct Reverb {
    sample_rate: f32,
    combs: [Comb; 8],
    allpasses: [Allpass; 4],
    predelay: DelayLine,
    predelay_ms: f32,
    pre_samples: Smoother,
    feedback: Smoother,
    damp: Smoother,
    mix: Smoother,
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        let sr = clamp_sr(sample_rate);
        let mut r = Self {
            sample_rate: sr,
            combs: std::array::from_fn(|i| Comb::new(COMB_TUNINGS[i])),
            allpasses: std::array::from_fn(|i| Allpass::new(ALLPASS_TUNINGS[i])),
            predelay: DelayLine::new((MAX_PREDELAY_MS * MAX_SR / 1000.0) as usize + 4),
            predelay_ms: 0.0,
            pre_samples: Smoother::new(0.0, sr),
            feedback: Smoother::new(room_feedback(50.0), sr),
            damp: Smoother::new(damp_coef(50.0), sr),
            mix: Smoother::new(0.0, sr),
        };
        r.set_sample_rate(sr);
        r
    }

    /// Parametreleri ayarlar. Değişimler yumuşatılır (zipper gürültüsü yok).
    pub fn set(&mut self, size_pct: f32, damping_pct: f32, mix_pct: f32, predelay_ms: f32) {
        self.feedback
            .set_target(room_feedback(size_pct.clamp(0.0, 100.0)));
        self.damp
            .set_target(damp_coef(damping_pct.clamp(0.0, 100.0)));
        self.mix.set_target(mix_pct.clamp(0.0, 100.0) / 100.0);
        self.predelay_ms = predelay_ms.clamp(0.0, MAX_PREDELAY_MS);
        self.pre_samples
            .set_target(self.predelay_ms * self.sample_rate / 1000.0);
    }

    /// Kuru yolun gecikmesi yoktur; reverb yalnızca ıslak yolu geciktirir.
    pub fn latency_samples(&self) -> usize {
        0
    }
}

impl Processor for Reverb {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let x = *s;
            self.predelay.push(x);
            let input = self.predelay.read(self.pre_samples.next_value()) * INPUT_GAIN;
            let fb = self.feedback.next_value();
            let damp = self.damp.next_value();
            let m = self.mix.next_value();

            let mut wet = 0.0;
            for c in &mut self.combs {
                wet += c.tick(input, fb, damp);
            }
            for a in &mut self.allpasses {
                wet = a.tick(wet);
            }
            *s = x * (1.0 - m) + wet * (WET_GAIN * m);
        }
    }

    fn reset(&mut self) {
        for c in &mut self.combs {
            c.ring.clear();
            c.store = 0.0;
        }
        for a in &mut self.allpasses {
            a.ring.clear();
        }
        self.predelay.clear();
        self.pre_samples.snap(self.pre_samples.target());
        self.feedback.snap(self.feedback.target());
        self.damp.snap(self.damp.target());
        self.mix.snap(self.mix.target());
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = clamp_sr(sample_rate);
        self.sample_rate = sr;
        for c in &mut self.combs {
            c.ring.set_rate(sr);
        }
        for a in &mut self.allpasses {
            a.ring.set_rate(sr);
        }
        self.pre_samples.set_sample_rate(sr);
        self.feedback.set_sample_rate(sr);
        self.damp.set_sample_rate(sr);
        self.mix.set_sample_rate(sr);
        self.pre_samples.set_target(self.predelay_ms * sr / 1000.0);
    }
}

/// Tek kutuplu alçak geçiren katsayısı (`hz` kesim frekansı).
#[inline]
fn one_pole_coef(hz: f32, sample_rate: f32) -> f32 {
    1.0 - (-TAU * hz / sample_rate).exp()
}

/// Echo (gecikmeli geri besleme). Geri besleme yolunda tek kutuplu alçak
/// geçiren vardır; her tekrar biraz daha karanlık olur.
///
/// `set(time_ms, feedback_pct, mix_pct, high_cut_hz)`:
/// - `time_ms` (10..2000): gecikme süresi. Değişimde kayarak (glide) geçilir.
/// - `feedback_pct` (0..95): tekrarların geri besleme oranı (en fazla 0.95).
/// - `mix_pct` (0..100): echo seviyesi. Kuru sinyal değişmez:
///   `out = dry + wet*m`. Yani echo kuruya eklenir, onu bastırmaz.
/// - `high_cut_hz` (1000..20000): geri besleme yolundaki alçak geçiren kesimi.
pub struct Delay {
    sample_rate: f32,
    line: DelayLine,
    time_ms: f32,
    time_samples: Smoother,
    feedback: Smoother,
    mix: Smoother,
    high_cut_hz: f32,
    lp_coef: f32,
    lp_state: f32,
}

/// En uzun echo süresi (ms).
const MAX_DELAY_MS: f32 = 2_000.0;
/// Geri beslemenin üst sınırı; döngü kazancı 1'in altında kalsın.
const MAX_FEEDBACK: f32 = 0.95;

impl Delay {
    pub fn new(sample_rate: f32) -> Self {
        let sr = clamp_sr(sample_rate);
        let mut d = Self {
            sample_rate: sr,
            line: DelayLine::new((MAX_DELAY_MS * MAX_SR / 1000.0) as usize + 4),
            time_ms: 300.0,
            time_samples: Smoother::new(300.0 * sr / 1000.0, sr),
            feedback: Smoother::new(0.0, sr),
            mix: Smoother::new(0.0, sr),
            high_cut_hz: 8_000.0,
            lp_coef: 0.0,
            lp_state: 0.0,
        };
        d.set_sample_rate(sr);
        d
    }

    /// Parametreleri ayarlar (bkz. tür belgesi). Aralıklar kenetlenir.
    pub fn set(&mut self, time_ms: f32, feedback_pct: f32, mix_pct: f32, high_cut_hz: f32) {
        self.time_ms = time_ms.clamp(10.0, MAX_DELAY_MS);
        self.time_samples
            .set_target(self.time_ms * self.sample_rate / 1000.0);
        self.feedback
            .set_target((feedback_pct / 100.0).clamp(0.0, MAX_FEEDBACK));
        self.mix.set_target((mix_pct / 100.0).clamp(0.0, 1.0));
        self.high_cut_hz = high_cut_hz.clamp(1000.0, 20_000.0);
        self.lp_coef = one_pole_coef(self.high_cut_hz, self.sample_rate);
    }
}

impl Processor for Delay {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let x = *s;
            let d = self.time_samples.next_value();
            let fb = self.feedback.next_value();
            let m = self.mix.next_value();

            // Son yazılan örnek `x_{n-1}`; `d` gecikme için `d - 1` geriye bakılır.
            let wet = self.line.read(d - 1.0);
            self.lp_state = flush(self.lp_state + self.lp_coef * (wet - self.lp_state));
            self.line.push(flush(x + fb * self.lp_state));
            *s = x + wet * m;
        }
    }

    fn reset(&mut self) {
        self.line.clear();
        self.lp_state = 0.0;
        self.time_samples.snap(self.time_samples.target());
        self.feedback.snap(self.feedback.target());
        self.mix.snap(self.mix.target());
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = clamp_sr(sample_rate);
        self.sample_rate = sr;
        self.time_samples.set_sample_rate(sr);
        self.feedback.set_sample_rate(sr);
        self.mix.set_sample_rate(sr);
        self.time_samples.set_target(self.time_ms * sr / 1000.0);
        self.lp_coef = one_pole_coef(self.high_cut_hz, sr);
    }
}

/// Chorus'un sabit taban gecikmesi (ms).
const CHORUS_BASE_MS: f32 = 15.0;
/// Chorus derinliğinin üst sınırı (ms); hat bu değere göre boyutlanır.
const CHORUS_MAX_DEPTH_MS: f32 = 10.0;
const CHORUS_VOICES: usize = 3;

/// 3 seslik chorus: her ses sinüs LFO ile modüle edilmiş bir gecikme okur.
/// LFO fazları 0, 1/3, 2/3 çevrim farkıyla dağıtılır.
///
/// `set(rate_hz, depth_ms, mix_pct)`:
/// - `rate_hz` (0.1..5): LFO hızı.
/// - `depth_ms` (0.5..10): gecikme salınımının genliği (yumuşatılır).
/// - `mix_pct` (0..100): ıslak oran. Çıkış eşit karışımdır:
///   `out = dry*(1 - 0.5m) + wet*0.5m`, yani m=1 iken kuru ve ıslak yarı yarıya.
pub struct Chorus {
    sample_rate: f32,
    samples_per_ms: f32,
    line: DelayLine,
    rate_hz: f32,
    phase_inc: f32,
    /// LFO fazı, her zaman `[0, 1)` aralığında.
    phase: f32,
    depth: Smoother,
    mix: Smoother,
}

impl Chorus {
    pub fn new(sample_rate: f32) -> Self {
        let sr = clamp_sr(sample_rate);
        let mut c = Self {
            sample_rate: sr,
            samples_per_ms: sr / 1000.0,
            line: DelayLine::new(
                ((CHORUS_BASE_MS + CHORUS_MAX_DEPTH_MS) * MAX_SR / 1000.0) as usize + 4,
            ),
            rate_hz: 0.5,
            phase_inc: 0.0,
            phase: 0.0,
            depth: Smoother::new(2.0, sr),
            mix: Smoother::new(0.0, sr),
        };
        c.set_sample_rate(sr);
        c
    }

    /// Parametreleri ayarlar. Aralıklar kenetlenir.
    pub fn set(&mut self, rate_hz: f32, depth_ms: f32, mix_pct: f32) {
        self.rate_hz = rate_hz.clamp(0.1, 5.0);
        self.phase_inc = self.rate_hz / self.sample_rate;
        self.depth
            .set_target(depth_ms.clamp(0.5, CHORUS_MAX_DEPTH_MS));
        self.mix.set_target((mix_pct / 100.0).clamp(0.0, 1.0));
    }
}

impl Processor for Chorus {
    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let x = *s;
            self.line.push(x);
            let depth = self.depth.next_value();
            let m = self.mix.next_value();

            let mut wet = 0.0;
            for v in 0..CHORUS_VOICES {
                let p = (self.phase + v as f32 / CHORUS_VOICES as f32).fract();
                let delay_ms = CHORUS_BASE_MS + depth * 0.5 * (1.0 + (TAU * p).sin());
                wet += self.line.read(delay_ms * self.samples_per_ms);
            }
            // Sesler kısmen ilintisiz: güç toplamına göre ölçekle (1/√N).
            wet *= (CHORUS_VOICES as f32).sqrt().recip();

            self.phase = (self.phase + self.phase_inc).fract();
            *s = x * (1.0 - 0.5 * m) + wet * (0.5 * m);
        }
    }

    fn reset(&mut self) {
        self.line.clear();
        self.phase = 0.0;
        self.depth.snap(self.depth.target());
        self.mix.snap(self.mix.target());
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        let sr = clamp_sr(sample_rate);
        self.sample_rate = sr;
        self.samples_per_ms = sr / 1000.0;
        self.phase_inc = self.rate_hz / sr;
        self.depth.set_sample_rate(sr);
        self.mix.set_sample_rate(sr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f32 = 48_000.0;

    /// Tekrarlanabilir beyaz gürültü (xorshift), değerler [-1, 1].
    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed.max(1);
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                (s as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect()
    }

    fn sine(n: usize, freq: f32) -> Vec<f32> {
        (0..n).map(|i| (TAU * freq * i as f32 / FS).sin()).collect()
    }

    fn run<P: Processor>(p: &mut P, input: &[f32]) -> Vec<f32> {
        let mut out = input.to_vec();
        p.process(&mut out);
        out
    }

    fn silence<P: Processor>(p: &mut P, secs: f32) {
        let mut b = vec![0.0; (secs * FS) as usize];
        p.process(&mut b);
    }

    fn energy(x: &[f32]) -> f32 {
        x.iter().map(|v| v * v).sum()
    }

    fn rms(x: &[f32]) -> f32 {
        (energy(x) / x.len() as f32).sqrt()
    }

    fn argmax_abs(x: &[f32]) -> usize {
        x.iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    #[test]
    fn delay_mix_zero_is_exact_dry() {
        let mut d = Delay::new(FS);
        d.set(250.0, 80.0, 0.0, 5000.0);
        let x = noise(20_000, 7);
        assert_eq!(run(&mut d, &x), x);
    }

    #[test]
    fn reverb_mix_zero_is_transparent_after_settling() {
        let mut r = Reverb::new(FS);
        r.set(80.0, 30.0, 100.0, 20.0);
        silence(&mut r, 1.0);
        r.set(80.0, 30.0, 0.0, 20.0);
        let x = noise(FS as usize, 5);
        let y = run(&mut r, &x);
        for (a, b) in x.iter().zip(&y).skip(FS as usize / 2) {
            assert!((a - b).abs() < 1e-4, "{a} ≠ {b}");
        }
    }

    #[test]
    fn chorus_mix_zero_is_transparent_after_settling() {
        let mut c = Chorus::new(FS);
        c.set(0.5, 2.0, 100.0);
        silence(&mut c, 1.0);
        c.set(0.5, 2.0, 0.0);
        let x = noise(FS as usize, 5);
        let y = run(&mut c, &x);
        for (a, b) in x.iter().zip(&y).skip(FS as usize / 2) {
            assert!((a - b).abs() < 1e-4, "{a} ≠ {b}");
        }
    }

    #[test]
    fn delay_echo_timing_and_feedback_decay() {
        let mut d = Delay::new(FS);
        d.set(100.0, 50.0, 100.0, 20_000.0);
        // Süre ve geri besleme yumuşatması bitsin diye önce sessizlik.
        silence(&mut d, 0.5);
        let mut b = vec![0.0; 12_000];
        b[0] = 1.0;
        d.process(&mut b);

        let e1 = argmax_abs(&b[4_700..4_900]) + 4_700;
        assert!((e1 as i64 - 4_800).abs() <= 2, "ilk echo {e1}");
        assert!((b[e1] - 1.0).abs() < 0.01, "ilk echo genliği {}", b[e1]);

        let e2 = argmax_abs(&b[9_500..9_700]) + 9_500;
        assert!((e2 as i64 - 9_600).abs() <= 2, "ikinci echo {e2}");
        // Geri besleme %50, yüksek kesim 20 kHz'de ~0.93 kazanç: ~0.46.
        let ratio = b[e2] / b[e1];
        assert!((ratio - 0.5).abs() <= 0.5 * 0.15, "tekrar oranı {ratio}");
    }

    fn reverb_tail(size: f32) -> Vec<f32> {
        let mut r = Reverb::new(FS);
        r.set(size, 50.0, 100.0, 0.0);
        silence(&mut r, 0.3);
        let mut b = vec![0.0; 2 * FS as usize];
        b[0] = 1.0;
        r.process(&mut b);
        b
    }

    #[test]
    fn reverb_impulse_has_decaying_bounded_tail() {
        let b = reverb_tail(50.0);
        assert!(b.iter().all(|v| v.is_finite() && v.abs() < 10.0));
        let early = rms(&b[FS as usize / 20..FS as usize / 2]);
        let late = rms(&b[FS as usize * 3 / 2..]);
        assert!(early > 1e-4, "kuyruk yok: {early}");
        assert!(late < early * 0.01, "kuyruk sönmüyor: {late} / {early}");
    }

    #[test]
    fn reverb_larger_size_rings_longer() {
        let lo = energy(&reverb_tail(0.0)[FS as usize / 2..FS as usize]);
        let hi = energy(&reverb_tail(100.0)[FS as usize / 2..FS as usize]);
        assert!(hi > lo, "size 100 ({hi}) size 0'dan ({lo}) uzun değil");
    }

    #[test]
    fn wet_level_is_comparable_to_dry_at_default_size() {
        let mut r = Reverb::new(FS);
        r.set(50.0, 50.0, 100.0, 0.0);
        silence(&mut r, 0.3);
        let x = noise(2 * FS as usize, 11);
        let y = run(&mut r, &x);
        let ratio = rms(&y[FS as usize..]) / rms(&x[FS as usize..]);
        assert!((0.5..=2.0).contains(&ratio), "ıslak/kuru RMS oranı {ratio}");
    }

    #[test]
    fn chorus_1khz_output_differs_but_level_is_preserved() {
        let mut c = Chorus::new(FS);
        c.set(0.5, 2.0, 100.0);
        silence(&mut c, 0.3);
        let x = sine(4 * FS as usize, 1_000.0);
        let y = run(&mut c, &x);
        let (xs, ys) = (&x[FS as usize..], &y[FS as usize..]);
        assert!(
            xs.iter().zip(ys).any(|(a, b)| (a - b).abs() > 0.01),
            "çıkış girişle aynı"
        );
        let db = 20.0 * (rms(ys) / rms(xs)).log10();
        assert!(db.abs() <= 3.0, "seviye farkı {db:.2} dB");
    }

    /// Parametreleri min/max arasında değiştirerek, değişken blok boylarıyla
    /// beyaz gürültü işler; her çıkış örneğinin sonlu ve |x| < 10 olduğunu doğrular.
    fn stress<P: Processor>(p: &mut P, sr: f32, min: impl Fn(&mut P), max: impl Fn(&mut P)) {
        let x = noise((2.0 * sr) as usize, 0x1234_5678);
        let block_sizes = [256usize, 1, 1000, 333];
        let (mut pos, mut i, mut use_max) = (0usize, 0usize, false);
        while pos < x.len() {
            if i % 8 == 0 {
                if use_max {
                    max(p);
                } else {
                    min(p);
                }
                use_max = !use_max;
            }
            let n = block_sizes[i % block_sizes.len()].min(x.len() - pos);
            let mut b = x[pos..pos + n].to_vec();
            p.process(&mut b);
            assert!(
                b.iter().all(|v| v.is_finite() && v.abs() < 10.0),
                "sınır dışı çıkış (sr {sr}, blok {i})"
            );
            pos += n;
            i += 1;
        }
    }

    #[test]
    fn extreme_parameters_stay_finite_and_bounded() {
        for sr in [44_100.0, 96_000.0] {
            let mut r = Reverb::new(sr);
            stress(
                &mut r,
                sr,
                |p| p.set(0.0, 0.0, 0.0, 0.0),
                |p| p.set(100.0, 100.0, 100.0, 200.0),
            );
            let mut d = Delay::new(sr);
            stress(
                &mut d,
                sr,
                |p| p.set(10.0, 0.0, 0.0, 1000.0),
                |p| p.set(2000.0, 95.0, 100.0, 20_000.0),
            );
            let mut c = Chorus::new(sr);
            stress(
                &mut c,
                sr,
                |p| p.set(0.1, 0.5, 0.0),
                |p| p.set(5.0, 10.0, 100.0),
            );
        }
    }

    fn reset_clears_tail<P: Processor>(p: &mut P) {
        run(p, &noise(FS as usize / 2, 9));
        p.reset();
        let y = run(p, &vec![0.0; FS as usize / 2]);
        assert!(y.iter().all(|v| *v == 0.0), "reset sonrası kuyruk kaldı");
    }

    #[test]
    fn reset_clears_tails() {
        let mut r = Reverb::new(FS);
        r.set(100.0, 0.0, 100.0, 50.0);
        reset_clears_tail(&mut r);

        let mut d = Delay::new(FS);
        d.set(100.0, 90.0, 100.0, 20_000.0);
        reset_clears_tail(&mut d);

        let mut c = Chorus::new(FS);
        c.set(1.0, 5.0, 100.0);
        reset_clears_tail(&mut c);
    }
}
