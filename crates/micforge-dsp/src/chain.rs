//! İşleme zinciri: giriş → high-pass → RNNoise → spektral kapı → gate → expander → ton → Bass Boost →
//! parametrik EQ → compressor → de-esser → pitch → ses değiştirici → chorus → delay → reverb → çıkış kazancı → limiter → denge.
//! Tüm bellek `new` içinde ayrılır; `process` gerçek zamanlı güvenlidir.

use crate::Processor;
use crate::biquad::{Coeffs, FilterType, butterworth_qs};
use crate::dynamics::{Compressor, DeEsser, Expander, Gate};
use crate::effects::{Chorus, Delay, Reverb};
use crate::eq::{BASS_BOOST_Q, BassBoost, ParametricEq, band_response_db, kind_from_index};
use crate::limiter::Limiter;
use crate::modules::{
    BASS_FREQ, ChannelMode, HighPass, InputStage, MID_FREQ, Output, TREBLE_FREQ, Tone,
};
use crate::params::{DEFAULT_ORDER, EQ_BANDS, EqField, MODULE_COUNT, ParamId, ParamStore};
use crate::pitch::{self, PitchShifter, VoiceChanger};
use crate::rnnoise::Rnnoise;
use crate::spectral_gate::SpectralGate;

/// Yeniden sıralanabilir modüllerin kalıcı anahtarları (config/preset), varsayılan sırayla.
pub const MODULE_KEYS: [&str; MODULE_COUNT] = [
    "hpf",
    "rnnoise",
    "spectral_gate",
    "gate",
    "expander",
    "tone",
    "bass_boost",
    "eq",
    "compressor",
    "deesser",
    "pitch",
    "voice",
    "chorus",
    "delay",
    "reverb",
];

/// Modüllerin arayüzde görünen adları (MODULE_KEYS sırasıyla).
pub const MODULE_NAMES: [&str; MODULE_COUNT] = [
    "High-pass",
    "RNNoise",
    "Spektral kapı",
    "Noise gate",
    "Expander",
    "Ton (Bass/Mid/Tiz)",
    "Bass Boost",
    "Parametrik EQ",
    "Compressor",
    "De-esser",
    "Pitch shift",
    "Ses değiştirici",
    "Chorus",
    "Delay / Echo",
    "Reverb",
];

/// `MODULE_NAMES` İngilizce karşılıkları (aynı sıra).
pub const MODULE_NAMES_EN: [&str; MODULE_COUNT] = [
    "High-pass",
    "RNNoise",
    "Spectral gate",
    "Noise gate",
    "Expander",
    "Tone (Bass/Mid/Treble)",
    "Bass Boost",
    "Parametric EQ",
    "Compressor",
    "De-esser",
    "Pitch shift",
    "Voice changer",
    "Chorus",
    "Delay / Echo",
    "Reverb",
];

/// Her modülün aç/kapa parametresi (MODULE_KEYS sırasıyla).
pub const MODULE_TOGGLES: [ParamId; MODULE_COUNT] = [
    ParamId::HpfOn,
    ParamId::RnnoiseOn,
    ParamId::SpecGateOn,
    ParamId::GateOn,
    ParamId::ExpOn,
    ParamId::ToneOn,
    ParamId::BassBoostOn,
    ParamId::EqOn,
    ParamId::CompOn,
    ParamId::DeessOn,
    ParamId::PitchOn,
    ParamId::VoiceOn,
    ParamId::ChorusOn,
    ParamId::DelayOn,
    ParamId::ReverbOn,
];

