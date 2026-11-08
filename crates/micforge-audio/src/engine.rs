use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Instant;

use micforge_dsp::chain::Chain;
use micforge_dsp::params::{ParamId, ParamStore};
use micforge_dsp::smooth::Smoother;
use micforge_dsp::{db_to_lin, denormal, meter};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::stream::{StreamFlags, StreamRc};
use rtrb::{Consumer, Producer, RingBuffer};
use spa::pod::Pod;

use crate::{
    NOISE_LEARN_SECONDS, NOISE_PROFILE_LEN, QUANTUM, SAMPLE_RATE, Shared, VIRTUAL_DESCRIPTION,
    VIRTUAL_NODE_NAME,
};

/// Yakalama ve sanal kaynak akışlarının kanal sayısı (iç işlem monodur).
const CHANNELS: usize = 2;
/// Tek seferde işlenebilen en büyük blok (kare). Daha büyük bloklar parçalanır.
const MAX_BLOCK: usize = 8192;
/// Yakalama → sanal kaynak halka tamponu kapasitesi (örnek).
const RING_CAPACITY: usize = MAX_BLOCK * CHANNELS * 2;
/// Spektrum için UI'a giden mono örnek halkalarının kapasitesi.
const TAP_CAPACITY: usize = 16_384;

/// Gürültü profili taşıyan kutu. UI ile ses thread'i arasında gidip gelir;
/// böylece ses thread'i hiç bellek ayırmaz/serbest bırakmaz.
pub struct ProfileBox {
    pub data: Box<[f32]>,
    pub kind: ProfileKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileKind {
    /// UI → ses: bu profili yükle (kutu geri gönderilir).
    Load,
    /// UI → ses: öğrenme bitince doldurulacak boş kutu.
    Spare,
    /// Ses → UI: yeni öğrenilen profil (kaydedilmek üzere).
    Learned,
}

impl ProfileBox {
    pub fn new(kind: ProfileKind) -> Self {
        Self {
            data: vec![0.0; NOISE_PROFILE_LEN].into_boxed_slice(),
            kind,
        }
    }
}

/// Seçilebilir bir giriş cihazı (PipeWire Audio/Source düğümü).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: u32,
    /// `node.name`, kalıcı kimlik olarak kullanılır (yeniden takılınca aynı kalır).
    pub name: String,
    /// İnsan tarafından okunur ad.
    pub description: String,
}

/// Yakalama akışının durumu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureStatus {
    /// Cihaz seçilmedi.
    Idle,
    /// Seçilen cihaz şu an yok (ör. USB çıkarıldı); geri gelince bağlanılır.
    Waiting(String),
    /// Cihaza bağlı.
    Active(String),
}

/// PipeWire thread'inden UI'a giden olaylar.
#[derive(Debug)]
pub enum Event {
    Devices(Vec<Device>),
    Capture(CaptureStatus),
    /// Kurtarılamaz hata (ör. PipeWire bağlantısı koptu).
    Fatal(String),
}

enum Command {
    SelectDevice(Option<String>),
    /// Monitor akışını bağla/kes.
    Monitor(bool),
    /// Sanal mikrofonu verilen kanal sayısıyla (1/2) yeniden bağla.
    OutputChannels(u32),
    Quit,
}

/// Çalışan ses motoru. Düşürüldüğünde (drop) sanal mikrofon kaldırılır.
pub struct AudioEngine {
    cmd: pw::channel::Sender<Command>,
    pub events: mpsc::Receiver<Event>,
    pub shared: Arc<Shared>,
    /// Spektrum analizi için giriş (DSP öncesi) ve çıkış (DSP sonrası) mono
    /// örnekleri. UI her karede boşaltır; doluysa ses thread'i yeni veriyi atar.
    pub tap_in: Consumer<f32>,
    pub tap_out: Consumer<f32>,
    /// Gürültü profili kutuları: UI → ses ve ses → UI.
    pub profile_tx: Producer<ProfileBox>,
    pub profile_rx: Consumer<ProfileBox>,
    thread: Option<JoinHandle<()>>,
}

