use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ebur128::EbuR128;
use micforge_audio::{
    AudioEngine, CaptureStatus, Device, Event as AudioEvent, NOISE_LEARN_SECONDS,
    NOISE_PROFILE_LEN, ProfileBox, ProfileKind, SAMPLE_RATE,
};
use micforge_dsp::chain::{MODULE_KEYS, MODULE_TOGGLES};
use micforge_dsp::feedback::FeedbackDetector;
use micforge_dsp::lin_to_db;
use micforge_dsp::params::{
    DEFAULT_ORDER, EQ_BANDS, EqField, MODULE_COUNT, ParamId, ParamStore, Scale,
};
use micforge_dsp::spectrum::SpectrumAnalyzer;
use ratatui::layout::{Position, Rect};
use ratatui::widgets::ListState;

use crate::config::Config;
use crate::graph::{self, BandMarker, GraphView, SpectrumMode};
use crate::history::History;
use crate::i18n::{self, l, tr};
use crate::preset::{MODULE_KEYS as PRESET_MODULE_KEYS, Preset, PresetEntry, PresetStore};
use crate::theme::{self, Theme};

pub const TABS: [&str; 8] = [
    "Genel",
    "EQ",
    "Dinamikler",
    "Gürültü",
    "Efektler",
    "Zincir",
    "Presetler",
    "Ayarlar",
];

/// `TABS` İngilizce karşılıkları.
pub const TABS_EN: [&str; 8] = [
    "General", "EQ", "Dynamics", "Noise", "Effects", "Chain", "Presets", "Settings",
];

/// Sekme adı (`en`: İngilizce).
pub fn tab_name_lang(i: usize, en: bool) -> &'static str {
    if en { TABS_EN[i] } else { TABS[i] }
}

/// Etkin dildeki sekme adı.
pub fn tab_name(i: usize) -> &'static str {
    tab_name_lang(i, i18n::en())
}

/// Bir sekmedeki parametre paneli.
pub struct Panel {
    pub title: String,
    pub ids: Vec<ParamId>,
}

fn panel(title: &str, ids: &[ParamId]) -> Panel {
    Panel {
        title: title.to_string(),
        ids: ids.to_vec(),
    }
}

/// Bir sekmenin parametre panelleri (odak sırasıyla).
pub fn tab_panels(tab: usize, eq_band: usize) -> Vec<Panel> {
    use ParamId::*;
    match tab {
        0 => vec![
            panel(
                l(" Giriş / Çıkış ", " Input / Output "),
                &[InputGain, InputInvert, InputChannel, OutputGain, OutputPan],
            ),
            panel(
                l(" Monitor (kendini duy) ", " Monitor (hear yourself) "),
                &[MonitorOn, MonitorVolume],
            ),
        ],
        1 => vec![
            panel(
                l(" Hızlı kontroller ", " Quick controls "),
                &[
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
                ],
            ),
            Panel {
                title: tr!(
                    format!(" Bant {}/{EQ_BANDS} — [ ] ile değiştir ", eq_band + 1),
                    format!(" Band {}/{EQ_BANDS} — change with [ ] ", eq_band + 1)
                ),
                ids: band_params(eq_band).to_vec(),
            },
        ],
        2 => vec![
            panel(
                " Noise gate ",
                &[
                    GateOn,
                    GateThreshold,
                    GateHysteresis,
                    GateAttack,
                    GateHold,
                    GateRelease,
                    GateRange,
                ],
            ),
            panel(
                " Expander ",
                &[
                    ExpOn,
                    ExpThreshold,
                    ExpRatio,
                    ExpAttack,
                    ExpRelease,
                    ExpRange,
                ],
            ),
            panel(
                " Compressor ",
                &[
                    CompOn,
                    CompThreshold,
                    CompRatio,
                    CompKnee,
                    CompAttack,
                    CompRelease,
                    CompMakeup,
                    CompScHpf,
                ],
            ),
            panel(
                " De-esser ",
                &[
                    DeessOn,
                    DeessFreq,
                    DeessBandwidth,
                    DeessThreshold,
                    DeessRange,
                ],
            ),
            panel(
                " Limiter (brickwall) ",
                &[LimOn, LimCeiling, LimRelease, LimLookahead],
            ),
        ],
        3 => vec![
            panel(
                l(" RNNoise (yapay zekâ) ", " RNNoise (AI) "),
                &[RnnoiseOn, RnnoiseAmount],
            ),
            panel(
                l(" Spektral gürültü kapısı ", " Spectral noise gate "),
                &[
                    SpecGateOn,
                    SpecGateThreshold,
                    SpecGateReduction,
                    SpecGateSmoothing,
                ],
            ),
        ],
        4 => vec![
            panel(
                " Pitch shift ",
                &[PitchOn, PitchSemitones, PitchCents, PitchMix],
            ),
            panel(
                l(" Ses değiştirici ", " Voice changer "),
                &[VoiceOn, VoiceMode, VoiceIntensity, VoiceMix],
            ),
            panel(" Chorus ", &[ChorusOn, ChorusRate, ChorusDepth, ChorusMix]),
            panel(
                " Delay / Echo ",
                &[DelayOn, DelayTime, DelayFeedback, DelayMix, DelayHighCut],
            ),
            panel(
                " Reverb ",
                &[
                    ReverbOn,
                    ReverbSize,
                    ReverbDamping,
                    ReverbMix,
                    ReverbPredelay,
                ],
            ),
        ],
        _ => vec![],
    }
}

/// Bir sekmedeki en fazla panel sayısı (seçim belleği için).
const MAX_PANELS: usize = 8;

/// Seçili EQ bandının düzenlenebilir parametreleri.
pub fn band_params(band: usize) -> [ParamId; EqField::COUNT] {
    EqField::ALL.map(|f| ParamId::Eq(band as u8, f))
}

/// Spektrum FFT boyutu (48 kHz'de ~85 ms pencere, ~11,7 Hz çözünürlük).
const FFT_SIZE: usize = 4096;

/// Kazanç azaltma göstergeleri (motorun gönderdiği sırayla).
pub const GR_LABELS: [&str; 6] = [
    "Gate",
    "Expander",
    "Compressor",
    "De-esser",
    "Limiter",
    "Spektral",
];

/// Kazanç azaltma satırının etiketi (yalnızca sonuncusu dile göre değişir).
pub fn gr_label(i: usize) -> &'static str {
    if i == 5 {
        l("Spektral", "Spectral")
    } else {
        GR_LABELS[i]
    }
}

/// Metrenin alt sınırı (dBFS).
pub const METER_FLOOR_DB: f32 = -60.0;
const DECAY_DB_PER_S: f32 = 24.0;
const HOLD: Duration = Duration::from_millis(1500);
const CLIP_HOLD: Duration = Duration::from_secs(2);
/// Geri al geçmişi: en fazla adım ve aynı parametredeki ardışık değişiklikleri
/// tek adımda birleştirme penceresi.
const HISTORY_MAX: usize = 200;
const HISTORY_COALESCE: Duration = Duration::from_millis(600);

/// Geri alma geçmişindeki bir anahtar: parametre değeri ya da zincirdeki bir
/// sıra konumunda duran modül (değer = modül indeksi).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HistKey {
    Param(ParamId),
    Order(u8),
}

fn new_history() -> History<HistKey> {
    History::new(HISTORY_MAX, HISTORY_COALESCE)
}

/// Motor durduğunda yeniden başlatma denemeleri arası süre.
const ENGINE_RETRY: Duration = Duration::from_secs(3);

/// A/B karşılaştırmada etkin olmayan ayar kümesi.
struct AbSlot {
    values: Vec<f32>,
    order: [u8; MODULE_COUNT],
    history: History<HistKey>,
}

/// Metin girişi / onay gerektiren preset işlemleri.
#[derive(Clone, Debug, PartialEq)]
pub enum PromptKind {
    SaveAs,
    /// "Farklı kaydet" adı mevcut bir presetle çakışıyor; üzerine yazma onayı.
    ConfirmSaveAs(String),
    Import,
    Export(std::path::PathBuf),
    ConfirmOverwriteExport(std::path::PathBuf, std::path::PathBuf),
    ConfirmSave(std::path::PathBuf),
    ConfirmDelete(std::path::PathBuf),
}

pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
}

/// Değişen parametreler bu süre sonra diske yazılır.
const SAVE_DELAY: Duration = Duration::from_secs(2);

/// Ekrandaki bir metrenin görsel durumu (düşüş ve tepe tutma UI tarafında).
pub struct Meter {
    pub db: f32,
    pub hold_db: f32,
    hold_until: Instant,
}

impl Meter {
    fn new() -> Self {
        Self {
            db: METER_FLOOR_DB,
            hold_db: METER_FLOOR_DB,
            hold_until: Instant::now(),
        }
    }

    fn update(&mut self, lin: f32, dt: f32, now: Instant) {
        let db = lin_to_db(lin).max(METER_FLOOR_DB);
        self.db = db.max(self.db - DECAY_DB_PER_S * dt);
        if db >= self.hold_db || now > self.hold_until {
            self.hold_db = db;
            self.hold_until = now + HOLD;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    /// Genel sekmesindeki cihaz listesi.
    Devices,
    /// Sekmenin n. parametre paneli.
    Panel(usize),
}

/// Fare için son çizimde kullanılan alanlar.
#[derive(Default)]
pub struct Areas {
    pub tabs: Vec<Rect>,
    pub devices: Rect,
    /// Parametre satırları: (parametre, satır, kaydırıcı, panel, paneldeki indeks).
    pub params: Vec<(ParamId, Rect, Rect, usize, usize)>,
    /// EQ grafiğinin çizim alanı (eğri ve bant noktaları).
    pub graph: Rect,
    /// Zincir sekmesindeki modül satırları (sıra konumuna göre).
    pub chain_rows: Vec<Rect>,
    /// Presetler sekmesindeki satırlar.
    pub preset_rows: Vec<Rect>,
    /// Ayarlar sekmesindeki satırlar.
    pub settings_rows: Vec<Rect>,
}

pub struct App {
    pub engine: AudioEngine,
    pub params: Arc<ParamStore>,
    pub config: Config,
    pub devices: Vec<Device>,
    pub list: ListState,
    pub status: CaptureStatus,
    pub tab: usize,
    pub focus: Focus,
    /// Sekme başına seçili parametre satırı.
    pub param_sel: [[usize; MAX_PANELS]; TABS.len()],
    /// Sayısal değer girişi: (parametre, yazılan metin).
    pub entry: Option<(ParamId, String)>,
    dragging: Option<(ParamId, Rect)>,
    /// Grafikte sürüklenen EQ bandı.
    dragging_band: Option<usize>,
    /// Seçili EQ bandı (0..EQ_BANDS).
    pub eq_band: usize,
    pub spectrum_mode: SpectrumMode,
    pub view: GraphView,
    pub analyzer_in: SpectrumAnalyzer,
    pub analyzer_out: SpectrumAnalyzer,
    pub spec_in: Vec<f32>,
    pub spec_out: Vec<f32>,
    /// Son iki kare arası süre (spektrum düşüşü için).
    pub frame_dt: f32,
    tap_buf: Vec<f32>,
    pub in_peak: Meter,
    pub in_rms: Meter,
    pub out_peak: Meter,
    pub out_rms: Meter,
    /// Kazanç azaltma (pozitif dB, düşüşlü gösterim).
    pub gr: [f32; 6],
    /// Profil öğrenmenin başladığı an (geri sayım için).
    pub learn_started: Option<Instant>,
    /// "Profil öğren" düğmesinin alanı.
    pub learn_button: Rect,
    /// Zincir sekmesinde seçili sıra konumu.
    pub chain_sel: usize,
    /// Fareyle sürüklenen modülün şu anki sıra konumu.
    chain_drag: Option<usize>,
    pub clip_until: Option<Instant>,
    last_clip_count: u64,
    pub show_notice: bool,
    pub message: Option<String>,
    pub fatal: Option<String>,
    pub quit: bool,
    pub areas: Areas,
    history: History<HistKey>,
    /// A/B: true ise B etkin.
    pub ab_is_b: bool,
    ab_other: Option<AbSlot>,
    pub presets: Option<PresetStore>,
    /// Preset listesi ve açıklamaları.
    pub preset_list: Vec<(PresetEntry, String)>,
    pub preset_sel: usize,
    /// Son yüklenen/kaydedilen preset ve sonrasında değişiklik yapıldı mı.
    pub current_preset: Option<String>,
    pub preset_dirty: bool,
    pub prompt: Option<Prompt>,
    /// Kısa süreli bilgi mesajı (durum satırında).
    pub toast: Option<(String, Instant)>,
    /// Önceki karedeki grafik çizim alanı (spektrum sütun sayısı için).
    pub areas_graph_last: Rect,
    last_tick: Instant,
    save_at: Option<Instant>,
    /// Ses motoruna en son bildirilen monitor durumu.
    monitor_sent: bool,
    feedback: FeedbackDetector,
    /// EBU R128 ölçer (çıkış, çift mono).
    lufs: EbuR128,
    /// LUFS: anlık (400 ms), kısa süreli (3 s), entegre. Ölçüm yoksa -inf.
    pub lufs_vals: [f64; 3],
    /// Ayarlar sekmesinde seçili satır.
    pub settings_sel: usize,
    /// Yardım ekranı açık mı.
    pub show_help: bool,
    /// Config'deki geçersiz özel renkler.
    pub theme_errors: Vec<String>,
    /// Son geri besleme algılaması (frekans, an): uyarı rozeti için.
    pub feedback_hit: Option<(f32, Instant)>,
    /// Ses motoru durduysa (PipeWire koptu) bir sonraki yeniden başlatma denemesi.
    pub engine_retry_at: Option<Instant>,
}

impl App {
    pub fn new(
        engine: AudioEngine,
        params: Arc<ParamStore>,
        config: Config,
        warning: Option<String>,
    ) -> Self {
        let (t, theme_errors) = Theme::build(config.light_theme(), &config.theme_colors);
        theme::set(t);
        Self {
            engine,
            params,
            show_notice: !config.first_run_notice_shown,
            config,
            devices: Vec::new(),
            list: ListState::default(),
            status: CaptureStatus::Idle,
            tab: 0,
            focus: Focus::Devices,
            param_sel: [[0; MAX_PANELS]; TABS.len()],
            entry: None,
            dragging: None,
            dragging_band: None,
            eq_band: 0,
            spectrum_mode: SpectrumMode::Overlay,
            view: GraphView::default(),
            analyzer_in: SpectrumAnalyzer::new(SAMPLE_RATE as f32, FFT_SIZE),
            analyzer_out: SpectrumAnalyzer::new(SAMPLE_RATE as f32, FFT_SIZE),
            spec_in: Vec::new(),
            spec_out: Vec::new(),
            frame_dt: 0.0,
            tap_buf: vec![0.0; 16_384],
            in_peak: Meter::new(),
            in_rms: Meter::new(),
            out_peak: Meter::new(),
            out_rms: Meter::new(),
            gr: [0.0; 6],
            learn_started: None,
            learn_button: Rect::default(),
            chain_sel: 0,
            chain_drag: None,
            clip_until: None,
            last_clip_count: 0,
            message: warning,
            fatal: None,
            quit: false,
            areas: Areas::default(),
            history: new_history(),
            ab_is_b: false,
            ab_other: None,
            presets: None,
            preset_list: Vec::new(),
            preset_sel: 0,
            current_preset: None,
            preset_dirty: false,
            prompt: None,
            toast: None,
            areas_graph_last: Rect::default(),
            last_tick: Instant::now(),
            save_at: None,
            monitor_sent: false,
            feedback: FeedbackDetector::new(SAMPLE_RATE as f32),
            lufs: new_lufs(),
            lufs_vals: [f64::NEG_INFINITY; 3],
            settings_sel: 0,
            show_help: false,
            theme_errors,
            feedback_hit: None,
            engine_retry_at: None,
        }
    }

    pub fn wanted_device(&self) -> Option<&str> {
        match &self.status {
            CaptureStatus::Idle => None,
            CaptureStatus::Waiting(n) | CaptureStatus::Active(n) => Some(n),
        }
    }

    /// Şu an seçili parametre (bir parametre panelinde odak varsa).
    pub fn selected_param(&self) -> Option<ParamId> {
        let Focus::Panel(p) = self.focus else {
            return None;
        };
        let panels = tab_panels(self.tab, self.eq_band);
        panels.get(p)?.ids.get(self.param_sel[self.tab][p]).copied()
    }

    /// EQ grafiğindeki bant noktaları.
    pub fn band_markers(&self) -> Vec<BandMarker> {
        let p = &self.params;
        let eq_on = p.on(ParamId::EqOn);
        (0..EQ_BANDS)
            .map(|b| {
                let g = |f| p.get(ParamId::Eq(b as u8, f));
                // Kazancı olmayan tipler (LP/HP/notch/band-pass) 0 dB çizgisinde.
                let has_gain = g(EqField::Kind) < 2.5;
                BandMarker {
                    freq: g(EqField::Freq),
                    gain_db: if has_gain { g(EqField::Gain) } else { 0.0 },
                    label: (b + 1).to_string(),
                    selected: b == self.eq_band,
                    enabled: eq_on && g(EqField::On) >= 0.5,
                }
            })
            .collect()
    }

    /// Ses motorundan gelen olayları ve metreleri günceller (her karede).
    pub fn tick(&mut self) {
        while let Ok(ev) = self.engine.events.try_recv() {
            match ev {
                AudioEvent::Devices(list) => {
                    let selected = self
                        .list
                        .selected()
                        .and_then(|i| self.devices.get(i))
                        .map(|d| d.name.clone())
                        .or_else(|| self.wanted_device().map(str::to_string))
                        .or_else(|| self.config.last_device.clone());
                    self.devices = list;
                    let idx = selected
                        .and_then(|n| self.devices.iter().position(|d| d.name == n))
                        .or(if self.devices.is_empty() {
                            None
                        } else {
                            Some(0)
                        });
                    self.list.select(idx);
                }
                AudioEvent::Capture(s) => self.status = s,
                AudioEvent::Fatal(e) => {
                    self.fatal = Some(e);
                    self.engine_down();
                }
                AudioEvent::Error(e) => self.message = Some(e),
                AudioEvent::MonitorFailed(e) => {
                    // Monitör çalışmıyor: parametreyi kapat ki açık görünmesin.
                    self.apply_param(ParamId::MonitorOn, 0.0);
                    self.message = Some(tr!(
                        format!("Monitor akışı başlatılamadı, kapatıldı: {e}"),
                        format!("Monitor stream failed and was turned off: {e}")
                    ));
                }
            }
        }
        if self.engine_retry_at.is_none() && !self.engine.is_running() {
            // Thread olay göndermeden bittiyse de motor durmuş sayılır.
            self.fatal.get_or_insert_with(|| {
                l("Ses motoru durdu.", "The audio engine stopped.").to_string()
            });
            self.engine_down();
        }
        if self.engine_retry_at.is_some_and(|t| Instant::now() >= t) {
            self.restart_engine();
        }

        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32();
        self.last_tick = now;
        self.frame_dt = dt;
        let monitor = self.params.on(ParamId::MonitorOn);
        let feedback = &mut self.feedback;
        drain_tap(
            &mut self.engine.tap_in,
            &mut self.tap_buf,
            &mut self.analyzer_in,
            |x| {
                if monitor {
                    feedback.push(x)
                }
            },
        );
        let lufs = &mut self.lufs;
        drain_tap(
            &mut self.engine.tap_out,
            &mut self.tap_buf,
            &mut self.analyzer_out,
            |x| {
                let _ = lufs.add_frames_f32(x);
            },
        );
        let v = |r: Result<f64, ebur128::Error>| r.unwrap_or(f64::NEG_INFINITY);
        self.lufs_vals = [
            v(self.lufs.loudness_momentary()),
            v(self.lufs.loudness_shortterm()),
            v(self.lufs.loudness_global()),
        ];
        self.sync_monitor(monitor, now);
        self.handle_profiles();
        let sh = &self.engine.shared;
        self.in_peak.update(sh.in_peak.take(), dt, now);
        self.in_rms.update(sh.in_rms.load(), dt, now);
        self.out_peak.update(sh.out_peak.take(), dt, now);
        self.out_rms.update(sh.out_rms.load(), dt, now);
        if self.learn_started.is_some() && !sh.noise_learning.load(Relaxed) {
            // Ses thread'i öğrenmeyi bitirdi (veya hiç başlamadı: cihaz yok).
            let elapsed = self
                .learn_started
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            if elapsed > 0.5 {
                self.learn_started = None;
            }
        }
        for (g, a) in self.gr.iter_mut().zip(&sh.gain_reduction) {
            // Anında yüksel, 30 dB/s ile düş (okunabilir gösterim).
            *g = a.take().max(*g - 30.0 * dt).max(0.0);
        }
        let clips = sh.clip_count.load(Relaxed);
        if clips != self.last_clip_count {
            self.last_clip_count = clips;
            self.clip_until = Some(now + CLIP_HOLD);
        }
        if self.clip_until.is_some_and(|t| now > t) {
            self.clip_until = None;
        }
        if self.save_at.is_some_and(|t| now >= t) {
            self.save_config();
        }
    }

    /// Motor durdu: monitörü kapat, durum bilgisini sıfırla ve yeniden başlatmayı planla.
    fn engine_down(&mut self) {
        if self.engine_retry_at.is_some() {
            return;
        }
        self.apply_param(ParamId::MonitorOn, 0.0);
        self.monitor_sent = false;
        self.status = CaptureStatus::Idle;
        self.learn_started = None;
        self.engine_retry_at = Some(Instant::now() + ENGINE_RETRY);
        if let Some(f) = &mut self.fatal {
            f.push_str(l(
                "\n\nSes motoru birkaç saniyede bir otomatik olarak yeniden başlatılacak.",
                "\n\nThe audio engine will be restarted automatically every few seconds.",
            ));
        }
    }

    /// Ses motorunu yeniden başlatmayı dener; başarısızsa daha sonra tekrar dener.
    fn restart_engine(&mut self) {
        self.engine.shutdown();
        let device = self.config.last_device.clone();
        match AudioEngine::start(device, self.params.clone(), self.config.out_channels()) {
            Ok(e) => {
                self.engine = e;
                self.engine_retry_at = None;
                self.fatal = None;
                self.init_profiles();
                self.flash(l("Ses motoru yeniden başlatıldı", "Audio engine restarted"));
            }
            Err(_) => self.engine_retry_at = Some(Instant::now() + ENGINE_RETRY),
        }
    }

    /// Monitor akışını parametreyle eşitler; geri besleme algılanırsa monitörü kapatır.
    fn sync_monitor(&mut self, monitor: bool, now: Instant) {
        if monitor != self.monitor_sent {
            self.monitor_sent = monitor;
            self.engine.set_monitor(monitor);
            self.feedback.reset();
            if monitor {
                self.flash(l(
                    "Monitor açık — yankı/ıslık olmaması için kulaklık kullanın",
                    "Monitor on — use headphones to avoid echo/howling",
                ));
            }
        }
        if monitor && let Some(freq) = self.feedback.howling() {
            self.apply_param(ParamId::MonitorOn, 0.0);
            self.feedback_hit = Some((freq, now));
            self.message = Some(tr!(
                format!(
                    "Geri besleme (ıslık) algılandı: ~{freq:.0} Hz.\n\n\
                     Monitor otomatik kapatıldı. Hoparlör yerine kulaklık kullanın \
                     veya monitor seviyesini düşürüp tekrar açın."
                ),
                format!(
                    "Feedback (howling) detected: ~{freq:.0} Hz.\n\n\
                     Monitor was turned off automatically. Use headphones instead of \
                     speakers, or lower the monitor level and turn it on again."
                )
            ));
        }
        if self
            .feedback_hit
            .is_some_and(|(_, t)| now.duration_since(t) > Duration::from_secs(10))
        {
            self.feedback_hit = None;
        }
    }

    /// Ayarlar sekmesindeki bir satırı sıradaki değere geçirir (hepsi iki seçenekli).
    pub fn change_setting(&mut self, row: usize) {
        match row {
            0 => {
                let en = !i18n::en();
                i18n::set_en(en);
                self.config.language = Some(if en { "en" } else { "tr" }.into());
            }
            1 => {
                let light = !self.config.light_theme();
                self.config.theme = Some(if light { "light" } else { "dark" }.into());
                let (t, errors) = Theme::build(light, &self.config.theme_colors);
                theme::set(t);
                self.theme_errors = errors;
            }
            2 => {
                let n = if self.config.out_channels() == 2 {
                    1
                } else {
                    2
                };
                self.config.output_channels = Some(n);
                self.engine.set_output_channels(n);
                self.flash(if n == 1 {
                    i18n::l(
                        "Sanal mikrofon mono — uygulamalar mikrofonu yeniden seçmek zorunda kalabilir",
                        "Virtual mic is mono — apps may need to reselect the microphone",
                    )
                } else {
                    i18n::l(
                        "Sanal mikrofon stereo — uygulamalar mikrofonu yeniden seçmek zorunda kalabilir",
                        "Virtual mic is stereo — apps may need to reselect the microphone",
                    )
                });
            }
            _ => return,
        }
        self.save_config();
    }

    pub fn muted(&self) -> bool {
        self.engine.shared.mute.load(Relaxed)
    }

    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.key(k),
            Event::Mouse(m) => self.mouse(m),
            _ => {}
        }
    }