pub struct Chain {
    /// İşlem sırası (MODULE_KEYS indeksleri).
    order: [u8; MODULE_COUNT],
    input: InputStage,
    hpf: HighPass,
    hpf_on: bool,
    tone: Tone,
    tone_on: bool,
    boost: BassBoost,
    boost_on: bool,
    eq: ParametricEq,
    eq_on: bool,
    rnnoise: Rnnoise,
    rn_on: bool,
    spectral: SpectralGate,
    sg_on: bool,
    /// Spektral kapı kapalıyken profil öğrenmek için kopya tampon.
    scratch: Vec<f32>,
    gate: Gate,
    gate_on: bool,
    expander: Expander,
    exp_on: bool,
    comp: Compressor,
    comp_on: bool,
    deess: DeEsser,
    deess_on: bool,
    pitch: PitchShifter,
    pitch_on: bool,
    voice: VoiceChanger,
    voice_on: bool,
    chorus: Chorus,
    chorus_on: bool,
    delay: Delay,
    delay_on: bool,
    reverb: Reverb,
    reverb_on: bool,
    limiter: Limiter,
    lim_on: bool,
    output: Output,
    channel: ChannelMode,
}

impl Chain {
    pub fn new(sample_rate: f32, params: &ParamStore) -> Self {
        let mut c = Self {
            order: DEFAULT_ORDER,
            input: InputStage::new(sample_rate),
            hpf: HighPass::new(sample_rate, params.get(ParamId::HpfFreq)),
            hpf_on: false,
            tone: Tone::new(sample_rate),
            tone_on: false,
            boost: BassBoost::new(sample_rate),
            boost_on: false,
            eq: ParametricEq::new(sample_rate),
            eq_on: false,
            rnnoise: Rnnoise::new(),
            rn_on: false,
            spectral: SpectralGate::new(sample_rate),
            sg_on: false,
            scratch: vec![0.0; 8192],
            gate: Gate::new(sample_rate),
            gate_on: false,
            expander: Expander::new(sample_rate),
            exp_on: false,
            comp: Compressor::new(sample_rate),
            comp_on: false,
            deess: DeEsser::new(sample_rate),
            deess_on: false,
            pitch: PitchShifter::new(sample_rate),
            pitch_on: false,
            voice: VoiceChanger::new(sample_rate),
            voice_on: false,
            chorus: Chorus::new(sample_rate),
            chorus_on: false,
            delay: Delay::new(sample_rate),
            delay_on: false,
            reverb: Reverb::new(sample_rate),
            reverb_on: false,
            limiter: Limiter::new(sample_rate),
            lim_on: false,
            output: Output::new(sample_rate),
            channel: ChannelMode::Average,
        };
        c.update(params);
        c.input.reset();
        c.hpf.reset();
        c.tone.reset();
        c.boost.reset();
        c.eq.reset();
        c.gate.reset();
        c.expander.reset();
        c.comp.reset();
        c.deess.reset();
        c.pitch.reset();
        c.voice.reset();
        c.chorus.reset();
        c.delay.reset();
        c.reverb.reset();
        c.limiter.reset();
        c
    }