impl AudioEngine {
    /// PipeWire'a bağlanır ve sanal mikrofonu oluşturur. PipeWire yoksa
    /// anlaşılır bir hata mesajı döner.
    /// `params`: UI'ın yazdığı, ses thread'inin okuduğu kilitsiz parametreler.
    /// `out_channels`: sanal mikrofonun kanal sayısı (1: mono, 2: stereo).
    pub fn start(
        initial_device: Option<String>,
        params: Arc<ParamStore>,
        out_channels: u32,
    ) -> Result<Self, String> {
        let (cmd_tx, cmd_rx) = pw::channel::channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared::default());
        shared
            .out_channels
            .store(out_channels.clamp(1, CHANNELS as u32), Relaxed);
        let shared2 = shared.clone();
        let (tin_p, tap_in) = RingBuffer::<f32>::new(TAP_CAPACITY);
        let (tout_p, tap_out) = RingBuffer::<f32>::new(TAP_CAPACITY);
        let (profile_tx, prof_in) = RingBuffer::<ProfileBox>::new(4);
        let (prof_out, profile_rx) = RingBuffer::<ProfileBox>::new(4);
        let taps = Taps {
            input: tin_p,
            output: tout_p,
            prof_in,
            prof_out,
        };
        let thread = std::thread::Builder::new()
            .name("micforge-pw".into())
            .spawn(move || {
                if let Err(e) = run(
                    cmd_rx,
                    evt_tx,
                    shared2,
                    params,
                    taps,
                    initial_device,
                    &ready_tx,
                ) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                cmd: cmd_tx,
                events: evt_rx,
                shared,
                tap_in,
                tap_out,
                profile_tx,
                profile_rx,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err("PipeWire thread'i beklenmedik şekilde sonlandı".into()),
        }
    }

    /// Giriş cihazını `node.name` ile seçer (`None`: bağlantıyı kes).
    pub fn select_device(&self, name: Option<String>) {
        let _ = self.cmd.send(Command::SelectDevice(name));
    }

    /// Sanal mikrofonun kanal sayısını değiştirir (1: mono, 2: stereo). Akış
    /// yeniden bağlanır; uygulamalar kısa bir kesinti görür.
    pub fn set_output_channels(&self, n: u32) {
        let _ = self.cmd.send(Command::OutputChannels(n));
    }