    fn key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.show_notice {
            if matches!(k.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ')) {
                self.dismiss_notice();
            }
            return;
        }
        if self.message.is_some() || self.fatal.is_some() {
            if matches!(k.code, KeyCode::Esc | KeyCode::Enter) {
                self.message = None;
                self.fatal = None;
            }
            return;
        }
        if self.show_help {
            if matches!(
                k.code,
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                self.show_help = false;
            }
            return;
        }
        if self.prompt.is_some() {
            self.prompt_key(k);
            return;
        }
        if self.entry.is_some() {
            self.entry_key(k);
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('r') {
            self.redo();
            return;
        }
        let fine = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('j') | KeyCode::Down if self.tab == 7 => {
                self.settings_sel = (self.settings_sel + 1).min(SETTINGS_COUNT - 1)
            }
            KeyCode::Char('k') | KeyCode::Up if self.tab == 7 => {
                self.settings_sel = self.settings_sel.saturating_sub(1)
            }
            KeyCode::Left if self.tab == 7 => self.change_setting(self.settings_sel),
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('b') if self.tab == 7 => {
                self.change_setting(self.settings_sel)
            }
            KeyCode::Char('i') if self.tab == 0 => {
                self.lufs.reset();
                self.flash(i18n::l("Entegre LUFS sıfırlandı", "Integrated LUFS reset"));
            }
            KeyCode::Char(' ') => self.toggle_ab(),
            KeyCode::Char('j') | KeyCode::Down if self.tab == 6 => {
                self.preset_sel =
                    (self.preset_sel + 1).min(self.preset_list.len().saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up if self.tab == 6 => {
                self.preset_sel = self.preset_sel.saturating_sub(1)
            }
            KeyCode::Enter if self.tab == 6 => self.load_selected_preset(),
            KeyCode::Char('n') if self.tab == 6 => self.open_prompt(PromptKind::SaveAs, ""),
            KeyCode::Char('s') if self.tab == 6 => {
                if let Some(p) = self.selected_preset_path() {
                    self.open_prompt(PromptKind::ConfirmSave(p), "")
                }
            }
            KeyCode::Char('c') if self.tab == 6 => self.duplicate_selected(),
            KeyCode::Char('d') | KeyCode::Delete if self.tab == 6 => {
                if let Some(p) = self.selected_preset_path() {
                    self.open_prompt(PromptKind::ConfirmDelete(p), "")
                }
            }
            KeyCode::Char('i') if self.tab == 6 => self.open_prompt(PromptKind::Import, "~/"),
            KeyCode::Char('e') if self.tab == 6 => {
                if let Some(p) = self.selected_preset_path() {
                    self.open_prompt(PromptKind::Export(p), "~/")
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('m') => {
                let m = &self.engine.shared.mute;
                m.store(!m.load(Relaxed), Relaxed);
            }
            KeyCode::Tab => self.set_tab((self.tab + 1) % TABS.len()),
            KeyCode::BackTab => self.set_tab((self.tab + TABS.len() - 1) % TABS.len()),
            KeyCode::Char(c @ '1'..='8') => self.set_tab(c as usize - '1' as usize),
            KeyCode::Char('h') => self.move_focus(-1),
            KeyCode::Char('l') => self.move_focus(1),
            KeyCode::Char('[') if self.tab == 1 => self.select_band(self.eq_band + EQ_BANDS - 1),
            KeyCode::Char(']') if self.tab == 1 => self.select_band(self.eq_band + 1),
            KeyCode::Char('p') if self.tab == 3 => self.start_learning(),
            KeyCode::Char('J') if self.tab == 5 => self.move_module(1),
            KeyCode::Char('K') if self.tab == 5 => self.move_module(-1),
            KeyCode::Down if self.tab == 5 && k.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_module(1)
            }
            KeyCode::Up if self.tab == 5 && k.modifiers.contains(KeyModifiers::SHIFT) => {
                self.move_module(-1)
            }
            KeyCode::Char('j') | KeyCode::Down if self.tab == 5 => {
                self.chain_sel = (self.chain_sel + 1).min(MODULE_COUNT - 1)
            }
            KeyCode::Char('k') | KeyCode::Up if self.tab == 5 => {
                self.chain_sel = self.chain_sel.saturating_sub(1)
            }
            KeyCode::Char('b') | KeyCode::Enter if self.tab == 5 => self.toggle_chain_module(),
            KeyCode::Char('r') if self.tab == 5 => self.set_order(DEFAULT_ORDER),
            KeyCode::Char('s') if self.tab == 1 => {
                self.spectrum_mode = match self.spectrum_mode {
                    SpectrumMode::Overlay => SpectrumMode::Split,
                    SpectrumMode::Split => SpectrumMode::Off,
                    SpectrumMode::Off => SpectrumMode::Overlay,
                };
                self.analyzer_in.reset();
                self.analyzer_out.reset();
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Left => self.adjust(-1.0, fine),
            KeyCode::Right => self.adjust(1.0, fine),
            KeyCode::Enter => match self.selected_param() {
                Some(id) => self.activate_param(id),
                None if self.tab == 0 => self.connect_selected(),
                None => {}
            },
            KeyCode::Char('b') => self.toggle_bypass(),
            KeyCode::Char('r') => {
                if let Some(id) = self.selected_param() {
                    self.set_param(id, id.spec().default);
                }
            }
            KeyCode::Char('d') if self.tab == 0 => self.select_device(None),
            _ => {}
        }
    }

    fn entry_key(&mut self, k: KeyEvent) {
        let Some((id, text)) = &mut self.entry else {
            return;
        };
        match k.code {
            KeyCode::Esc => self.entry = None,
            KeyCode::Enter => {
                let id = *id;
                match text.trim().replace(',', ".").parse::<f32>() {
                    Ok(v) => {
                        self.set_param(id, v);
                        self.entry = None;
                    }
                    Err(_) => text.clear(),
                }
            }
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(c)
                if text.len() < 12
                    && (c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+')) =>
            {
                text.push(c);
            }
            _ => {}
        }
    }

    /// Klavye/fare girişini kendine alan bir pencere açık mı.
    fn modal_open(&self) -> bool {
        self.show_notice
            || self.show_help
            || self.entry.is_some()
            || self.prompt.is_some()
            || self.message.is_some()
            || self.fatal.is_some()
    }

    fn mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        if self.modal_open() {
            // Pencere açıkken arkadaki arayüz fareye tepki vermez; tıklama yalnızca
            // bilgi ve yardım pencerelerini kapatır.
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                if self.show_notice {
                    self.dismiss_notice();
                } else if self.show_help {
                    self.show_help = false;
                }
            }
            self.dragging = None;
            self.dragging_band = None;
            self.chain_drag = None;
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left)
                if self.tab == 1 && self.areas.graph.contains(pos) =>
            {
                let markers = self.band_markers();
                if let Some(b) =
                    graph::hit_marker(self.areas.graph, &self.view, &markers, m.column, m.row)
                {
                    self.select_band(b);
                    self.focus = Focus::Panel(1);
                    self.dragging_band = Some(b);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.chain_drag.is_some() => {
                // Satır üzerinden geçtikçe modülü o konuma taşı.
                if let (Some(from), Some(to)) = (
                    self.chain_drag,
                    self.areas.chain_rows.iter().position(|r| r.y == m.row),
                ) && from != to
                {
                    let mut o = self.params.order();
                    let module = o[from];
                    if from < to {
                        o.copy_within(from + 1..=to, from);
                    } else {
                        o.copy_within(to..from, to + 1);
                    }
                    o[to] = module;
                    self.set_order(o);
                    self.chain_sel = to;
                    self.chain_drag = Some(to);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_band.is_some() => {
                self.drag_band(m.column, m.row)
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                if self.tab == 1 && self.areas.graph.contains(pos) =>
            {
                // Grafik üzerinde tekerlek: seçili bandın Q değeri.
                let dir = if m.kind == MouseEventKind::ScrollUp {
                    1.0
                } else {
                    -1.0
                };
                let id = ParamId::Eq(self.eq_band as u8, EqField::Q);
                let fine = m.modifiers.contains(KeyModifiers::SHIFT);
                self.set_param(id, id.spec().step(self.params.get(id), dir, fine));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.tab == 7
                    && let Some(i) = self
                        .areas
                        .settings_rows
                        .iter()
                        .position(|r| r.contains(pos))
                {
                    self.settings_sel = i;
                    self.change_setting(i);
                    return;
                }
                if self.tab == 6
                    && let Some(i) = self.areas.preset_rows.iter().position(|r| r.contains(pos))
                {
                    // İkinci tıklama (zaten seçiliyse) yükler.
                    if self.preset_sel == i {
                        self.load_selected_preset();
                    } else {
                        self.preset_sel = i;
                    }
                    return;
                }
                if self.tab == 5
                    && let Some(i) = self.areas.chain_rows.iter().position(|r| r.contains(pos))
                {
                    self.chain_sel = i;
                    self.chain_drag = Some(i);
                    return;
                }
                if self.tab == 3 && self.learn_button.contains(pos) {
                    self.start_learning();
                    return;
                }
                if let Some(i) = self.areas.tabs.iter().position(|r| r.contains(pos)) {
                    self.set_tab(i);
                    return;
                }
                if let Some(&(id, _, bar, panel, i)) =
                    self.areas.params.iter().find(|r| r.1.contains(pos))
                {
                    self.focus = Focus::Panel(panel);
                    self.param_sel[self.tab][panel] = i;
                    match id.spec().scale {
                        Scale::Toggle | Scale::Choice(_) => self.activate_param(id),
                        _ if bar.contains(pos) => {
                            self.dragging = Some((id, bar));
                            self.drag_to(m.column);
                        }
                        _ => {}
                    }
                    return;
                }
                let area = self.areas.devices;
                if area.contains(pos) && m.row > area.y {
                    self.focus = Focus::Devices;
                    let row = (m.row - area.y - 1) as usize + self.list.offset();
                    if row < self.devices.len() {
                        self.list.select(Some(row));
                        self.connect_selected();
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => self.drag_to(m.column),
            MouseEventKind::Up(MouseButton::Left) => {
                // Sürükleme bitti: sonraki değişiklik yeni bir geri alma adımı olsun.
                self.history.seal();
                self.dragging = None;
                self.dragging_band = None;
                self.chain_drag = None;
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let dir = if m.kind == MouseEventKind::ScrollUp {
                    1.0
                } else {
                    -1.0
                };
                let fine = m.modifiers.contains(KeyModifiers::SHIFT);
                if let Some(&(id, ..)) = self.areas.params.iter().find(|r| r.1.contains(pos)) {
                    self.set_param(id, id.spec().step(self.params.get(id), dir, fine));
                } else if self.areas.devices.contains(pos) {
                    self.focus = Focus::Devices;
                    self.move_selection(-dir as i32);
                }
            }
            _ => {}
        }
    }

    fn drag_to(&mut self, column: u16) {
        if let Some((id, bar)) = self.dragging {
            let n = (column.saturating_sub(bar.x) as f32 + 0.5) / bar.width.max(1) as f32;
            self.set_param(id, id.spec().from_norm(n));
        }
    }

    fn set_tab(&mut self, tab: usize) {
        self.tab = tab;
        let n = tab_panels(tab, self.eq_band).len();
        self.focus = match self.focus {
            Focus::Devices if tab == 0 => Focus::Devices,
            Focus::Panel(p) if p < n => Focus::Panel(p),
            _ if tab == 0 => Focus::Devices,
            _ => Focus::Panel(0),
        };
    }

    /// h/l: önceki/sonraki panel (Genel'de cihaz listesi en soldadır).
    fn move_focus(&mut self, dir: i32) {
        let n = tab_panels(self.tab, self.eq_band).len() as i32;
        if n == 0 {
            return;
        }
        // Cihaz listesi -1 konumunda sayılır.
        let min = if self.tab == 0 { -1 } else { 0 };
        let cur = match self.focus {
            Focus::Devices => -1,
            Focus::Panel(p) => p as i32,
        };
        let next = (cur + dir).clamp(min, n - 1);
        self.focus = if next < 0 {
            Focus::Devices
        } else {
            Focus::Panel(next as usize)
        };
    }

    /// Gürültü profili öğrenmeyi başlatır (ses thread'i 3 sn dinler).
    fn start_learning(&mut self) {
        if self.learn_started.is_some() {
            return;
        }
        if !matches!(self.status, CaptureStatus::Active(_)) {
            self.message = Some(
                l(
                    "Profil öğrenmek için önce bir giriş cihazı bağlayın.",
                    "Connect an input device first to learn a noise profile.",
                )
                .into(),
            );
            return;
        }
        self.engine.shared.learn_request.fetch_add(1, Relaxed);
        self.learn_started = Some(Instant::now());
    }

    /// Öğrenme geri sayımı (saniye), öğrenme sürmüyorsa `None`.
    pub fn learn_remaining(&self) -> Option<f32> {
        self.learn_started
            .map(|t| (NOISE_LEARN_SECONDS - t.elapsed().as_secs_f32()).max(0.0))
    }

    /// Kayıtlı profili ses thread'ine gönderir ve öğrenme için boş kutu verir.
    pub fn init_profiles(&mut self) {
        if let Some(p) = &self.config.noise_profile
            && p.len() == NOISE_PROFILE_LEN
            && p.iter().all(|v| v.is_finite() && *v >= 0.0)
        {
            let mut b = ProfileBox::new(ProfileKind::Load);
            b.data.copy_from_slice(p);
            let _ = self.engine.profile_tx.push(b);
        }
        let _ = self
            .engine
            .profile_tx
            .push(ProfileBox::new(ProfileKind::Spare));
    }

    /// Ses thread'inden dönen profil kutularını işler; yeni profili kaydeder.
    fn handle_profiles(&mut self) {
        while let Ok(mut b) = self.engine.profile_rx.pop() {
            match b.kind {
                ProfileKind::Learned => {
                    self.config.noise_profile = Some(b.data.to_vec());
                    self.save_config();
                    b.kind = ProfileKind::Spare;
                    let _ = self.engine.profile_tx.push(b);
                }
                ProfileKind::Spare => {
                    let _ = self.engine.profile_tx.push(b);
                }
                // Yüklenen profilin kutusu geri döndü; burada (UI thread) serbest bırakılır.
                ProfileKind::Load => {}
            }
        }
    }

    /// Seçili modülü sırada `dir` (±1) kadar kaydırır.
    fn move_module(&mut self, dir: i32) {
        let to = self.chain_sel as i32 + dir;
        if !(0..MODULE_COUNT as i32).contains(&to) {
            return;
        }
        let mut o = self.params.order();
        o.swap(self.chain_sel, to as usize);
        self.set_order(o);
        self.chain_sel = to as usize;
    }

    fn toggle_chain_module(&mut self) {
        let module = self.params.order()[self.chain_sel] as usize;
        let t = MODULE_TOGGLES[module];
        self.set_param(t, 1.0 - self.params.get(t));
    }

    /// Kullanıcı değişikliği: sırayı uygular ve geri alma geçmişine yazar.
    pub fn set_order(&mut self, order: [u8; MODULE_COUNT]) {
        let changes = self.apply_order(order);
        if !changes.is_empty() {
            self.history.record_bulk(changes, Instant::now());
            self.preset_dirty = true;
        }
    }

    /// Modül sırasını uygular ve config'e yazar (geçmişe yazmaz). Değişen sıra
    /// konumlarını geçmiş kaydı biçiminde döner.
    fn apply_order(&mut self, order: [u8; MODULE_COUNT]) -> Vec<(HistKey, f32, f32)> {
        let old = self.params.order();
        if !self.params.set_order(&order) {
            return Vec::new();
        }
        self.config.chain_order = Some(
            order
                .iter()
                .map(|&m| MODULE_KEYS[m as usize].to_string())
                .collect(),
        );
        self.save_at.get_or_insert(Instant::now() + SAVE_DELAY);
        (0..MODULE_COUNT)
            .filter(|&i| old[i] != order[i])
            .map(|i| (HistKey::Order(i as u8), old[i] as f32, order[i] as f32))
            .collect()
    }

    /// Geçmişten gelen değerleri uygular (geri al / yinele).
    fn apply_history(&mut self, changes: Vec<(HistKey, f32)>) {
        let mut order = self.params.order();
        let mut order_changed = false;
        for (k, v) in changes {
            match k {
                HistKey::Param(id) => {
                    self.apply_param(id, v);
                }
                HistKey::Order(i) => {
                    order[i as usize] = v as u8;
                    order_changed = true;
                }
            }
        }
        if order_changed {
            self.apply_order(order);
        }
    }

    fn select_band(&mut self, band: usize) {
        self.eq_band = band % EQ_BANDS;
    }

    /// Grafikte sürüklenen bandın frekansını (x) ve kazancını (y) ayarlar.
    fn drag_band(&mut self, x: u16, y: u16) {
        let Some(b) = self.dragging_band else { return };
        let plot = self.areas.graph;
        let x = x.clamp(plot.x, plot.right().saturating_sub(1));
        let y = y.clamp(plot.y, plot.bottom().saturating_sub(1));
        self.set_param(
            ParamId::Eq(b as u8, EqField::Freq),
            graph::x_to_freq(plot, &self.view, x),
        );
        if self.params.get(ParamId::Eq(b as u8, EqField::Kind)) < 2.5 {
            self.set_param(
                ParamId::Eq(b as u8, EqField::Gain),
                graph::y_to_gain(plot, &self.view, y),
            );
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.focus == Focus::Devices {
            if self.devices.is_empty() {
                return;
            }
            let cur = self.list.selected().unwrap_or(0) as i32;
            let next = (cur + delta).clamp(0, self.devices.len() as i32 - 1);
            self.list.select(Some(next as usize));
        } else if let Focus::Panel(p) = self.focus
            && let Some(panel) = tab_panels(self.tab, self.eq_band).get(p)
        {
            let sel = &mut self.param_sel[self.tab][p];
            *sel = (*sel as i32 + delta).clamp(0, panel.ids.len() as i32 - 1) as usize;
        }
    }

    fn adjust(&mut self, dir: f32, fine: bool) {
        if let Some(id) = self.selected_param() {
            self.set_param(id, id.spec().step(self.params.get(id), dir, fine));
        }
    }

    /// Enter: açma/kapama ve seçenekleri değiştirir, sürekli değerlerde sayı girişi açar.
    fn activate_param(&mut self, id: ParamId) {
        match id.spec().scale {
            Scale::Toggle | Scale::Choice(_) => {
                self.set_param(id, id.spec().step(self.params.get(id), 1.0, false))
            }
            _ => self.entry = Some((id, String::new())),
        }
    }

    fn toggle_bypass(&mut self) {
        match self.selected_param().and_then(ParamId::module_toggle) {
            Some(t) => self.set_param(t, 1.0 - self.params.get(t)),
            None => {
                self.message = Some(
                    l(
                        "Seçili parametrenin bypass edilebilir bir modülü yok.",
                        "The selected parameter has no module to bypass.",
                    )
                    .into(),
                )
            }
        }
    }

    /// Kullanıcı değişikliği: uygular ve geri alma geçmişine yazar.
    pub fn set_param(&mut self, id: ParamId, v: f32) {
        let old = self.params.get(id);
        let v = self.apply_param(id, v);
        if v != old && id.is_sound() {
            self.history
                .record(HistKey::Param(id), old, v, Instant::now());
            self.preset_dirty = true;
        }
    }

    /// Değeri uygular ve kalıcı config'e işler (geçmişe yazmaz).
    fn apply_param(&mut self, id: ParamId, v: f32) -> f32 {
        let v = self.params.set(id, v);
        let key = id.spec().key.clone();
        if (v - id.spec().default).abs() > 1e-6 {
            self.config.params.insert(key, v);
        } else {
            self.config.params.remove(&key);
        }
        self.save_at.get_or_insert(Instant::now() + SAVE_DELAY);
        v
    }

    /// Toplu değer uygulama (preset, A/B); değişiklikleri geçmiş kaydı biçiminde döner.
    fn apply_values(&mut self, values: &[(ParamId, f32)]) -> Vec<(HistKey, f32, f32)> {
        let mut changes = Vec::new();
        for &(id, v) in values {
            let old = self.params.get(id);
            let new = self.apply_param(id, v);
            changes.push((HistKey::Param(id), old, new));
        }
        changes
    }

    fn current_values(&self) -> Vec<(ParamId, f32)> {
        ParamId::sound()
            .map(|id| (id, self.params.get(id)))
            .collect()
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now() + Duration::from_secs(3)));
    }

    /// Geri alınabilir / yinelenebilir adım sayıları.
    pub fn history_lens(&self) -> (usize, usize) {
        (self.history.undo_len(), self.history.redo_len())
    }

    fn undo(&mut self) {
        match self.history.undo() {
            Some(ch) => {
                self.apply_history(ch);
                self.preset_dirty = true;
                self.flash(l("Geri alındı", "Undone"));
            }
            None => self.flash(l("Geri alınacak değişiklik yok", "Nothing to undo")),
        }
    }

    fn redo(&mut self) {
        match self.history.redo() {
            Some(ch) => {
                self.apply_history(ch);
                self.preset_dirty = true;
                self.flash(l("Yinelendi", "Redone"));
            }
            None => self.flash(l("Yinelenecek değişiklik yok", "Nothing to redo")),
        }
    }

    /// A/B karşılaştırma: iki ayar kümesi arasında geçiş. B ilk geçişte A'nın kopyasıdır.
    fn toggle_ab(&mut self) {
        let cur = AbSlot {
            values: ParamId::sound().map(|id| self.params.get(id)).collect(),
            order: self.params.order(),
            history: std::mem::replace(&mut self.history, new_history()),
        };
        let other = self.ab_other.take().unwrap_or_else(|| AbSlot {
            values: cur.values.clone(),
            order: cur.order,
            history: new_history(),
        });
        let vals: Vec<(ParamId, f32)> =
            ParamId::sound().zip(other.values.iter().copied()).collect();
        self.apply_values(&vals);
        self.apply_order(other.order);
        self.history = other.history;
        self.ab_other = Some(cur);
        self.ab_is_b = !self.ab_is_b;
        let name = if self.ab_is_b { "B" } else { "A" };
        self.flash(tr!(
            format!("A/B: {name} ayarları etkin"),
            format!("A/B: settings {name} active")
        ));
    }

    // ---- Presetler ----

    /// Preset klasörünü hazırlar (hazır presetler ilk açılışta bir kez yazılır).
    pub fn init_presets(&mut self) {
        let Some(dir) = PresetStore::default_dir() else {
            return;
        };
        let store = PresetStore::new(dir);
        if !self.config.builtin_presets_installed {
            match store.install_builtins() {
                Ok(_) => {
                    self.config.builtin_presets_installed = true;
                    self.save_config();
                }
                Err(e) => {
                    self.message = Some(tr!(
                        format!("Hazır presetler yazılamadı: {e}"),
                        format!("Could not write built-in presets: {e}")
                    ))
                }
            }
        }
        self.presets = Some(store);
        self.current_preset = self.config.last_preset.clone();
        let warnings = self.refresh_presets();
        if !warnings.is_empty() && self.message.is_none() {
            self.message = Some(format!(
                "{}\n{}",
                l(
                    "Bazı presetler okunamadı:",
                    "Some presets could not be read:"
                ),
                warnings.join("\n")
            ));
        }
    }

    fn refresh_presets(&mut self) -> Vec<String> {
        let Some(store) = &self.presets else {
            return vec![];
        };
        let (list, warnings) = store.list();
        self.preset_list = list
            .into_iter()
            .map(|e| {
                let desc = store
                    .load(&e.path)
                    .map(|p| p.description)
                    .unwrap_or_default();
                (e, desc)
            })
            .collect();
        self.preset_sel = self
            .preset_sel
            .min(self.preset_list.len().saturating_sub(1));
        warnings
    }

    fn selected_preset_path(&self) -> Option<std::path::PathBuf> {
        self.preset_list
            .get(self.preset_sel)
            .map(|(e, _)| e.path.clone())
    }

    fn select_preset_named(&mut self, name: &str) {
        if let Some(i) = self.preset_list.iter().position(|(e, _)| e.name == name) {
            self.preset_sel = i;
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, text: &str) {
        self.prompt = Some(Prompt {
            kind,
            text: text.to_string(),
        });
    }

    fn load_selected_preset(&mut self) {
        let (Some(store), Some(path)) = (&self.presets, self.selected_preset_path()) else {
            return;
        };
        let preset = match store.load(&path) {
            Ok(p) => p,
            Err(e) => {
                self.message = Some(e);
                return;
            }
        };
        let values = preset.resolved_values();
        let mut changes = self.apply_values(&values);
        let order = preset
            .chain_order
            .as_ref()
            .and_then(|keys| order_from_keys(keys))
            .unwrap_or(DEFAULT_ORDER);
        changes.extend(self.apply_order(order));
        // Değerler ve sıra tek adımda geri alınır.
        self.history.record_bulk(changes, Instant::now());
        self.current_preset = Some(preset.name.clone());
        self.config.last_preset = Some(preset.name.clone());
        self.preset_dirty = false;
        self.save_config();
        self.flash(tr!(
            format!("Preset yüklendi: {}", preset.name),
            format!("Preset loaded: {}", preset.name)
        ));
    }

    fn chain_order_keys(&self) -> Vec<String> {
        self.params
            .order()
            .iter()
            .map(|&m| PRESET_MODULE_KEYS[m as usize].to_string())
            .collect()
    }

    fn save_preset(&mut self, name: &str, description: &str) {
        let Some(store) = &self.presets else {
            return;
        };
        let preset = Preset::from_values(
            name,
            description,
            &self.current_values(),
            Some(self.chain_order_keys()),
        );
        match store.save(&preset) {
            Ok(_) => {
                self.current_preset = Some(name.to_string());
                self.config.last_preset = Some(name.to_string());
                self.preset_dirty = false;
                self.save_config();
                self.refresh_presets();
                self.select_preset_named(name);
                self.flash(tr!(format!("Kaydedildi: {name}"), format!("Saved: {name}")));
            }
            Err(e) => self.message = Some(e),
        }
    }

    fn duplicate_selected(&mut self) {
        let (Some(store), Some(path)) = (&self.presets, self.selected_preset_path()) else {
            return;
        };
        match store.duplicate(&path).and_then(|p| store.load(&p)) {
            Ok(p) => {
                self.refresh_presets();
                self.select_preset_named(&p.name);
                self.flash(tr!(
                    format!("Kopyalandı: {}", p.name),
                    format!("Duplicated: {}", p.name)
                ));
            }
            Err(e) => self.message = Some(e),
        }
    }

    fn prompt_key(&mut self, k: KeyEvent) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let confirm = matches!(
            prompt.kind,
            PromptKind::ConfirmDelete(_)
                | PromptKind::ConfirmSave(_)
                | PromptKind::ConfirmSaveAs(_)
                | PromptKind::ConfirmOverwriteExport(..)
        );
        match k.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Char('n') | KeyCode::Char('h') if confirm => self.prompt = None,
            KeyCode::Char('e') | KeyCode::Char('y') | KeyCode::Enter if confirm => {
                let kind = prompt.kind.clone();
                self.prompt = None;
                self.confirm_prompt(kind, String::new());
            }
            KeyCode::Enter => {
                let kind = prompt.kind.clone();
                let text = prompt.text.trim().to_string();
                self.prompt = None;
                if !text.is_empty() {
                    self.confirm_prompt(kind, text);
                }
            }
            KeyCode::Backspace if !confirm => {
                prompt.text.pop();
            }
            KeyCode::Char(c) if !confirm && prompt.text.chars().count() < 200 => {
                prompt.text.push(c)
            }
            _ => {}
        }
    }

    fn confirm_prompt(&mut self, kind: PromptKind, text: String) {
        let Some(store) = &self.presets else {
            return;
        };
        match kind {
            PromptKind::SaveAs if store.name_taken(&text) => {
                self.open_prompt(PromptKind::ConfirmSaveAs(text), "")
            }
            PromptKind::SaveAs => self.save_preset(&text, ""),
            PromptKind::ConfirmSaveAs(name) => self.save_preset(&name, ""),
            PromptKind::ConfirmSave(path) => match store.load(&path) {
                Ok(p) => self.save_preset(&p.name, &p.description),
                Err(e) => self.message = Some(e),
            },
            PromptKind::ConfirmDelete(path) => match store.delete(&path) {
                Ok(()) => {
                    self.refresh_presets();
                    self.flash(l("Preset silindi", "Preset deleted"));
                }
                Err(e) => self.message = Some(e),
            },
            PromptKind::Import => match store.import(std::path::Path::new(&text)) {
                Ok(p) => {
                    let name = store.load(&p).map(|p| p.name).unwrap_or_default();
                    self.refresh_presets();
                    self.select_preset_named(&name);
                    self.flash(tr!(
                        format!("İçe aktarıldı: {name}"),
                        format!("Imported: {name}")
                    ));
                }
                Err(e) => self.message = Some(e),
            },
            PromptKind::Export(path) => {
                let mut dest = expand_home(&text);
                // Klasöre aktarılıyorsa hedef dosya <slug>.toml olur; çakışmayı önceden sor.
                if dest.is_dir()
                    && let Ok(p) = store.load(&path)
                {
                    dest = dest.join(format!("{}.toml", crate::preset::slug(&p.name)));
                }
                if dest.is_file() {
                    self.open_prompt(PromptKind::ConfirmOverwriteExport(path, dest), "");
                } else {
                    self.export(&path, &dest, false);
                }
            }
            PromptKind::ConfirmOverwriteExport(path, dest) => self.export(&path, &dest, true),
        }
    }

    fn export(&mut self, path: &std::path::Path, dest: &std::path::Path, overwrite: bool) {
        let Some(store) = &self.presets else {
            return;
        };
        match store.export(path, dest, overwrite) {
            Ok(p) => self.flash(tr!(
                format!("Dışa aktarıldı: {}", p.display()),
                format!("Exported: {}", p.display())
            )),
            Err(e) => self.message = Some(e),
        }
    }

    fn connect_selected(&mut self) {
        if let Some(d) = self.list.selected().and_then(|i| self.devices.get(i)) {
            let name = d.name.clone();
            self.select_device(Some(name));
        }
    }

    fn select_device(&mut self, name: Option<String>) {
        self.engine.select_device(name.clone());
        self.config.last_device = name;
        self.save_config();
    }

    fn dismiss_notice(&mut self) {
        self.show_notice = false;
        self.config.first_run_notice_shown = true;
        self.save_config();
    }

    pub fn save_config(&mut self) {
        self.save_at = None;
        if let Err(e) = self.config.save() {
            self.message = Some(tr!(
                format!("Config kaydedilemedi: {e}"),
                format!("Could not save config: {e}")
            ));
        }
    }
}

/// Ses thread'inden gelen spektrum örneklerini analizöre (ve istenirse geri
/// besleme dedektörüne) aktarır.
fn drain_tap(
    tap: &mut rtrb::Consumer<f32>,
    scratch: &mut [f32],
    an: &mut SpectrumAnalyzer,
    mut sink: impl FnMut(&[f32]),
) {
    loop {
        let n = tap.slots().min(scratch.len());
        if n == 0 {
            break;
        }
        let Ok(chunk) = tap.read_chunk(n) else { break };
        let (a, b) = chunk.as_slices();
        scratch[..a.len()].copy_from_slice(a);
        scratch[a.len()..a.len() + b.len()].copy_from_slice(b);
        chunk.commit_all();
        an.push(&scratch[..n]);
        sink(&scratch[..n]);
    }
}

/// Modül anahtarlarından sıra; geçersizse `None`. Eski sürümlerde olmayan
/// modüller (ör. FAZ 7 efektleri) varsayılan sırayla sona eklenir.
pub fn order_from_keys(keys: &[String]) -> Option<[u8; MODULE_COUNT]> {
    let mut v = Vec::with_capacity(MODULE_COUNT);
    for k in keys {
        let i = MODULE_KEYS.iter().position(|m| m == k)? as u8;
        if v.contains(&i) {
            return None;
        }
        v.push(i);
    }
    for i in DEFAULT_ORDER {
        if !v.contains(&i) {
            v.push(i);
        }
    }
    let o = <[u8; MODULE_COUNT]>::try_from(v).ok()?;
    micforge_dsp::params::is_valid_order(&o).then_some(o)
}

/// Çıkış için LUFS ölçer: tek kanal, çift mono (stereo eşdeğeri).
fn new_lufs() -> EbuR128 {
    use ebur128::{Channel, Mode};
    let mut m =
        EbuR128::new(1, SAMPLE_RATE, Mode::M | Mode::S | Mode::I).expect("geçerli EBU R128 ayarı");
    let _ = m.set_channel(0, Channel::DualMono);
    m
}

/// Ayarlar sekmesindeki satır sayısı.
pub const SETTINGS_COUNT: usize = 3;

/// Baştaki `~` işaretini ev dizinine çevirir.
fn expand_home(s: &str) -> std::path::PathBuf {
    match (s.strip_prefix('~'), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest.trim_start_matches('/')),
        _ => std::path::PathBuf::from(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_names_in_both_languages() {
        assert_eq!(TABS.len(), TABS_EN.len());
        for (i, tr) in TABS.iter().enumerate() {
            assert_eq!(tab_name_lang(i, false), *tr);
            assert!(!tab_name_lang(i, true).is_empty());
        }
        assert_eq!(tab_name_lang(2, false), "Dinamikler");
        assert_eq!(tab_name_lang(2, true), "Dynamics");
    }

    #[test]
    fn old_orders_get_new_modules_appended() {
        let old: Vec<String> = MODULE_KEYS[..10]
            .iter()
            .rev()
            .map(|s| s.to_string())
            .collect();
        let o = order_from_keys(&old).unwrap();
        assert_eq!(&o[..10], &[9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(&o[10..], &[10, 11, 12, 13, 14]);
        assert!(order_from_keys(&["hpf".into(), "hpf".into()]).is_none());
        assert!(order_from_keys(&["yok".into()]).is_none());
    }

    fn key(app: &mut App, c: KeyCode) {
        app.handle(Event::Key(KeyEvent::new(c, KeyModifiers::NONE)));
    }

    fn click(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        app.handle(Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }));
    }

    /// Gerçek ses motoruyla uygulama davranışı. Kullanıcının config'ine
    /// dokunmamak için XDG_CONFIG_HOME geçici bir dizine yönlendirilir.
    /// Çalıştırma: cargo test -p micforge-tui -- --ignored app_behaviour
    #[test]
    #[ignore = "çalışan bir PipeWire oturumu gerekir"]
    fn app_behaviour_with_engine() {
        let tmp = std::env::temp_dir().join(format!("micforge-app-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: Bu test dosyadaki ortam değişkenine dokunan tek test.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let params = Arc::new(ParamStore::default());
        let engine = AudioEngine::start(None, params.clone(), 2).expect("PipeWire");
        let mut app = App::new(engine, params, Config::default(), None);
        app.show_notice = false;
        let store = PresetStore::new(tmp.join("presets"));
        app.presets = Some(store);
        app.tab = 6;

        // 2: "Farklı kaydet" mevcut adı sorar, onaysız ezmez.
        app.save_preset("Deneme", "ilk");
        let path = app.presets.as_ref().unwrap().dir().join("deneme.toml");
        let before = std::fs::read_to_string(&path).unwrap();
        app.set_param(ParamId::InputGain, 6.0);
        key(&mut app, KeyCode::Char('n'));
        for c in "Deneme".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.prompt.as_ref().map(|p| &p.kind),
            Some(PromptKind::ConfirmSaveAs(n)) if n == "Deneme"
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        key(&mut app, KeyCode::Char('h'));
        assert!(app.prompt.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        key(&mut app, KeyCode::Char('n'));
        for c in "Deneme".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Enter);
        assert_ne!(std::fs::read_to_string(&path).unwrap(), before);

        // 3: Preset yükleme sırayı da değiştirir; geri alma sırayı geri getirir.
        let mut custom = DEFAULT_ORDER;
        custom.swap(0, 1);
        app.set_order(custom);
        assert!(app.preset_dirty, "set_order preset'i kirli işaretlemeli");
        app.set_param(ParamId::InputGain, 0.0);
        app.save_preset("Ozel", "");
        app.set_order(DEFAULT_ORDER);
        app.set_param(ParamId::InputGain, 3.0);
        app.select_preset_named("Ozel");
        app.load_selected_preset();
        assert_eq!(app.params.order(), custom);
        app.undo();
        assert_eq!(app.params.order(), DEFAULT_ORDER);
        assert_eq!(app.params.get(ParamId::InputGain), 3.0);
        app.redo();
        assert_eq!(app.params.order(), custom);
        app.undo();
        app.undo(); // InputGain 0 -> 3
        app.undo(); // sıra: custom -> DEFAULT geri alınır
        assert_eq!(app.params.order(), custom);

        // 7: Yardım açıkken fare arkadaki arayüzü değiştirmez.
        app.areas.params = vec![(
            ParamId::InputGain,
            Rect::new(0, 10, 40, 1),
            Rect::new(10, 10, 30, 1),
            0,
            0,
        )];
        let g = app.params.get(ParamId::InputGain);
        app.show_help = true;
        click(&mut app, MouseEventKind::ScrollUp, 15, 10);
        click(&mut app, MouseEventKind::Drag(MouseButton::Left), 39, 10);
        assert_eq!(app.params.get(ParamId::InputGain), g);
        click(&mut app, MouseEventKind::Down(MouseButton::Left), 39, 10);
        assert!(!app.show_help);
        assert_eq!(app.params.get(ParamId::InputGain), g);
        app.message = Some("x".into());
        click(&mut app, MouseEventKind::ScrollUp, 15, 10);
        assert_eq!(app.params.get(ParamId::InputGain), g);
        app.message = None;
        click(&mut app, MouseEventKind::ScrollUp, 15, 10);
        assert_ne!(app.params.get(ParamId::InputGain), g);

        // 8: Motor durursa monitor kapanır ve motor yeniden başlatılır.
        app.set_param(ParamId::MonitorOn, 1.0);
        app.tick();
        app.engine.shutdown();
        app.tick();
        assert!(app.engine_retry_at.is_some());
        assert!(app.fatal.is_some());
        assert!(!app.params.on(ParamId::MonitorOn));
        app.engine_retry_at = Some(Instant::now());
        app.tick();
        assert!(app.engine_retry_at.is_none());
        assert!(app.engine.is_running());
        assert!(app.fatal.is_none());

        drop(app);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
