//! Kilitsiz parametre deposu. UI atomiklere yazar, ses thread'i her blokta
//! okur. Kilit ve allocation yoktur.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

/// Parametrik EQ bant sayısı.
pub const EQ_BANDS: usize = 16;

/// Parametrik EQ bandının alanları.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EqField {
    On,
    Kind,
    Freq,
    Gain,
    Q,
    Slope,
}

impl EqField {
    pub const COUNT: usize = 6;
    pub const ALL: [EqField; Self::COUNT] = [
        EqField::On,
        EqField::Kind,
        EqField::Freq,
        EqField::Gain,
        EqField::Q,
        EqField::Slope,
    ];
}

/// Parametre kimlikleri.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamId {
    InputGain,
    InputInvert,
    InputChannel,
    HpfOn,
    HpfFreq,
    HpfSlope,
    ToneOn,
    Bass,
    Mid,
    Treble,
    BassBoostOn,
    BassBoostFreq,
    BassBoostAmount,
    EqOn,
    OutputGain,
    OutputPan,
    GateOn,
    GateThreshold,
    GateHysteresis,
    GateAttack,
    GateHold,
    GateRelease,
    GateRange,
    ExpOn,
    ExpThreshold,
    ExpRatio,
    ExpAttack,
    ExpRelease,
    ExpRange,
    CompOn,
    CompThreshold,
    CompRatio,
    CompKnee,
    CompAttack,
    CompRelease,
    CompMakeup,
    CompScHpf,
    DeessOn,
    DeessFreq,
    DeessBandwidth,
    DeessThreshold,
    DeessRange,
    LimOn,
    LimCeiling,
    LimRelease,
    LimLookahead,
    RnnoiseOn,
    RnnoiseAmount,
    SpecGateOn,
    SpecGateThreshold,
    SpecGateReduction,
    SpecGateSmoothing,
    PitchOn,
    PitchSemitones,
    PitchCents,
    PitchMix,
    VoiceOn,
    VoiceMode,
    VoiceIntensity,
    VoiceMix,
    ChorusOn,
    ChorusRate,
    ChorusDepth,
    ChorusMix,
    DelayOn,
    DelayTime,
    DelayFeedback,
    DelayMix,
    DelayHighCut,
    ReverbOn,
    ReverbSize,
    ReverbDamping,
    ReverbMix,
    ReverbPredelay,
    MonitorOn,
    MonitorVolume,
    /// Parametrik EQ: (bant 0..EQ_BANDS, alan).
    Eq(u8, EqField),
}

/// Değerin gösterim birimi.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Unit {
    Db,
    Hz,
    /// -100 (sol) .. +100 (sağ)
    Pan,
    /// Milisaniye
    Ms,
    /// Oran (x:1)
    Ratio,
    /// Oktav
    Oct,
    /// Yüzde
    Percent,
    /// Yarım ton
    Semitone,
    /// Cent (yarım tonun 1/100'ü)
    Cent,
    None,
}

/// Değer ölçeği ve adımları.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scale {
    /// Doğrusal: kaba ve ince adım (birim cinsinden).
    Linear { coarse: f32, fine: f32 },
    /// Logaritmik (frekans, Q): kaba ve ince adım çarpanı.
    Log { coarse: f32, fine: f32 },
    /// Açık/kapalı (0/1).
    Toggle,
    /// Seçenek indeksleri (0..n).
    Choice(&'static [&'static str]),
}

#[derive(Debug)]
pub struct ParamSpec {
    pub id: ParamId,
    /// Config/preset anahtarı (kalıcı, değiştirme).
    pub key: String,
    /// Türkçe görünen ad.
    pub name: String,
    /// İngilizce görünen ad (yalnızca veri; ses yoluna girmez).
    pub name_en: String,
    /// `Scale::Choice` için İngilizce seçenek etiketleri (başka ölçeklerde boş).
    pub choices_en: &'static [&'static str],
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: Unit,
    pub scale: Scale,
}

const GAIN: Scale = Scale::Linear {
    coarse: 1.0,
    fine: 0.1,
};
const FREQ: Scale = Scale::Log {
    coarse: 1.122_462, // 2^(1/6): 1/6 oktav
    fine: 1.014_545,   // 2^(1/48)
};
const Q: Scale = Scale::Log {
    coarse: 1.122_462,
    fine: 1.014_545,
};
const TOGGLE: Scale = Scale::Toggle;
const PCT: Scale = Scale::Linear {
    coarse: 5.0,
    fine: 1.0,
};
const TIME: Scale = Scale::Log {
    coarse: 1.189_207, // 2^(1/4)
    fine: 1.021_897,   // 2^(1/32)
};
const RATIO: Scale = Scale::Log {
    coarse: 1.122_462,
    fine: 1.014_545,
};

pub const CHANNEL_CHOICES: &[&str] = &["Ortalama", "Sol", "Sağ"];
pub const SLOPE_CHOICES: &[&str] = &["12 dB/okt", "24 dB/okt", "48 dB/okt"];
/// Sıra `biquad::FilterType` dönüşümüyle aynıdır (bkz. `eq::kind_from_index`).
pub const VOICE_MODE_CHOICES: &[&str] = &["Robot", "Derin ses", "Telsiz"];
pub const EQ_KIND_CHOICES: &[&str] = &[
    "Peak",
    "Low shelf",
    "High shelf",
    "Low-pass",
    "High-pass",
    "Notch",
    "Band-pass",
];

// İngilizce karşılıklar (aynı sıra ve uzunluk).
pub const CHANNEL_CHOICES_EN: &[&str] = &["Average", "Left", "Right"];
pub const SLOPE_CHOICES_EN: &[&str] = &["12 dB/oct", "24 dB/oct", "48 dB/oct"];
pub const VOICE_MODE_CHOICES_EN: &[&str] = &["Robot", "Deep voice", "Radio"];
pub const EQ_KIND_CHOICES_EN: &[&str] = EQ_KIND_CHOICES;