    /// Monitor (kendini duy) akışını varsayılan çıkışa bağlar veya keser.
    pub fn set_monitor(&self, on: bool) {
        let _ = self.cmd.send(Command::Monitor(on));
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.cmd.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Ana döngü thread'indeki (gerçek zamanlı olmayan) durum.
struct State {
    devices: BTreeMap<u32, Device>,
    wanted: Option<String>,
    connected: Option<String>,
    capture: StreamRc,
    events: mpsc::Sender<Event>,
    last_status: Option<CaptureStatus>,
    shared: Arc<Shared>,
}

impl State {
    fn send_devices(&self) {
        let mut list: Vec<Device> = self.devices.values().cloned().collect();
        list.sort_by(|a, b| a.description.cmp(&b.description));
        let _ = self.events.send(Event::Devices(list));
    }

    /// İstenen cihaz ile bağlı cihazı eşitler (cihaz takılınca/çıkınca da çağrılır).
    fn sync_capture(&mut self) {
        let present = self
            .wanted
            .clone()
            .filter(|n| self.devices.values().any(|d| &d.name == n));
        if present != self.connected {
            if self.connected.take().is_some() {
                let _ = self.capture.disconnect();
                self.shared.capturing.store(false, Relaxed);
                self.shared.cpu_load.store(0.0);
            }
            if let Some(name) = &present {
                match connect_capture(&self.capture, name) {
                    Ok(()) => self.connected = present.clone(),
                    Err(e) => {
                        let _ = self
                            .events
                            .send(Event::Fatal(format!("Yakalama akışı bağlanamadı: {e}")));
                    }
                }
            }
        }
        let status = match (&self.wanted, &self.connected) {
            (None, _) => CaptureStatus::Idle,
            (Some(_), Some(c)) => CaptureStatus::Active(c.clone()),
            (Some(w), None) => CaptureStatus::Waiting(w.clone()),
        };
        if self.last_status.as_ref() != Some(&status) {
            self.last_status = Some(status.clone());
            let _ = self.events.send(Event::Capture(status));
        }
    }
}

fn run(
    cmd_rx: pw::channel::Receiver<Command>,
    events: mpsc::Sender<Event>,
    shared: Arc<Shared>,
    params: Arc<ParamStore>,
    taps: Taps,
    initial_device: Option<String>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| format!("PipeWire ana döngüsü oluşturulamadı: {e}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| format!("PipeWire bağlamı oluşturulamadı: {e}"))?;
    let core = context.connect_rc(None).map_err(|_| {
        "PipeWire sunucusuna bağlanılamadı. PipeWire çalışıyor mu?\n\
         Kontrol: systemctl --user status pipewire pipewire-pulse wireplumber"
            .to_string()
    })?;
    let registry = core
        .get_registry_rc()
        .map_err(|e| format!("PipeWire kayıt defteri alınamadı: {e}"))?;

    let (producer, consumer) = RingBuffer::<f32>::new(RING_CAPACITY);
    let (mon_producer, mon_consumer) = RingBuffer::<f32>::new(RING_CAPACITY);

    // Sanal mikrofon (uygulamaların göreceği kaynak).
    let output = StreamRc::new(
        core.clone(),
        "micforge-mic",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            // "Audio/Source/Virtual" sınıfına WirePlumber port açmıyor (0.5 ile
            // doğrulandı); sanal kaynaklar için standart yol budur.
            *pw::keys::MEDIA_CLASS => "Audio/Source",
            "node.virtual" => "true",
            *pw::keys::NODE_NAME => VIRTUAL_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => VIRTUAL_DESCRIPTION,
            *pw::keys::NODE_GROUP => "micforge",
            *pw::keys::NODE_LATENCY => format!("{QUANTUM}/{SAMPLE_RATE}"),
            "node.loop.name" => "data-loop.0",
        },
    )
    .map_err(|e| format!("Sanal mikrofon oluşturulamadı: {e}"))?;
    let _out_listener = output
        .add_local_listener_with_user_data(PlaybackRt {
            consumer,
            shared: shared.clone(),
            monitor: None,
        })
        .process(|stream, rt| rt.process(stream))
        .register()
        .map_err(|e| e.to_string())?;
    let fmt = audio_format_pod(shared.out_channels.load(Relaxed));
    output
        .connect(
            spa::utils::Direction::Output,
            None,
            StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
            &mut [Pod::from_bytes(&fmt).expect("geçerli format pod'u")],
        )
        .map_err(|e| format!("Sanal mikrofon bağlanamadı: {e}"))?;

    // Monitor: işlenmiş sesi varsayılan çıkışa (kulaklık) verir. Yalnızca
    // açıkken bağlıdır. Gruba alınmaz: çıkış cihazının kendi saati sürer,
    // kayma halka tamponunda düşürülerek sınırlanır.
    let monitor = StreamRc::new(
        core.clone(),
        "micforge-monitor",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::NODE_NAME => "micforge_monitor",
            *pw::keys::NODE_DESCRIPTION => "MicForge Monitor",
            *pw::keys::NODE_LATENCY => format!("{QUANTUM}/{SAMPLE_RATE}"),
            "node.loop.name" => "data-loop.0",
        },
    )
    .map_err(|e| format!("Monitor akışı oluşturulamadı: {e}"))?;
    let _mon_listener = monitor
        .add_local_listener_with_user_data(PlaybackRt {
            consumer: mon_consumer,
            shared: shared.clone(),
            monitor: Some(MonitorGain {
                params: params.clone(),
                gain: Smoother::new(0.0, SAMPLE_RATE as f32),
            }),
        })
        .process(|stream, rt| rt.process(stream))
        .register()
        .map_err(|e| e.to_string())?;
    let monitor_on = std::cell::Cell::new(false);

    // Fiziksel mikrofondan yakalama akışı. Cihaz değişince aynı akış yeniden bağlanır.
    let capture = StreamRc::new(
        core.clone(),
        "micforge-capture",
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Communication",
            *pw::keys::NODE_NAME => "micforge_capture",
            *pw::keys::NODE_DESCRIPTION => "MicForge Capture",
            *pw::keys::NODE_GROUP => "micforge",
            *pw::keys::NODE_LATENCY => format!("{QUANTUM}/{SAMPLE_RATE}"),
            *pw::keys::NODE_DONT_RECONNECT => "true",
            "node.dont-fallback" => "true",
            "node.loop.name" => "data-loop.0",
        },
    )
    .map_err(|e| format!("Yakalama akışı oluşturulamadı: {e}"))?;
    let _cap_listener = capture
        .add_local_listener_with_user_data(CaptureRt {
            producer,
            monitor: mon_producer,
            shared: shared.clone(),
            mono: vec![0.0; MAX_BLOCK],
            stereo: vec![0.0; MAX_BLOCK * CHANNELS],
            chain: Chain::new(SAMPLE_RATE as f32, &params),
            mute: Smoother::new(1.0, SAMPLE_RATE as f32),
            params: params.clone(),
            taps,
            spare: None,
            last_learn_request: 0,
            was_learning: false,
            ftz_set: false,
            rms_in: 0.0,
            rms_out: 0.0,
        })
        .process(|stream, rt| rt.process(stream))
        .register()
        .map_err(|e| e.to_string())?;