    /// Parametreleri depodan okur (blok başına bir kez). Allocation yok.
    pub fn update(&mut self, p: &ParamStore) {
        use ParamId::*;
        self.channel = ChannelMode::from_index(p.get(InputChannel));
        self.order = p.order();
        self.input.set(p.get(InputGain), p.on(InputInvert));

        let on = p.on(HpfOn);
        if on && !self.hpf_on {
            self.hpf.reset();
        }
        self.hpf_on = on;
        self.hpf.set(p.get(HpfFreq), p.get(HpfSlope) as usize);

        let on = p.on(ToneOn);
        if on && !self.tone_on {
            self.tone.reset();
        }
        self.tone_on = on;
        self.tone.set(p.get(Bass), p.get(Mid), p.get(Treble));

        let on = p.on(BassBoostOn);
        if on && !self.boost_on {
            self.boost.reset();
        }
        self.boost_on = on;
        self.boost.set(p.get(BassBoostFreq), p.get(BassBoostAmount));

        self.eq_on = p.on(EqOn);
        for b in 0..EQ_BANDS {
            let f = |field| p.get(Eq(b as u8, field));
            self.eq.set_band(
                b,
                f(EqField::On) >= 0.5,
                kind_from_index(f(EqField::Kind)),
                f(EqField::Freq),
                f(EqField::Q),
                f(EqField::Gain),
                f(EqField::Slope) as usize,
            );
        }

        // RNNoise `reset` bellek ayırır: ses thread'inde çağrılmaz, durum sürer.
        self.rn_on = p.on(RnnoiseOn);
        self.rnnoise.set_amount(p.get(RnnoiseAmount));
        let on = p.on(SpecGateOn);
        if on && !self.sg_on {
            self.spectral.reset();
        }
        self.sg_on = on;
        self.spectral.set(
            p.get(SpecGateThreshold),
            p.get(SpecGateReduction),
            p.get(SpecGateSmoothing),
        );

        // Kapalıyken açılan dinamik modüller eski durumla başlamasın.
        let on = p.on(GateOn);
        if on && !self.gate_on {
            self.gate.reset();
        }
        self.gate_on = on;
        self.gate.set(
            p.get(GateThreshold),
            p.get(GateHysteresis),
            p.get(GateAttack),
            p.get(GateHold),
            p.get(GateRelease),
            p.get(GateRange),
        );
        let on = p.on(ExpOn);
        if on && !self.exp_on {
            self.expander.reset();
        }
        self.exp_on = on;
        self.expander.set(
            p.get(ExpThreshold),
            p.get(ExpRatio),
            p.get(ExpAttack),
            p.get(ExpRelease),
            p.get(ExpRange),
        );
        let on = p.on(CompOn);
        if on && !self.comp_on {
            self.comp.reset();
        }
        self.comp_on = on;
        self.comp.set(
            p.get(CompThreshold),
            p.get(CompRatio),
            p.get(CompKnee),
            p.get(CompAttack),
            p.get(CompRelease),
            p.get(CompMakeup),
            p.get(CompScHpf),
        );
        let on = p.on(DeessOn);
        if on && !self.deess_on {
            self.deess.reset();
        }
        self.deess_on = on;
        self.deess.set(
            p.get(DeessFreq),
            p.get(DeessBandwidth),
            p.get(DeessThreshold),
            p.get(DeessRange),
        );
        // Efektler: kapalıyken açılınca eski kuyruk (reverb/echo) duyulmasın.
        // `reset` yalnızca tamponları sıfırlar, bellek ayırmaz.
        self.pitch_on =
            cfg!(feature = "pitch") && switch(p.on(PitchOn), self.pitch_on, &mut self.pitch);
        self.pitch
            .set(p.get(PitchSemitones), p.get(PitchCents), p.get(PitchMix));
        self.voice.set(
            pitch::VoiceMode::from_index(p.get(VoiceMode)),
            p.get(VoiceIntensity),
            p.get(VoiceMix),
        );
        self.voice_on = switch(p.on(VoiceOn), self.voice_on, &mut self.voice);
        self.chorus
            .set(p.get(ChorusRate), p.get(ChorusDepth), p.get(ChorusMix));
        self.chorus_on = switch(p.on(ChorusOn), self.chorus_on, &mut self.chorus);
        self.delay.set(
            p.get(DelayTime),
            p.get(DelayFeedback),
            p.get(DelayMix),
            p.get(DelayHighCut),
        );
        self.delay_on = switch(p.on(DelayOn), self.delay_on, &mut self.delay);
        self.reverb.set(
            p.get(ReverbSize),
            p.get(ReverbDamping),
            p.get(ReverbMix),
            p.get(ReverbPredelay),
        );
        self.reverb_on = switch(p.on(ReverbOn), self.reverb_on, &mut self.reverb);

        let on = p.on(LimOn);
        if on && !self.lim_on {
            self.limiter.reset();
        }
        self.lim_on = on;
        self.limiter
            .set(p.get(LimCeiling), p.get(LimRelease), p.get(LimLookahead));

        self.output.set(p.get(OutputGain), p.get(OutputPan));
    }