/// Parametrenin İngilizce görünen adı.
fn english_name(id: ParamId) -> String {
    use ParamId::*;
    let name = match id {
        InputGain => "Input gain",
        InputInvert => "Phase invert",
        InputChannel => "Input channel",
        HpfOn => "High-pass",
        HpfFreq => "HPF frequency",
        HpfSlope => "HPF slope",
        ToneOn => "Tone controls",
        Bass => "Bass",
        Mid => "Mid",
        Treble => "Treble",
        BassBoostOn => "Bass Boost",
        BassBoostFreq => "Boost frequency",
        BassBoostAmount => "Boost amount",
        EqOn => "Parametric EQ",
        OutputGain => "Output gain",
        OutputPan => "Balance (pan)",
        GateOn => "Noise gate",
        GateThreshold | ExpThreshold | CompThreshold | DeessThreshold => "Threshold",
        GateHysteresis => "Hysteresis",
        GateAttack | ExpAttack | CompAttack => "Attack",
        GateHold => "Hold",
        GateRelease | ExpRelease | CompRelease | LimRelease => "Release",
        GateRange | ExpRange => "Range",
        ExpOn => "Expander",
        ExpRatio | CompRatio => "Ratio",
        CompOn => "Compressor",
        CompKnee => "Knee",
        CompMakeup => "Makeup gain",
        CompScHpf => "Sidechain HPF",
        DeessOn => "De-esser",
        DeessFreq => "Frequency",
        DeessBandwidth => "Bandwidth",
        DeessRange => "Max. reduction",
        LimOn => "Limiter",
        LimCeiling => "Ceiling",
        LimLookahead => "Lookahead",
        RnnoiseOn => "RNNoise",
        RnnoiseAmount => "Amount",
        SpecGateOn => "Spectral gate",
        SpecGateThreshold => "Profile margin",
        SpecGateReduction => "Reduction",
        SpecGateSmoothing => "Smoothing",
        PitchOn => "Pitch shift",
        PitchSemitones => "Pitch",
        PitchCents => "Fine tune",
        PitchMix | VoiceMix | ChorusMix => "Mix",
        VoiceOn => "Voice changer",
        VoiceMode => "Voice",
        VoiceIntensity => "Intensity",
        ChorusOn => "Chorus",
        ChorusRate => "Rate",
        ChorusDepth => "Depth",
        DelayOn => "Delay / Echo",
        DelayTime => "Time",
        DelayFeedback => "Feedback",
        DelayMix => "Echo level",
        DelayHighCut => "High cut",
        ReverbOn => "Reverb",
        ReverbSize => "Room size",
        ReverbDamping => "Damping",
        ReverbMix => "Wet/dry",
        ReverbPredelay => "Pre-delay",
        MonitorOn => "Self-monitor",
        MonitorVolume => "Monitor level",
        Eq(b, f) => {
            return match f {
                EqField::On => format!("Band {}", b + 1),
                EqField::Kind => "Type".into(),
                EqField::Freq => "Frequency".into(),
                EqField::Gain => "Gain".into(),
                EqField::Q => "Q".into(),
                EqField::Slope => "Slope (LP/HP)".into(),
            };
        }
    };
    name.to_string()
}

/// `Scale::Choice` parametrelerinin İngilizce etiketleri.
fn english_choices(id: ParamId) -> &'static [&'static str] {
    match id {
        ParamId::InputChannel => CHANNEL_CHOICES_EN,
        ParamId::HpfSlope | ParamId::Eq(_, EqField::Slope) => SLOPE_CHOICES_EN,
        ParamId::VoiceMode => VOICE_MODE_CHOICES_EN,
        ParamId::Eq(_, EqField::Kind) => EQ_KIND_CHOICES_EN,
        _ => &[],
    }
}

/// Skaler (bant dışı) parametreler, indeks sırasıyla.
const SCALARS: [ParamId; 76] = [
    ParamId::InputGain,
    ParamId::InputInvert,
    ParamId::InputChannel,
    ParamId::HpfOn,
    ParamId::HpfFreq,
    ParamId::HpfSlope,
    ParamId::ToneOn,
    ParamId::Bass,
    ParamId::Mid,
    ParamId::Treble,
    ParamId::BassBoostOn,
    ParamId::BassBoostFreq,
    ParamId::BassBoostAmount,
    ParamId::EqOn,
    ParamId::OutputGain,
    ParamId::OutputPan,
    ParamId::GateOn,
    ParamId::GateThreshold,
    ParamId::GateHysteresis,
    ParamId::GateAttack,
    ParamId::GateHold,
    ParamId::GateRelease,
    ParamId::GateRange,
    ParamId::ExpOn,
    ParamId::ExpThreshold,
    ParamId::ExpRatio,
    ParamId::ExpAttack,
    ParamId::ExpRelease,
    ParamId::ExpRange,
    ParamId::CompOn,
    ParamId::CompThreshold,
    ParamId::CompRatio,
    ParamId::CompKnee,
    ParamId::CompAttack,
    ParamId::CompRelease,
    ParamId::CompMakeup,
    ParamId::CompScHpf,
    ParamId::DeessOn,
    ParamId::DeessFreq,
    ParamId::DeessBandwidth,
    ParamId::DeessThreshold,
    ParamId::DeessRange,
    ParamId::LimOn,
    ParamId::LimCeiling,
    ParamId::LimRelease,
    ParamId::LimLookahead,
    ParamId::RnnoiseOn,
    ParamId::RnnoiseAmount,
    ParamId::SpecGateOn,
    ParamId::SpecGateThreshold,
    ParamId::SpecGateReduction,
    ParamId::SpecGateSmoothing,
    ParamId::PitchOn,
    ParamId::PitchSemitones,
    ParamId::PitchCents,
    ParamId::PitchMix,
    ParamId::VoiceOn,
    ParamId::VoiceMode,
    ParamId::VoiceIntensity,
    ParamId::VoiceMix,
    ParamId::ChorusOn,
    ParamId::ChorusRate,
    ParamId::ChorusDepth,
    ParamId::ChorusMix,
    ParamId::DelayOn,
    ParamId::DelayTime,
    ParamId::DelayFeedback,
    ParamId::DelayMix,
    ParamId::DelayHighCut,
    ParamId::ReverbOn,
    ParamId::ReverbSize,
    ParamId::ReverbDamping,
    ParamId::ReverbMix,
    ParamId::ReverbPredelay,
    ParamId::MonitorOn,
    ParamId::MonitorVolume,
];