    let state = Rc::new(RefCell::new(State {
        devices: BTreeMap::new(),
        wanted: initial_device,
        connected: None,
        capture,
        events: events.clone(),
        last_status: None,
        shared: shared.clone(),
    }));

    // Cihaz listesi ve tak-çıkar takibi.
    let st = state.clone();
    let st2 = state.clone();
    let _reg_listener = registry
        .add_listener_local()
        .global(move |g| {
            if g.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = g.props else { return };
            let class = props.get(*pw::keys::MEDIA_CLASS).unwrap_or("");
            let name = props.get(*pw::keys::NODE_NAME).unwrap_or("");
            if !class.starts_with("Audio/Source") || name.is_empty() || name.starts_with("micforge")
            {
                return;
            }
            let description = props
                .get(*pw::keys::NODE_DESCRIPTION)
                .or_else(|| props.get(*pw::keys::NODE_NICK))
                .unwrap_or(name)
                .to_string();
            let mut s = st.borrow_mut();
            s.devices.insert(
                g.id,
                Device {
                    id: g.id,
                    name: name.to_string(),
                    description,
                },
            );
            s.send_devices();
            s.sync_capture();
        })
        .global_remove(move |id| {
            let mut s = st2.borrow_mut();
            if s.devices.remove(&id).is_some() {
                s.send_devices();
                s.sync_capture();
            }
        })
        .register();

    // PipeWire sunucusu giderse UI'a bildir.
    let ml = mainloop.downgrade();
    let ev = events.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, res, msg| {
            if id == pw::core::PW_ID_CORE && res == -32 {
                let _ = ev.send(Event::Fatal(format!("PipeWire bağlantısı koptu: {msg}")));
                if let Some(ml) = ml.upgrade() {
                    ml.quit();
                }
            }
        })
        .register();

    let ml = mainloop.downgrade();
    let st = state.clone();
    let ev2 = events.clone();
    let mon2 = monitor.clone();
    let out2 = output.clone();
    let shared3 = shared.clone();
    let _cmd = cmd_rx.attach(mainloop.loop_(), move |cmd| match cmd {
        Command::SelectDevice(name) => {
            let mut s = st.borrow_mut();
            s.wanted = name;
            s.sync_capture();
        }
        Command::Monitor(on) => {
            if on == monitor_on.get() {
                return;
            }
            let res = if on {
                let fmt = audio_format_pod(CHANNELS as u32);
                monitor.connect(
                    spa::utils::Direction::Output,
                    None,
                    StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
                    &mut [Pod::from_bytes(&fmt).expect("geçerli format pod'u")],
                )
            } else {
                monitor.disconnect()
            };
            match res {
                Ok(()) => monitor_on.set(on),
                Err(e) => {
                    let _ = ev2.send(Event::Fatal(format!("Monitor akışı: {e}")));
                }
            }
        }
        Command::OutputChannels(n) => {
            let n = n.clamp(1, CHANNELS as u32);
            if n == shared3.out_channels.load(Relaxed) {
                return;
            }
            let _ = out2.disconnect();
            shared3.out_channels.store(n, Relaxed);
            let fmt = audio_format_pod(n);
            if let Err(e) = out2.connect(
                spa::utils::Direction::Output,
                None,
                StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
                &mut [Pod::from_bytes(&fmt).expect("geçerli format pod'u")],
            ) {
                let _ = ev2.send(Event::Fatal(format!(
                    "Sanal mikrofon yeniden bağlanamadı: {e}"
                )));
            }
        }
        Command::Quit => {
            if let Some(ml) = ml.upgrade() {
                ml.quit();
            }
        }
    });

    let _ = ready.send(Ok(()));
    // Kayıt defterindeki mevcut cihazlar gelince başlangıç durumu da gönderilir.
    state.borrow_mut().sync_capture();
    mainloop.run();

    // Temizlik: akışları kapat, sanal mikrofon kaldırılır.
    let s = state.borrow();
    let _ = s.capture.disconnect();
    let _ = output.disconnect();
    let _ = mon2.disconnect();
    Ok(())
}