    /// Ses thread'inin ilk çağrısında bir kez çağrılır: RNNoise'un thread'e
    /// özel FFT hazırlığı (tek seferlik allocation) oturum ortasında değil
    /// başlangıçta olsun.
    pub fn warm_up(&mut self) {
        let mut silence = [0.0f32; 960];
        self.rnnoise.process(&mut silence);
    }

    /// Gürültü profili öğrenmeyi başlatır (spektral kapı).
    pub fn start_noise_learning(&mut self, seconds: f32) {
        self.spectral.start_learning(seconds);
    }

    pub fn noise_learning(&self) -> bool {
        self.spectral.is_learning()
    }

    pub fn has_noise_profile(&self) -> bool {
        self.spectral.has_profile()
    }

    /// Kayıtlı profili yükler (allocation yok, kopyalar).
    pub fn set_noise_profile(&mut self, profile: &[f32]) -> bool {
        self.spectral.set_profile(profile)
    }

    /// Profili `out`'a kopyalar (allocation yok). Profil yoksa `false`.
    pub fn noise_profile_into(&self, out: &mut [f32]) -> bool {
        self.spectral.profile_into(out)
    }

    /// RNNoise ses aktivitesi olasılığı (0..1); kapalıyken 0.
    pub fn vad(&self) -> f32 {
        if self.rn_on { self.rnnoise.vad() } else { 0.0 }
    }

    /// İşlemcilerin son çağrıdan beri en büyük kazanç azaltması (pozitif dB):
    /// [gate, expander, compressor, de-esser, limiter, spektral kapı].
    pub fn gain_reductions(&mut self) -> [f32; 6] {
        let gr = |on: bool, v: f32| if on { v } else { 0.0 };
        [
            gr(self.gate_on, self.gate.gain_reduction_db()),
            gr(self.exp_on, self.expander.gain_reduction_db()),
            gr(self.comp_on, self.comp.gain_reduction_db()),
            gr(self.deess_on, self.deess.gain_reduction_db()),
            gr(self.lim_on, self.limiter.gain_reduction_db()),
            gr(self.sg_on, self.spectral.gain_reduction_db()),
        ]
    }

    /// Zincirin eklediği gecikme (örnek): limiter lookahead'i.
    pub fn latency_samples(&self) -> usize {
        let l = |on: bool, n: usize| if on { n } else { 0 };
        l(self.lim_on, self.limiter.latency_samples())
            + l(self.rn_on, self.rnnoise.latency_samples())
            + l(self.sg_on, self.spectral.latency_samples())
            + l(self.pitch_on, self.pitch.latency_samples())
            + l(self.voice_on, self.voice.latency_samples())
    }

    #[inline]
    pub fn channel_mode(&self) -> ChannelMode {
        self.channel
    }

    /// Mono tamponu işler (çıkış kazancı ve limiter dahil, denge hariç).
    pub fn process(&mut self, buf: &mut [f32]) {
        // Giriş kademesi her zaman başta; çıkış kazancı ve limiter her zaman
        // sonda (tavan güvencesi). Aradaki modüller kullanıcı sırasıyla.
        self.input.process(buf);
        for i in 0..MODULE_COUNT {
            self.run_module(self.order[i], buf);
        }
        self.output.apply_gain(buf);
        if self.lim_on {
            self.limiter.process(buf);
        }
    }