/// Bant varsayılan frekansı: 31,25 Hz – 16 kHz arası eşit oktav aralıklı.
pub fn eq_default_freq(band: usize) -> f32 {
    let f = 31.25 * 2f32.powf(9.0 * band as f32 / (EQ_BANDS - 1) as f32);
    if f >= 1000.0 {
        (f / 10.0).round() * 10.0
    } else {
        f.round()
    }
}

fn build_specs() -> Vec<ParamSpec> {
    use ParamId::*;
    let s = |id, key: &str, name: &str, min, max, default, unit, scale| ParamSpec {
        id,
        key: key.to_string(),
        name: name.to_string(),
        name_en: english_name(id),
        choices_en: english_choices(id),
        min,
        max,
        default,
        unit,
        scale,
    };
    let mut v = Vec::with_capacity(ParamId::COUNT);
    for id in SCALARS {
        v.push(match id {
            InputGain => s(
                id,
                "input_gain",
                "Giriş kazancı",
                -60.0,
                30.0,
                0.0,
                Unit::Db,
                GAIN,
            ),
            InputInvert => s(
                id,
                "input_invert",
                "Faz ters çevir",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            InputChannel => s(
                id,
                "input_channel",
                "Giriş kanalı",
                0.0,
                2.0,
                0.0,
                Unit::None,
                Scale::Choice(CHANNEL_CHOICES),
            ),
            HpfOn => s(id, "hpf_on", "High-pass", 0.0, 1.0, 0.0, Unit::None, TOGGLE),
            HpfFreq => s(
                id,
                "hpf_freq",
                "HPF frekansı",
                20.0,
                500.0,
                80.0,
                Unit::Hz,
                FREQ,
            ),
            HpfSlope => s(
                id,
                "hpf_slope",
                "HPF eğimi",
                0.0,
                2.0,
                1.0,
                Unit::None,
                Scale::Choice(SLOPE_CHOICES),
            ),
            ToneOn => s(
                id,
                "tone_on",
                "Ton (Bass/Mid/Tiz)",
                0.0,
                1.0,
                1.0,
                Unit::None,
                TOGGLE,
            ),
            Bass => s(id, "bass", "Bass", -24.0, 24.0, 0.0, Unit::Db, GAIN),
            Mid => s(id, "mid", "Mid", -24.0, 24.0, 0.0, Unit::Db, GAIN),
            Treble => s(id, "treble", "Tiz", -24.0, 24.0, 0.0, Unit::Db, GAIN),
            BassBoostOn => s(
                id,
                "bass_boost_on",
                "Bass Boost",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            BassBoostFreq => s(
                id,
                "bass_boost_freq",
                "Boost frekansı",
                30.0,
                250.0,
                80.0,
                Unit::Hz,
                FREQ,
            ),
            BassBoostAmount => s(
                id,
                "bass_boost_amount",
                "Boost miktarı",
                0.0,
                18.0,
                6.0,
                Unit::Db,
                GAIN,
            ),
            EqOn => s(
                id,
                "eq_on",
                "Parametrik EQ",
                0.0,
                1.0,
                1.0,
                Unit::None,
                TOGGLE,
            ),
            OutputGain => s(
                id,
                "output_gain",
                "Çıkış kazancı",
                -60.0,
                30.0,
                0.0,
                Unit::Db,
                GAIN,
            ),
            OutputPan => s(
                id,
                "output_pan",
                "Denge (pan)",
                -100.0,
                100.0,
                0.0,
                Unit::Pan,
                Scale::Linear {
                    coarse: 5.0,
                    fine: 1.0,
                },
            ),
            GateOn => s(
                id,
                "gate_on",
                "Noise gate",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            GateThreshold => s(
                id,
                "gate_threshold",
                "Eşik",
                -80.0,
                0.0,
                -50.0,
                Unit::Db,
                GAIN,
            ),
            GateHysteresis => s(
                id,
                "gate_hysteresis",
                "Histerezis",
                0.0,
                20.0,
                4.0,
                Unit::Db,
                GAIN,
            ),
            GateAttack => s(id, "gate_attack", "Atak", 0.1, 50.0, 1.0, Unit::Ms, TIME),
            GateHold => s(
                id,
                "gate_hold",
                "Tutma",
                0.0,
                500.0,
                50.0,
                Unit::Ms,
                Scale::Linear {
                    coarse: 10.0,
                    fine: 1.0,
                },
            ),
            GateRelease => s(
                id,
                "gate_release",
                "Bırakma",
                5.0,
                1000.0,
                100.0,
                Unit::Ms,
                TIME,
            ),
            GateRange => s(
                id,
                "gate_range",
                "Aralık",
                -80.0,
                0.0,
                -40.0,
                Unit::Db,
                GAIN,
            ),
            ExpOn => s(id, "exp_on", "Expander", 0.0, 1.0, 0.0, Unit::None, TOGGLE),
            ExpThreshold => s(
                id,
                "exp_threshold",
                "Eşik",
                -80.0,
                0.0,
                -45.0,
                Unit::Db,
                GAIN,
            ),
            ExpRatio => s(id, "exp_ratio", "Oran", 1.0, 8.0, 2.0, Unit::Ratio, RATIO),
            ExpAttack => s(id, "exp_attack", "Atak", 0.1, 100.0, 2.0, Unit::Ms, TIME),
            ExpRelease => s(
                id,
                "exp_release",
                "Bırakma",
                5.0,
                1000.0,
                150.0,
                Unit::Ms,
                TIME,
            ),
            ExpRange => s(id, "exp_range", "Aralık", -60.0, 0.0, -20.0, Unit::Db, GAIN),
            CompOn => s(
                id,
                "comp_on",
                "Compressor",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            CompThreshold => s(
                id,
                "comp_threshold",
                "Eşik",
                -60.0,
                0.0,
                -18.0,
                Unit::Db,
                GAIN,
            ),
            CompRatio => s(id, "comp_ratio", "Oran", 1.0, 20.0, 3.0, Unit::Ratio, RATIO),
            CompKnee => s(id, "comp_knee", "Knee", 0.0, 24.0, 6.0, Unit::Db, GAIN),
            CompAttack => s(id, "comp_attack", "Atak", 0.1, 200.0, 10.0, Unit::Ms, TIME),
            CompRelease => s(
                id,
                "comp_release",
                "Bırakma",
                5.0,
                2000.0,
                120.0,
                Unit::Ms,
                TIME,
            ),
            CompMakeup => s(
                id,
                "comp_makeup",
                "Makeup kazanç",
                0.0,
                24.0,
                0.0,
                Unit::Db,
                GAIN,
            ),
            CompScHpf => s(
                id,
                "comp_sc_hpf",
                "Sidechain HPF",
                20.0,
                500.0,
                20.0,
                Unit::Hz,
                FREQ,
            ),
            DeessOn => s(
                id,
                "deess_on",
                "De-esser",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            DeessFreq => s(
                id,
                "deess_freq",
                "Frekans",
                2000.0,
                12000.0,
                6000.0,
                Unit::Hz,
                FREQ,
            ),
            DeessBandwidth => s(
                id,
                "deess_bandwidth",
                "Bant genişliği",
                0.3,
                3.0,
                1.0,
                Unit::Oct,
                Scale::Linear {
                    coarse: 0.1,
                    fine: 0.01,
                },
            ),
            DeessThreshold => s(
                id,
                "deess_threshold",
                "Eşik",
                -60.0,
                0.0,
                -30.0,
                Unit::Db,
                GAIN,
            ),
            DeessRange => s(
                id,
                "deess_range",
                "Maks. azaltma",
                0.0,
                24.0,
                12.0,
                Unit::Db,
                GAIN,
            ),
            LimOn => s(id, "lim_on", "Limiter", 0.0, 1.0, 1.0, Unit::None, TOGGLE),
            LimCeiling => s(id, "lim_ceiling", "Tavan", -24.0, 0.0, -1.0, Unit::Db, GAIN),
            LimRelease => s(
                id,
                "lim_release",
                "Bırakma",
                5.0,
                1000.0,
                50.0,
                Unit::Ms,
                TIME,
            ),
            LimLookahead => s(
                id,
                "lim_lookahead",
                "Lookahead",
                0.5,
                10.0,
                2.0,
                Unit::Ms,
                Scale::Linear {
                    coarse: 0.5,
                    fine: 0.1,
                },
            ),
            RnnoiseOn => s(
                id,
                "rnnoise_on",
                "RNNoise",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            RnnoiseAmount => s(
                id,
                "rnnoise_amount",
                "Yoğunluk",
                0.0,
                100.0,
                100.0,
                Unit::Percent,
                Scale::Linear {
                    coarse: 5.0,
                    fine: 1.0,
                },
            ),
            SpecGateOn => s(
                id,
                "spec_gate_on",
                "Spektral kapı",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            SpecGateThreshold => s(
                id,
                "spec_gate_threshold",
                "Profil üstü eşik",
                0.0,
                20.0,
                6.0,
                Unit::Db,
                GAIN,
            ),
            SpecGateReduction => s(
                id,
                "spec_gate_reduction",
                "Azaltma",
                0.0,
                40.0,
                20.0,
                Unit::Db,
                GAIN,
            ),
            SpecGateSmoothing => s(
                id,
                "spec_gate_smoothing",
                "Yumuşatma",
                5.0,
                200.0,
                40.0,
                Unit::Ms,
                TIME,
            ),
            PitchOn => s(
                id,
                "pitch_on",
                "Pitch shift",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            PitchSemitones => s(
                id,
                "pitch_semitones",
                "Perde",
                -12.0,
                12.0,
                0.0,
                Unit::Semitone,
                Scale::Linear {
                    coarse: 1.0,
                    fine: 1.0,
                },
            ),
            PitchCents => s(
                id,
                "pitch_cents",
                "İnce ayar",
                -100.0,
                100.0,
                0.0,
                Unit::Cent,
                Scale::Linear {
                    coarse: 5.0,
                    fine: 1.0,
                },
            ),
            PitchMix => s(
                id,
                "pitch_mix",
                "Karışım",
                0.0,
                100.0,
                100.0,
                Unit::Percent,
                PCT,
            ),
            VoiceOn => s(
                id,
                "voice_on",
                "Ses değiştirici",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            VoiceMode => s(
                id,
                "voice_mode",
                "Ses",
                0.0,
                2.0,
                0.0,
                Unit::None,
                Scale::Choice(VOICE_MODE_CHOICES),
            ),
            VoiceIntensity => s(
                id,
                "voice_intensity",
                "Yoğunluk",
                0.0,
                100.0,
                50.0,
                Unit::Percent,
                PCT,
            ),
            VoiceMix => s(
                id,
                "voice_mix",
                "Karışım",
                0.0,
                100.0,
                100.0,
                Unit::Percent,
                PCT,
            ),
            ChorusOn => s(id, "chorus_on", "Chorus", 0.0, 1.0, 0.0, Unit::None, TOGGLE),
            ChorusRate => s(id, "chorus_rate", "Hız", 0.1, 5.0, 0.8, Unit::Hz, TIME),
            ChorusDepth => s(
                id,
                "chorus_depth",
                "Derinlik",
                0.5,
                10.0,
                3.0,
                Unit::Ms,
                TIME,
            ),
            ChorusMix => s(
                id,
                "chorus_mix",
                "Karışım",
                0.0,
                100.0,
                50.0,
                Unit::Percent,
                PCT,
            ),
            DelayOn => s(
                id,
                "delay_on",
                "Delay / Echo",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            DelayTime => s(
                id,
                "delay_time",
                "Süre",
                10.0,
                2000.0,
                300.0,
                Unit::Ms,
                TIME,
            ),
            DelayFeedback => s(
                id,
                "delay_feedback",
                "Geri besleme",
                0.0,
                95.0,
                35.0,
                Unit::Percent,
                PCT,
            ),
            DelayMix => s(
                id,
                "delay_mix",
                "Yankı seviyesi",
                0.0,
                100.0,
                30.0,
                Unit::Percent,
                PCT,
            ),
            DelayHighCut => s(
                id,
                "delay_high_cut",
                "Tiz kesme",
                1000.0,
                20_000.0,
                6000.0,
                Unit::Hz,
                FREQ,
            ),
            ReverbOn => s(id, "reverb_on", "Reverb", 0.0, 1.0, 0.0, Unit::None, TOGGLE),
            ReverbSize => s(
                id,
                "reverb_size",
                "Oda boyutu",
                0.0,
                100.0,
                50.0,
                Unit::Percent,
                PCT,
            ),
            ReverbDamping => s(
                id,
                "reverb_damping",
                "Damping",
                0.0,
                100.0,
                50.0,
                Unit::Percent,
                PCT,
            ),
            ReverbMix => s(
                id,
                "reverb_mix",
                "Wet/dry",
                0.0,
                100.0,
                25.0,
                Unit::Percent,
                PCT,
            ),
            ReverbPredelay => s(
                id,
                "reverb_predelay",
                "Pre-delay",
                0.0,
                200.0,
                20.0,
                Unit::Ms,
                Scale::Linear {
                    coarse: 5.0,
                    fine: 1.0,
                },
            ),
            MonitorOn => s(
                id,
                "monitor_on",
                "Monitor",
                0.0,
                1.0,
                0.0,
                Unit::None,
                TOGGLE,
            ),
            MonitorVolume => s(
                id,
                "monitor_volume",
                "Monitor seviyesi",
                -60.0,
                6.0,
                -12.0,
                Unit::Db,
                GAIN,
            ),
            Eq(..) => unreachable!(),
        });
    }
    for b in 0..EQ_BANDS {
        let n = b + 1;
        for f in EqField::ALL {
            let id = Eq(b as u8, f);
            v.push(match f {
                EqField::On => s(
                    id,
                    &format!("eq{n}_on"),
                    &format!("Bant {n}"),
                    0.0,
                    1.0,
                    1.0,
                    Unit::None,
                    TOGGLE,
                ),
                EqField::Kind => s(
                    id,
                    &format!("eq{n}_type"),
                    "Tip",
                    0.0,
                    6.0,
                    0.0,
                    Unit::None,
                    Scale::Choice(EQ_KIND_CHOICES),
                ),
                EqField::Freq => s(
                    id,
                    &format!("eq{n}_freq"),
                    "Frekans",
                    20.0,
                    20_000.0,
                    eq_default_freq(b),
                    Unit::Hz,
                    FREQ,
                ),
                EqField::Gain => s(
                    id,
                    &format!("eq{n}_gain"),
                    "Kazanç",
                    -24.0,
                    24.0,
                    0.0,
                    Unit::Db,
                    GAIN,
                ),
                EqField::Q => s(id, &format!("eq{n}_q"), "Q", 0.1, 18.0, 1.0, Unit::None, Q),
                EqField::Slope => s(
                    id,
                    &format!("eq{n}_slope"),
                    "Eğim (LP/HP)",
                    0.0,
                    2.0,
                    0.0,
                    Unit::None,
                    Scale::Choice(SLOPE_CHOICES),
                ),
            });
        }
    }
    v
}

/// Tüm parametrelerin tanımı (UI tarafında ilk erişimde oluşturulur).
pub static SPECS: LazyLock<Vec<ParamSpec>> = LazyLock::new(build_specs);

impl ParamId {
    const N_SCALAR: usize = SCALARS.len();
    pub const COUNT: usize = Self::N_SCALAR + EQ_BANDS * EqField::COUNT;

    /// Depo indeksi.
    #[inline]
    pub fn index(self) -> usize {
        use ParamId::*;
        match self {
            InputGain => 0,
            InputInvert => 1,
            InputChannel => 2,
            HpfOn => 3,
            HpfFreq => 4,
            HpfSlope => 5,
            ToneOn => 6,
            Bass => 7,
            Mid => 8,
            Treble => 9,
            BassBoostOn => 10,
            BassBoostFreq => 11,
            BassBoostAmount => 12,
            EqOn => 13,
            OutputGain => 14,
            OutputPan => 15,
            GateOn => 16,
            GateThreshold => 17,
            GateHysteresis => 18,
            GateAttack => 19,
            GateHold => 20,
            GateRelease => 21,
            GateRange => 22,
            ExpOn => 23,
            ExpThreshold => 24,
            ExpRatio => 25,
            ExpAttack => 26,
            ExpRelease => 27,
            ExpRange => 28,
            CompOn => 29,
            CompThreshold => 30,
            CompRatio => 31,
            CompKnee => 32,
            CompAttack => 33,
            CompRelease => 34,
            CompMakeup => 35,
            CompScHpf => 36,
            DeessOn => 37,
            DeessFreq => 38,
            DeessBandwidth => 39,
            DeessThreshold => 40,
            DeessRange => 41,
            LimOn => 42,
            LimCeiling => 43,
            LimRelease => 44,
            LimLookahead => 45,
            RnnoiseOn => 46,
            RnnoiseAmount => 47,
            SpecGateOn => 48,
            SpecGateThreshold => 49,
            SpecGateReduction => 50,
            SpecGateSmoothing => 51,
            PitchOn => 52,
            PitchSemitones => 53,
            PitchCents => 54,
            PitchMix => 55,
            VoiceOn => 56,
            VoiceMode => 57,
            VoiceIntensity => 58,
            VoiceMix => 59,
            ChorusOn => 60,
            ChorusRate => 61,
            ChorusDepth => 62,
            ChorusMix => 63,
            DelayOn => 64,
            DelayTime => 65,
            DelayFeedback => 66,
            DelayMix => 67,
            DelayHighCut => 68,
            ReverbOn => 69,
            ReverbSize => 70,
            ReverbDamping => 71,
            ReverbMix => 72,
            ReverbPredelay => 73,
            MonitorOn => 74,
            MonitorVolume => 75,
            Eq(b, f) => Self::N_SCALAR + b as usize * EqField::COUNT + f as usize,
        }
    }

    /// Tüm parametreler, indeks sırasıyla.
    pub fn all() -> impl Iterator<Item = ParamId> {
        SPECS.iter().map(|s| s.id)
    }

    /// Ses ayarı mı? Oturum ayarları (monitor) preset, A/B ve geri alma
    /// geçmişine girmez.
    pub fn is_sound(self) -> bool {
        !matches!(self, ParamId::MonitorOn | ParamId::MonitorVolume)
    }

    /// Ses ayarı parametreleri (preset/A-B/geri alma kapsamı).
    pub fn sound() -> impl Iterator<Item = ParamId> {
        Self::all().filter(|id| id.is_sound())
    }

    pub fn spec(self) -> &'static ParamSpec {
        &SPECS[self.index()]
    }

    pub fn from_key(key: &str) -> Option<ParamId> {
        SPECS.iter().find(|s| s.key == key).map(|s| s.id)
    }

    /// Bu parametrenin ait olduğu modülün açma/kapama (bypass) parametresi.
    pub fn module_toggle(self) -> Option<ParamId> {
        use ParamId::*;
        match self {
            HpfOn | HpfFreq | HpfSlope => Some(HpfOn),
            ToneOn | Bass | Mid | Treble => Some(ToneOn),
            BassBoostOn | BassBoostFreq | BassBoostAmount => Some(BassBoostOn),
            EqOn => Some(EqOn),
            GateOn | GateThreshold | GateHysteresis | GateAttack | GateHold | GateRelease
            | GateRange => Some(GateOn),
            ExpOn | ExpThreshold | ExpRatio | ExpAttack | ExpRelease | ExpRange => Some(ExpOn),
            CompOn | CompThreshold | CompRatio | CompKnee | CompAttack | CompRelease
            | CompMakeup | CompScHpf => Some(CompOn),
            DeessOn | DeessFreq | DeessBandwidth | DeessThreshold | DeessRange => Some(DeessOn),
            LimOn | LimCeiling | LimRelease | LimLookahead => Some(LimOn),
            RnnoiseOn | RnnoiseAmount => Some(RnnoiseOn),
            SpecGateOn | SpecGateThreshold | SpecGateReduction | SpecGateSmoothing => {
                Some(SpecGateOn)
            }
            PitchOn | PitchSemitones | PitchCents | PitchMix => Some(PitchOn),
            VoiceOn | VoiceMode | VoiceIntensity | VoiceMix => Some(VoiceOn),
            ChorusOn | ChorusRate | ChorusDepth | ChorusMix => Some(ChorusOn),
            DelayOn | DelayTime | DelayFeedback | DelayMix | DelayHighCut => Some(DelayOn),
            ReverbOn | ReverbSize | ReverbDamping | ReverbMix | ReverbPredelay => Some(ReverbOn),
            MonitorOn | MonitorVolume => Some(MonitorOn),
            Eq(b, _) => Some(Eq(b, EqField::On)),
            _ => None,
        }
    }
}

impl ParamSpec {
    /// Değeri aralığa sıkıştırır; açma/kapama ve seçenekleri tam sayıya yuvarlar.
    pub fn clamp(&self, v: f32) -> f32 {
        let v = if v.is_finite() { v } else { self.default };
        let v = v.clamp(self.min, self.max);
        match self.scale {
            Scale::Toggle | Scale::Choice(_) => v.round(),
            _ => v,
        }
    }

    /// Bir adım ileri/geri (`dir` = ±1). Açma/kapama ve seçenekler döner.
    pub fn step(&self, v: f32, dir: f32, fine: bool) -> f32 {
        match self.scale {
            Scale::Linear { coarse, fine: f } => {
                let s = if fine { f } else { coarse };
                // Adım ızgarasına hizala (ör. 0.37 → 1.0 değil 1.37 olmasın).
                self.clamp(((v / s).round() + dir) * s)
            }
            Scale::Log { coarse, fine: f } => {
                let r = if fine { f } else { coarse };
                self.clamp(v * r.powf(dir))
            }
            Scale::Toggle => 1.0 - v.round(),
            Scale::Choice(c) => {
                let n = c.len() as f32;
                ((v.round() + dir).rem_euclid(n)).round()
            }
        }
    }

    /// Değeri kaydırıcı konumuna (0..1) çevirir.
    pub fn to_norm(&self, v: f32) -> f32 {
        match self.scale {
            Scale::Log { .. } => (v / self.min).ln() / (self.max / self.min).ln(),
            _ => (v - self.min) / (self.max - self.min),
        }
        .clamp(0.0, 1.0)
    }

    /// Kaydırıcı konumunu (0..1) değere çevirir.
    pub fn from_norm(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        self.clamp(match self.scale {
            Scale::Log { .. } => self.min * (self.max / self.min).powf(n),
            _ => self.min + n * (self.max - self.min),
        })
    }

    /// Görünen ad (`en`: İngilizce).
    pub fn name_lang(&self, en: bool) -> &str {
        if en { &self.name_en } else { &self.name }
    }

    /// Seçenek etiketleri (`Scale::Choice` değilse boş).
    pub fn choices_lang(&self, en: bool) -> &'static [&'static str] {
        match self.scale {
            Scale::Choice(_) if en => self.choices_en,
            Scale::Choice(c) => c,
            _ => &[],
        }
    }

    /// İnsan tarafından okunur değer (Türkçe).
    pub fn format(&self, v: f32) -> String {
        self.format_lang(v, false)
    }

    /// İnsan tarafından okunur değer; `en`: İngilizce birimler/etiketler.
    pub fn format_lang(&self, v: f32, en: bool) -> String {
        match self.scale {
            Scale::Toggle => match (v >= 0.5, en) {
                (true, false) => "Açık",
                (false, false) => "Kapalı",
                (true, true) => "On",
                (false, true) => "Off",
            }
            .into(),
            Scale::Choice(_) => self
                .choices_lang(en)
                .get(v.round() as usize)
                .copied()
                .unwrap_or("?")
                .into(),
            _ => match self.unit {
                Unit::Db => format!("{v:+.1} dB"),
                Unit::Hz if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
                Unit::Hz if v < 10.0 => format!("{v:.2} Hz"),
                Unit::Hz => format!("{v:.0} Hz"),
                Unit::Semitone if en => format!("{v:+.0} st"),
                Unit::Semitone => format!("{v:+.0} yarım ton"),
                Unit::Cent => format!("{v:+.0} cent"),
                Unit::Pan if v.abs() < 0.5 => if en { "Center" } else { "Orta" }.into(),
                Unit::Pan if v < 0.0 && en => format!("L {:.0}", -v),
                Unit::Pan if v < 0.0 => format!("Sol {:.0}", -v),
                Unit::Pan if en => format!("R {v:.0}"),
                Unit::Pan => format!("Sağ {v:.0}"),
                Unit::Ms if v < 10.0 => format!("{v:.1} ms"),
                Unit::Ms => format!("{v:.0} ms"),
                Unit::Ratio => format!("{v:.1}:1"),
                Unit::Oct if en => format!("{v:.2} oct"),
                Unit::Oct => format!("{v:.2} okt"),
                Unit::Percent if en => format!("{v:.0}%"),
                Unit::Percent => format!("%{v:.0}"),
                Unit::None => format!("{v:.2}"),
            },
        }
    }
}

/// UI ile ses thread'i arasında paylaşılan parametre değerleri.
pub struct ParamStore {
    values: [AtomicU32; ParamId::COUNT],
    /// Yeniden sıralanabilir modüllerin işlem sırası, modül başına 4 bit.
    order: AtomicU64,
}

/// Yeniden sıralanabilir modül sayısı (bkz. `chain::MODULE_KEYS`).
pub const MODULE_COUNT: usize = 15;

/// Varsayılan sıra: 0, 1, …, 14.
pub const DEFAULT_ORDER: [u8; MODULE_COUNT] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14];

fn pack_order(order: &[u8; MODULE_COUNT]) -> u64 {
    order
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, &m)| acc | ((m as u64 & 0xF) << (i * 4)))
}

/// Geçerli bir permütasyon mu (her modül tam bir kez).
pub fn is_valid_order(order: &[u8]) -> bool {
    let mut seen = [false; MODULE_COUNT];
    order.len() == MODULE_COUNT
        && order.iter().all(|&m| {
            let ok = (m as usize) < MODULE_COUNT && !seen[m as usize];
            if ok {
                seen[m as usize] = true;
            }
            ok
        })
}

impl Default for ParamStore {
    fn default() -> Self {
        Self {
            values: std::array::from_fn(|i| AtomicU32::new(SPECS[i].default.to_bits())),
            order: AtomicU64::new(pack_order(&DEFAULT_ORDER)),
        }
    }
}

impl ParamStore {
    /// Modül sırası (ses thread'inden de okunabilir; allocation yok).
    #[inline]
    pub fn order(&self) -> [u8; MODULE_COUNT] {
        let v = self.order.load(Relaxed);
        let o: [u8; MODULE_COUNT] = std::array::from_fn(|i| ((v >> (i * 4)) & 0xF) as u8);
        if is_valid_order(&o) { o } else { DEFAULT_ORDER }
    }

    /// Geçersiz (permütasyon olmayan) sıra reddedilir: `false`.
    pub fn set_order(&self, order: &[u8; MODULE_COUNT]) -> bool {
        if !is_valid_order(order) {
            return false;
        }
        self.order.store(pack_order(order), Relaxed);
        true
    }

    #[inline]
    pub fn get(&self, id: ParamId) -> f32 {
        f32::from_bits(self.values[id.index()].load(Relaxed))
    }

    #[inline]
    pub fn on(&self, id: ParamId) -> bool {
        self.get(id) >= 0.5
    }

    /// Değeri sıkıştırarak yazar ve yazılan değeri döndürür (UI tarafı).
    pub fn set(&self, id: ParamId, v: f32) -> f32 {
        let v = id.spec().clamp(v);
        self.values[id.index()].store(v.to_bits(), Relaxed);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_are_in_index_order() {
        assert_eq!(SPECS.len(), ParamId::COUNT);
        let mut keys = std::collections::HashSet::new();
        for (i, s) in SPECS.iter().enumerate() {
            assert_eq!(s.id.index(), i, "{}", s.key);
            assert!(s.min <= s.default && s.default <= s.max, "{}", s.key);
            assert!(keys.insert(s.key.clone()), "yinelenen anahtar {}", s.key);
        }
    }

    #[test]
    fn english_names_and_choices_are_complete() {
        for s in SPECS.iter() {
            assert!(!s.name_en.is_empty(), "{}", s.key);
            // TUI parametre adı sütunu 18 karakter.
            assert!(s.name_en.chars().count() <= 18, "{}: {}", s.key, s.name_en);
            assert_eq!(s.name_lang(false), s.name);
            assert_eq!(s.name_lang(true), s.name_en);
            if let Scale::Choice(tr) = s.scale {
                assert_eq!(tr.len(), s.choices_en.len(), "{}", s.key);
                assert!(s.choices_en.iter().all(|c| !c.is_empty()), "{}", s.key);
                assert_eq!(s.choices_lang(true), s.choices_en);
                assert_eq!(s.choices_lang(false), tr);
            } else {
                assert!(s.choices_en.is_empty(), "{}", s.key);
                assert!(s.choices_lang(true).is_empty());
            }
        }
        for (tr, en) in [
            (CHANNEL_CHOICES, CHANNEL_CHOICES_EN),
            (SLOPE_CHOICES, SLOPE_CHOICES_EN),
            (VOICE_MODE_CHOICES, VOICE_MODE_CHOICES_EN),
            (EQ_KIND_CHOICES, EQ_KIND_CHOICES_EN),
        ] {
            assert_eq!(tr.len(), en.len());
        }
        assert_eq!(
            crate::chain::MODULE_NAMES.len(),
            crate::chain::MODULE_NAMES_EN.len()
        );
        // TUI zincir sütunu 22 karakter.
        assert!(
            crate::chain::MODULE_NAMES_EN
                .iter()
                .all(|n| !n.is_empty() && n.chars().count() <= 22)
        );
        assert_eq!(ParamId::Eq(2, EqField::On).spec().name_en, "Band 3");
        assert_eq!(ParamId::Eq(2, EqField::On).spec().name, "Bant 3");
    }

    #[test]
    fn formatting_in_both_languages() {
        let f = |id: ParamId, v: f32| {
            (
                id.spec().format_lang(v, false),
                id.spec().format_lang(v, true),
            )
        };
        let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(f(ParamId::HpfOn, 1.0), pair("Açık", "On"));
        assert_eq!(f(ParamId::HpfOn, 0.0), pair("Kapalı", "Off"));
        assert_eq!(f(ParamId::OutputPan, 0.0), pair("Orta", "Center"));
        assert_eq!(f(ParamId::OutputPan, -30.0), pair("Sol 30", "L 30"));
        assert_eq!(f(ParamId::OutputPan, 30.0), pair("Sağ 30", "R 30"));
        assert_eq!(
            f(ParamId::PitchSemitones, 3.0),
            pair("+3 yarım ton", "+3 st")
        );
        assert_eq!(
            f(ParamId::DeessBandwidth, 1.0),
            pair("1.00 okt", "1.00 oct")
        );
        assert_eq!(f(ParamId::RnnoiseAmount, 50.0), pair("%50", "50%"));
        assert_eq!(f(ParamId::HpfSlope, 1.0), pair("24 dB/okt", "24 dB/oct"));
        assert_eq!(f(ParamId::InputChannel, 2.0), pair("Sağ", "Right"));
        assert_eq!(f(ParamId::InputGain, 3.0), pair("+3.0 dB", "+3.0 dB"));
        assert_eq!(f(ParamId::HpfFreq, 1500.0), pair("1.50 kHz", "1.50 kHz"));
        // `format` Türkçe davranışı değişmedi.
        assert_eq!(ParamId::OutputPan.spec().format(-30.0), "Sol 30");
        assert_eq!(ParamId::InputChannel.spec().format(9.0), "?");
    }

    #[test]
    fn eq_default_freqs_span_range() {
        assert_eq!(eq_default_freq(0), 31.0);
        assert_eq!(eq_default_freq(EQ_BANDS - 1), 16_000.0);
        for b in 1..EQ_BANDS {
            assert!(eq_default_freq(b) > eq_default_freq(b - 1));
        }
    }

    #[test]
    fn clamp_and_steps() {
        let g = ParamId::InputGain.spec();
        assert_eq!(g.clamp(100.0), 30.0);
        assert_eq!(g.clamp(f32::NAN), 0.0);
        assert!((g.step(0.37, 1.0, false) - 1.0).abs() < 1e-6);
        assert!((g.step(0.0, 1.0, true) - 0.1).abs() < 1e-6);
        let c = ParamId::InputChannel.spec();
        assert_eq!(c.step(2.0, 1.0, false), 0.0);
        assert_eq!(c.step(0.0, -1.0, false), 2.0);
        assert_eq!(ParamId::HpfOn.spec().step(0.0, 1.0, false), 1.0);
    }

    #[test]
    fn log_norm_roundtrip() {
        let f = ParamId::HpfFreq.spec();
        for v in [20.0f32, 80.0, 500.0] {
            assert!((f.from_norm(f.to_norm(v)) - v).abs() < 0.01);
        }
    }

    #[test]
    fn store_set_get() {
        let s = ParamStore::default();
        assert_eq!(s.get(ParamId::HpfFreq), 80.0);
        assert_eq!(s.set(ParamId::Bass, 99.0), 24.0);
        assert_eq!(s.get(ParamId::Bass), 24.0);
        let id = ParamId::Eq(15, EqField::Gain);
        assert_eq!(s.set(id, -5.0), -5.0);
        assert_eq!(ParamId::from_key("eq16_gain"), Some(id));
    }
}