fn connect_capture(stream: &StreamRc, target: &str) -> Result<(), pw::Error> {
    let props = properties! { *pw::keys::TARGET_OBJECT => target };
    // SAFETY: Geçerli bir akış ve sözlük işaretçisi; PipeWire sözlüğü kopyalar.
    unsafe {
        pw::sys::pw_stream_update_properties(stream.as_raw_ptr(), props.dict().as_raw_ptr());
    }
    let fmt = audio_format_pod(CHANNELS as u32);
    stream.connect(
        spa::utils::Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
        &mut [Pod::from_bytes(&fmt).expect("geçerli format pod'u")],
    )
}

/// 48 kHz, F32LE format tanımı (1: mono, 2: stereo).
fn audio_format_pod(channels: u32) -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(channels);
    let mut pos = [0; spa::param::audio::MAX_CHANNELS];
    if channels == 1 {
        pos[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_MONO;
    } else {
        pos[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
        pos[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    }
    info.set_position(pos);
    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::sys::SPA_TYPE_OBJECT_Format,
            id: pw::spa::sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
        }),
    )
    .expect("format serileştirme")
    .0
    .into_inner()
}

/// Spektrum için UI'a örnek gönderen halkalar (ses thread'i tarafı).
struct Taps {
    input: Producer<f32>,
    output: Producer<f32>,
    prof_in: Consumer<ProfileBox>,
    prof_out: Producer<ProfileBox>,
}

/// Sığdığı kadarını yazar, kalanı atar (UI yetişemiyorsa beklenmez).
#[inline]
fn push_tap<I: Iterator<Item = f32>>(p: &mut Producer<f32>, n: usize, it: I) {
    let n = n.min(p.slots());
    if let Ok(mut w) = p.write_chunk_uninit(n) {
        let (a, b) = w.as_mut_slices();
        for (slot, v) in a.iter_mut().chain(b.iter_mut()).zip(it) {
            slot.write(v);
        }
        // SAFETY: `it` en az `n` öğe üretir (çağıranlar `n` uzunlukta dilim verir).
        unsafe { w.commit_all() };
    }
}

/// Yakalama tarafı gerçek zamanlı durumu. `process` içinde allocation yok.
struct CaptureRt {
    producer: Producer<f32>,
    /// Monitor akışına giden işlenmiş stereo örnekler (yalnızca monitor açıkken).
    monitor: Producer<f32>,
    shared: Arc<Shared>,
    /// Önceden ayrılmış mono işlem tamponu.
    mono: Vec<f32>,
    /// Önceden ayrılmış serpiştirilmiş (interleaved) stereo çıkış tamponu.
    stereo: Vec<f32>,
    chain: Chain,
    /// Sessiz geçişi tık yapmasın diye yumuşatılmış kazanç.
    mute: Smoother,
    params: Arc<ParamStore>,
    taps: Taps,
    /// Öğrenme bitince doldurulacak boş profil kutusu.
    spare: Option<ProfileBox>,
    last_learn_request: u32,
    was_learning: bool,
    ftz_set: bool,
    rms_in: f32,
    rms_out: f32,
}

impl CaptureRt {
    /// Profil öğrenme isteği ve profil kutularını işler. Allocation/free yok:
    /// kutular yalnızca halkalar arasında taşınır.
    fn handle_noise_profile(&mut self) {
        let req = self.shared.learn_request.load(Relaxed);
        if req != self.last_learn_request {
            self.last_learn_request = req;
            self.chain.start_noise_learning(NOISE_LEARN_SECONDS);
        }
        while let Ok(mut b) = self.taps.prof_in.pop() {
            match b.kind {
                ProfileKind::Load => {
                    self.chain.set_noise_profile(&b.data);
                    self.give_back(b);
                }
                _ => {
                    if self.spare.is_none() {
                        b.kind = ProfileKind::Spare;
                        self.spare = Some(b);
                    } else {
                        self.give_back(b);
                    }
                }
            }
        }
        let learning = self.chain.noise_learning();
        if self.was_learning
            && !learning
            && let Some(mut b) = self.spare.take()
        {
            if self.chain.noise_profile_into(&mut b.data) {
                b.kind = ProfileKind::Learned;
            }
            self.give_back(b);
        }
        self.was_learning = learning;
        self.shared.noise_learning.store(learning, Relaxed);
        self.shared
            .noise_profile
            .store(self.chain.has_noise_profile(), Relaxed);
        self.shared.vad.store(self.chain.vad());
    }