    /// Tek bir modülü çalıştırır; kapalı (bypass) modüller CPU harcamaz.
    fn run_module(&mut self, module: u8, buf: &mut [f32]) {
        match module {
            0 if self.hpf_on => self.hpf.process(buf),
            1 if self.rn_on => self.rnnoise.process(buf),
            2 if self.sg_on => self.spectral.process(buf),
            2 if self.spectral.is_learning() => {
                // Kapalıyken de profil öğrenilebilsin: kopyada çalıştır, sonucu at.
                for chunk in buf.chunks(self.scratch.len()) {
                    let s = &mut self.scratch[..chunk.len()];
                    s.copy_from_slice(chunk);
                    self.spectral.process(s);
                }
            }
            3 if self.gate_on => self.gate.process(buf),
            4 if self.exp_on => self.expander.process(buf),
            5 if self.tone_on => self.tone.process(buf),
            6 if self.boost_on => self.boost.process(buf),
            7 if self.eq_on => self.eq.process(buf),
            8 if self.comp_on => self.comp.process(buf),
            9 if self.deess_on => self.deess.process(buf),
            10 if self.pitch_on => self.pitch.process(buf),
            11 if self.voice_on => self.voice.process(buf),
            12 if self.chorus_on => self.chorus.process(buf),
            13 if self.delay_on => self.delay.process(buf),
            14 if self.reverb_on => self.reverb.process(buf),
            _ => {}
        }
    }

    /// İşlenmiş mono örneği dengeyle stereo'ya çevirir.
    #[inline]
    pub fn output(&mut self, x: f32) -> (f32, f32) {
        self.output.tick(x)
    }
}

/// Kapalıyken açılan modülü sıfırlar; yeni açık durumunu döndürür.
fn switch(on: bool, was_on: bool, p: &mut impl Processor) -> bool {
    if on && !was_on {
        p.reset();
    }
    on
}