    /// Kutuyu UI'a geri gönderir. Halka doluysa (olmamalı) serbest bırakmak
    /// yerine sızdırır: ses thread'inde `free` çağrılmaz.
    fn give_back(&mut self, b: ProfileBox) {
        if let Err(rtrb::PushError::Full(b)) = self.taps.prof_out.push(b) {
            std::mem::forget(b);
        }
    }

    fn process(&mut self, stream: &pw::stream::Stream) {
        if !self.ftz_set {
            denormal::enable_flush_to_zero();
            self.chain.warm_up();
            self.ftz_set = true;
        }
        let start = Instant::now();
        let Some(mut buffer) = stream.dequeue_buffer() else {
            return;
        };
        let datas = buffer.datas_mut();
        let Some(d) = datas.first_mut() else { return };
        let offset = d.chunk().offset() as usize;
        let size = d.chunk().size() as usize;
        let Some(bytes) = d.data() else { return };
        let end = (offset + size).min(bytes.len());
        let bytes = &bytes[offset.min(end)..end];
        let stride = CHANNELS * 4;
        let total = bytes.len() / stride;
        self.shared.block_frames.store(total as u32, Relaxed);
        self.shared.capturing.store(true, Relaxed);
        self.chain.update(&self.params);
        self.handle_noise_profile();
        self.mute.set_target(if self.shared.mute.load(Relaxed) {
            0.0
        } else {
            1.0
        });
        let mode = self.chain.channel_mode();

        for chunk in bytes.chunks(MAX_BLOCK * stride) {
            let frames = chunk.len() / stride;
            let mono = &mut self.mono[..frames];
            for (m, f) in mono.iter_mut().zip(chunk.chunks_exact(stride)) {
                let l = f32::from_le_bytes([f[0], f[1], f[2], f[3]]);
                let r = f32::from_le_bytes([f[4], f[5], f[6], f[7]]);
                *m = mode.downmix(l, r);
            }
            let (p, r) = meter::peak_rms(mono);
            self.shared.in_peak.fetch_max(p);
            self.rms_in = smooth_rms(self.rms_in, r);
            push_tap(&mut self.taps.input, frames, mono.iter().copied());

            self.chain.process(mono);

            let stereo = &mut self.stereo[..frames * CHANNELS];
            for (x, lr) in mono.iter().zip(stereo.as_chunks_mut::<CHANNELS>().0) {
                let (l, r) = self.chain.output(*x);
                let g = self.mute.next_value();
                lr[0] = l * g;
                lr[1] = r * g;
            }
            push_tap(
                &mut self.taps.output,
                frames,
                stereo
                    .as_chunks::<CHANNELS>()
                    .0
                    .iter()
                    .map(|lr| 0.5 * (lr[0] + lr[1])),
            );
            let (p, r) = meter::peak_rms(stereo);
            self.shared.out_peak.fetch_max(p);
            if p >= 0.999 {
                self.shared.clip_count.fetch_add(1, Relaxed);
            }
            self.rms_out = smooth_rms(self.rms_out, r);

            let n = (frames * CHANNELS).min(self.producer.slots());
            if let Ok(mut w) = self.producer.write_chunk_uninit(n) {
                let (a, b) = w.as_mut_slices();
                for (slot, v) in a.iter_mut().chain(b.iter_mut()).zip(stereo.iter()) {
                    slot.write(*v);
                }
                // SAFETY: Yukarıda tüm `n` yuva yazıldı.
                unsafe { w.commit_all() };
            }
            if self.params.on(ParamId::MonitorOn) {
                push_tap(&mut self.monitor, frames * CHANNELS, stereo.iter().copied());
            }
        }
        for (a, gr) in self
            .shared
            .gain_reduction
            .iter()
            .zip(self.chain.gain_reductions())
        {
            a.fetch_max(gr);
        }
        self.shared
            .dsp_latency
            .store(self.chain.latency_samples() as u32, Relaxed);
        self.shared.in_rms.store(self.rms_in);
        self.shared.out_rms.store(self.rms_out);

        if total > 0 {
            let block = total as f32 / SAMPLE_RATE as f32;
            let load = start.elapsed().as_secs_f32() / block;
            let prev = self.shared.cpu_load.load();
            self.shared.cpu_load.store(prev + 0.05 * (load - prev));
        }
    }
}

#[inline]
fn smooth_rms(prev: f32, new: f32) -> f32 {
    prev + 0.3 * (new - prev)
}

/// Sanal mikrofon veya monitor tarafı gerçek zamanlı durumu.
struct PlaybackRt {
    consumer: Consumer<f32>,
    shared: Arc<Shared>,
    /// Monitor akışıysa ses seviyesi (sanal mikrofon için `None`).
    monitor: Option<MonitorGain>,
}

/// Monitor ses seviyesi: kapalıyken 0'a, açıkken ayara yumuşak geçer.
struct MonitorGain {
    params: Arc<ParamStore>,
    gain: Smoother,
}

impl PlaybackRt {
    fn process(&mut self, stream: &pw::stream::Stream) {
        let Some(mut buffer) = stream.dequeue_buffer() else {
            return;
        };
        let requested = buffer.requested() as usize;
        let datas = buffer.datas_mut();
        let Some(d) = datas.first_mut() else { return };
        // Sanal mikrofon mono veya stereo olabilir; halka her zaman stereo taşır.
        let out_ch = if self.monitor.is_none() {
            (self.shared.out_channels.load(Relaxed) as usize).clamp(1, CHANNELS)
        } else {
            CHANNELS
        };
        let stride = out_ch * 4;
        let written = if let Some(bytes) = d.data() {
            let max_frames = bytes.len() / stride;
            let frames = if requested == 0 {
                max_frames.min(QUANTUM as usize)
            } else {
                requested.min(max_frames)
            };
            let need = frames * CHANNELS;

            // Saat kayması veya tüketici yokken biriken veriyi at: gecikme sınırlı kalsın.
            let cap_block = self.shared.block_frames.load(Relaxed) as usize * CHANNELS;
            let avail = self.consumer.slots();
            let keep = need + cap_block;
            if avail > keep + cap_block.max(need)
                && let Ok(c) = self.consumer.read_chunk(avail - keep)
            {
                c.commit_all();
            }

            let avail = self.consumer.slots().min(need);
            if avail < need && self.monitor.is_none() && self.shared.capturing.load(Relaxed) {
                self.shared.underruns.fetch_add(1, Relaxed);
            }
            if let Some(m) = &mut self.monitor {
                let target = if m.params.on(ParamId::MonitorOn) {
                    db_to_lin(m.params.get(ParamId::MonitorVolume))
                } else {
                    0.0
                };
                m.gain.set_target(target);
            }
            let out = &mut bytes[..frames * stride];
            let mut i = 0;
            // Mono çıkışta yazılan örnek sayısı (L/R ortalaması).
            let mut o = 0;
            let mut left = 0.0f32;
            if let Ok(c) = self.consumer.read_chunk(avail) {
                let (a, b) = c.as_slices();
                for s in a.iter().chain(b.iter()) {
                    // Stereo çiftine aynı kazanç: çerçevenin ilk örneğinde ilerlet.
                    let g = match &mut self.monitor {
                        Some(m) if i % CHANNELS == 0 => m.gain.next_value(),
                        Some(m) => m.gain.value(),
                        None => 1.0,
                    };
                    if out_ch == CHANNELS {
                        out[i * 4..i * 4 + 4].copy_from_slice(&(s * g).to_le_bytes());
                        o = i + 1;
                    } else if i % 2 == 0 {
                        left = *s;
                    } else {
                        let v = 0.5 * (left + s);
                        out[o * 4..o * 4 + 4].copy_from_slice(&v.to_le_bytes());
                        o += 1;
                    }
                    i += 1;
                }
                c.commit_all();
            }
            out[o * 4..].fill(0);
            if self.monitor.is_none() {
                self.shared
                    .ring_frames
                    .store((self.consumer.slots() / CHANNELS) as u32, Relaxed);
            }
            frames
        } else {
            0
        };
        let chunk = d.chunk_mut();
        *chunk.offset_mut() = 0;
        *chunk.stride_mut() = stride as i32;
        *chunk.size_mut() = (written * stride) as u32;
    }
}