/// Filtre modüllerinin (HPF, ton, Bass Boost, EQ) hedef değerlerle toplam
/// genlik tepkisi (dB). Kazanç/denge hariç. UI'daki EQ eğrisi için; RT değil.
pub fn response_db(p: &ParamStore, freq: f32, sample_rate: f32) -> f32 {
    use ParamId::*;
    let mut db = 0.0;
    if p.on(HpfOn) {
        let n = [2, 4, 8][(p.get(HpfSlope) as usize).min(2)];
        for q in butterworth_qs(n) {
            db += Coeffs::new(FilterType::HighPass, sample_rate, p.get(HpfFreq), q, 0.0)
                .magnitude_db(freq, sample_rate);
        }
    }
    if p.on(ToneOn) {
        let s = std::f32::consts::FRAC_1_SQRT_2;
        for (kind, f0, q, g) in [
            (FilterType::LowShelf, BASS_FREQ, s, p.get(Bass)),
            (FilterType::Peak, MID_FREQ, 0.7, p.get(Mid)),
            (FilterType::HighShelf, TREBLE_FREQ, s, p.get(Treble)),
        ] {
            if g != 0.0 {
                db += Coeffs::new(kind, sample_rate, f0, q, g).magnitude_db(freq, sample_rate);
            }
        }
    }
    if p.on(BassBoostOn) {
        db += Coeffs::new(
            FilterType::LowShelf,
            sample_rate,
            p.get(BassBoostFreq),
            BASS_BOOST_Q,
            p.get(BassBoostAmount),
        )
        .magnitude_db(freq, sample_rate);
    }
    if p.on(EqOn) {
        for b in 0..EQ_BANDS {
            let f = |field| p.get(Eq(b as u8, field));
            if f(EqField::On) < 0.5 {
                continue;
            }
            db += band_response_db(
                kind_from_index(f(EqField::Kind)),
                sample_rate,
                f(EqField::Freq),
                f(EqField::Q),
                f(EqField::Gain),
                f(EqField::Slope) as usize,
                freq,
            );
        }
    }
    db
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_store_roundtrip_and_validation() {
        let p = ParamStore::default();
        assert_eq!(p.order(), DEFAULT_ORDER);
        let o = [14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
        assert!(p.set_order(&o));
        assert_eq!(p.order(), o);
        assert!(!p.set_order(&[0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]));
        assert_eq!(p.order(), o);
    }

    #[test]
    fn order_changes_result() {
        // Doğrusal olmayan bir çift: compressor ve +12 dB bass. Sıra sonucu değiştirmeli.
        let make = |order: [u8; MODULE_COUNT]| {
            let p = ParamStore::default();
            p.set(ParamId::LimOn, 0.0);
            p.set(ParamId::CompOn, 1.0);
            p.set(ParamId::CompThreshold, -30.0);
            p.set(ParamId::CompRatio, 10.0);
            p.set(ParamId::Bass, 12.0);
            p.set_order(&order);
            let mut c = Chain::new(48_000.0, &p);
            let mut buf: Vec<f32> = (0..48_000)
                .map(|i| 0.1 * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / 48_000.0).sin())
                .collect();
            for ch in buf.chunks_mut(256) {
                c.process(ch);
            }
            buf[24_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()))
        };
        let tone_first = make([5, 0, 1, 2, 3, 4, 6, 7, 8, 9, 10, 11, 12, 13, 14]);
        let comp_first = make([8, 0, 1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14]);
        // Ton önce: compressor yükseltilmiş sinyali bastırır → daha düşük çıkış.
        assert!(
            comp_first > tone_first * 1.5,
            "sıra etkisiz: {tone_first} vs {comp_first}"
        );
    }

    #[test]
    fn defaults_are_transparent() {
        let p = ParamStore::default();
        // Limiter varsayılan açık ve lookahead gecikmesi ekler; burada kapat.
        p.set(ParamId::LimOn, 0.0);
        let mut c = Chain::new(48_000.0, &p);
        let mut buf: Vec<f32> = (0..1024).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect();
        let orig = buf.clone();
        c.process(&mut buf);
        for (a, b) in buf.iter().zip(&orig) {
            assert!((a - b).abs() < 1e-6);
            let (l, r) = c.output(*a);
            assert!((l - a).abs() < 1e-6 && (r - a).abs() < 1e-6);
        }
        assert!(response_db(&p, 1000.0, 48_000.0).abs() < 1e-6);
    }

    #[test]
    fn response_matches_processing() {
        let p = ParamStore::default();
        p.set(ParamId::Eq(4, EqField::Gain), 8.0);
        p.set(ParamId::Eq(4, EqField::Freq), 500.0);
        p.set(ParamId::BassBoostOn, 1.0);
        p.set(ParamId::Mid, -4.0);
        let mut c = Chain::new(48_000.0, &p);
        for f in [60.0f32, 500.0, 3000.0] {
            let mut buf: Vec<f32> = (0..48_000)
                .map(|i| 0.1 * (2.0 * std::f32::consts::PI * f * i as f32 / 48_000.0).sin())
                .collect();
            for ch in buf.chunks_mut(256) {
                c.process(ch);
            }
            let peak = buf[24_000..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let measured = crate::lin_to_db(peak / 0.1);
            let expected = response_db(&p, f, 48_000.0);
            assert!(
                (measured - expected).abs() < 0.2,
                "{f} Hz: {measured} vs {expected}"
            );
        }
    }

    #[test]
    fn no_nan_or_inf_with_extreme_params() {
        let p = ParamStore::default();
        let ids: Vec<ParamId> = ParamId::all().collect();
        for &id in &ids {
            p.set(id, id.spec().max);
        }
        let mut c = Chain::new(48_000.0, &p);
        let mut seed = 1u32;
        let mut buf = [0.0f32; 256];
        for round in 0..400 {
            if round % 50 == 0 {
                // Parametreleri uçtan uca sıçrat.
                for &id in &ids {
                    let s = id.spec();
                    p.set(id, if round % 100 == 0 { s.min } else { s.max });
                }
                c.update(&p);
            }
            for s in &mut buf {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *s = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            }
            c.process(&mut buf);
            for &s in &buf {
                assert!(s.is_finite());
                // Limiter açıkken tavan (en fazla 0 dBFS) asla aşılmaz.
                if p.on(ParamId::LimOn) {
                    assert!(s.abs() <= 1.0 + 1e-6, "tavan aşıldı: {s}");
                }
                let (l, r) = c.output(s);
                assert!(l.is_finite() && r.is_finite());
            }
        }
    }
}
