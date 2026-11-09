//! `AudioEngine` için gerçek PipeWire entegrasyon testleri.
//!
//! Bunlar çalışan bir PipeWire oturumu ister (CI'da yok), bu yüzden hepsi
//! `#[ignore]` ile işaretlidir ve normal `cargo test` içinde atlanır.
//! Çalıştırmak için (kullanıcı oturumunda PipeWire + WirePlumber açıkken):
//!
//! ```text
//! cargo test -p micforge-audio --test pipewire_engine -- --ignored --test-threads=1
//! ```
//!
//! Gerçek mikrofon gerekmez: sahte bir kaynak `pw-loopback` ile oluşturulur ve
//! `pw-play` ile içine sinüs verilir; sanal mikrofon `pw-record` ile okunur.
//! Gerekli araçlar: `pw-dump`, `pw-loopback`, `pw-play`, `pw-record`
//! (`pipewire` paketi). `--test-threads=1` şarttır: testler aynı oturumdaki
//! sabit adlı `micforge_mic` düğümünü paylaşır.
//!
//! Sahte kaynağın `node.name`'i `micforge` ile başlamamalıdır: motor bu önekli
//! düğümleri cihaz listesinden eler.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use micforge_audio::{
    AudioEngine, CaptureStatus, Device, Event, SAMPLE_RATE, VIRTUAL_DESCRIPTION, VIRTUAL_NODE_NAME,
};
use micforge_dsp::params::ParamStore;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Üretilen test sinüsünün süresi (saniye); en uzun test bundan kısadır.
const TONE_SECONDS: u32 = 60;

// ---------------------------------------------------------------- yardımcılar

/// Gerekli PipeWire araçları yoksa testi açık bir mesajla düşürür.
fn require_tools() {
    for tool in ["pw-dump", "pw-loopback", "pw-play", "pw-record"] {
        let ok = Command::new(tool)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        assert!(ok, "`{tool}` bulunamadı; PipeWire araçları kurulu olmalı");
    }
}

/// `pw-dump` çıktısı.
fn pw_dump() -> String {
    let out = Command::new("pw-dump")
        .output()
        .expect("pw-dump çalıştırılamadı");
    assert!(
        out.status.success(),
        "pw-dump başarısız (PipeWire çalışıyor mu?): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn node_exists(name: &str) -> bool {
    pw_dump().contains(&format!("\"node.name\": \"{name}\""))
}

/// `cond` doğru olana kadar bekler; süre dolarsa `false`.
fn poll(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Motor olaylarını, `pick` `Some` dönene kadar tüketir. `Fatal` testi düşürür.
fn wait_event<T>(engine: &AudioEngine, what: &str, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match engine.events.recv_timeout(left) {
            Ok(Event::Fatal(msg)) => {
                panic!("motor Fatal olayı gönderdi ({what} beklenirken): {msg}")
            }
            Ok(ev) => {
                if let Some(v) = pick(&ev) {
                    return v;
                }
            }
            Err(_) => panic!("{what}: {TIMEOUT:?} içinde olay gelmedi"),
        }
    }
}

fn wait_capture(engine: &AudioEngine, want: CaptureStatus) {
    let what = format!("{want:?}");
    wait_event(engine, &what, |ev| {
        matches!(ev, Event::Capture(s) if *s == want).then_some(())
    });
}

fn start_engine(device: Option<&str>) -> AudioEngine {
    AudioEngine::start(
        device.map(str::to_string),
        Arc::new(ParamStore::default()),
        2,
    )
    .expect("AudioEngine başlatılamadı (PipeWire oturumu var mı?)")
}

/// Çocuk süreci öldürür ve bekler (Drop'ta temizlik için).
fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Mono 16-bit 48 kHz sinüs WAV dosyası.
fn write_tone_wav(path: &PathBuf) {
    let n = SAMPLE_RATE * TONE_SECONDS;
    let data_len = n * 2;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    b.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        let t = i as f32 / SAMPLE_RATE as f32;
        let s = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.25;
        b.extend_from_slice(&((s * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, b).expect("test WAV dosyası yazılamadı");
}

/// Sahte mikrofon: `pw-loopback` bir `Audio/Sink` (içine sinüs çalınır) ve onun
/// çıkışını sunan bir `Audio/Source` düğümü oluşturur. Drop'ta her şey temizlenir;
/// "mikrofonu çekmek" için nesneyi düşürmek yeter.
struct FakeSource {
    /// Motora verilecek `node.name`.
    name: String,
    loopback: Child,
    player: Option<Child>,
    wav: PathBuf,
}

impl FakeSource {
    fn new(tag: &str) -> Self {
        let name = format!("mfgtest_src_{}_{tag}", std::process::id());
        let sink = format!("mfgtest_sink_{}_{tag}", std::process::id());
        let loopback = Command::new("pw-loopback")
            .args(["-n", &format!("mfgtest_{}_{tag}", std::process::id())])
            .args([
                "-i",
                &format!("media.class=Audio/Sink node.name={sink} node.description=MicForge-Test-Sink"),
                "-o",
                &format!("media.class=Audio/Source node.name={name} node.description=MicForge-Test-Source"),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("pw-loopback başlatılamadı");
        let wav = std::env::temp_dir().join(format!(
            "micforge_test_tone_{}_{tag}.wav",
            std::process::id()
        ));
        // Self hemen kurulur: sonraki bir assert düşse bile Drop süreçleri temizler.
        let mut me = Self {
            name,
            loopback,
            player: None,
            wav,
        };
        assert!(
            poll(TIMEOUT, || node_exists(&me.name) && node_exists(&sink)),
            "sahte kaynak düğümleri {TIMEOUT:?} içinde oluşmadı"
        );
        write_tone_wav(&me.wav);
        me.player = Some(
            Command::new("pw-play")
                .args(["--target", &sink])
                .arg(&me.wav)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("pw-play başlatılamadı"),
        );
        me
    }
}

impl Drop for FakeSource {
    fn drop(&mut self) {
        if let Some(p) = &mut self.player {
            kill(p);
        }
        kill(&mut self.loopback);
        let _ = std::fs::remove_file(&self.wav);
    }
}

/// Sanal mikrofonu `pw-record` ile okur (ham f32, stereo) ve gelen örnekleri
/// kanal üzerinden verir. Drop'ta süreç öldürülür.
struct VirtualMicReader {
    child: Child,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl VirtualMicReader {
    fn new() -> Self {
        let mut child = Command::new("pw-record")
            .args(["--target", VIRTUAL_NODE_NAME])
            .args(["--rate", &SAMPLE_RATE.to_string()])
            .args(["--channels", "2", "--format", "f32", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("pw-record başlatılamadı");
        let mut out = child.stdout.take().expect("pw-record stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            // Okumalar 4 baytın katı olmayabilir: artığı sonrakine taşı.
            let mut carry: Vec<u8> = Vec::new();
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 {
                    break;
                }
                carry.extend_from_slice(&buf[..n]);
                let whole = carry.len() / 4 * 4;
                let chunk: Vec<u8> = carry.drain(..whole).collect();
                if tx.send(chunk).is_err() {
                    break;
                }
            }
        });
        Self { child, rx }
    }

    /// `timeout` içinde okunan örneklerin en büyük mutlak değeri.
    fn peak_within(&self, timeout: Duration) -> f32 {
        let deadline = Instant::now() + timeout;
        let mut peak = 0.0f32;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(left) {
                Ok(bytes) => {
                    for c in bytes.as_chunks::<4>().0 {
                        let v = f32::from_le_bytes(*c);
                        if v.is_finite() {
                            peak = peak.max(v.abs());
                        }
                    }
                }
                Err(_) => break,
            }
        }
        peak
    }
}

impl Drop for VirtualMicReader {
    fn drop(&mut self) {
        kill(&mut self.child);
    }
}

// ---------------------------------------------------------------------- testler

#[test]
#[ignore = "needs a running PipeWire session"]
fn virtual_mic_lives_and_dies_with_engine() {
    require_tools();
    assert!(
        !node_exists(VIRTUAL_NODE_NAME),
        "`{VIRTUAL_NODE_NAME}` zaten var: çalışan bir MicForge'u kapatıp tekrar deneyin"
    );

    let engine = start_engine(None);
    assert!(
        poll(TIMEOUT, || node_exists(VIRTUAL_NODE_NAME)),
        "sanal mikrofon düğümü ({VIRTUAL_NODE_NAME}) {TIMEOUT:?} içinde görünmedi"
    );
    assert!(
        pw_dump().contains(&format!("\"node.description\": \"{VIRTUAL_DESCRIPTION}\"")),
        "sanal mikrofon açıklaması `{VIRTUAL_DESCRIPTION}` değil"
    );

    drop(engine);
    assert!(
        poll(TIMEOUT, || !node_exists(VIRTUAL_NODE_NAME)),
        "motor düşürüldükten sonra sanal mikrofon {TIMEOUT:?} içinde kalkmadı"
    );
}

#[test]
#[ignore = "needs a running PipeWire session"]
fn device_list_event_arrives() {
    require_tools();
    let fake = FakeSource::new("list");
    let engine = start_engine(None);

    let devices: Vec<Device> =
        wait_event(
            &engine,
            "Event::Devices (sahte kaynak ile)",
            |ev| match ev {
                Event::Devices(list) if list.iter().any(|d| d.name == fake.name) => {
                    Some(list.clone())
                }
                _ => None,
            },
        );
    let d = devices.iter().find(|d| d.name == fake.name).unwrap();
    assert_eq!(d.description, "MicForge-Test-Source");
    assert!(
        devices.iter().all(|d| !d.name.starts_with("micforge")),
        "sanal mikrofon cihaz listesinde görünmemeli: {devices:?}"
    );
}

#[test]
#[ignore = "needs a running PipeWire session"]
fn connects_to_source_and_audio_flows() {
    require_tools();
    let fake = FakeSource::new("flow");
    let engine = start_engine(Some(&fake.name));

    wait_capture(&engine, CaptureStatus::Active(fake.name.clone()));
    assert!(
        poll(TIMEOUT, || engine.shared.capturing.load(Relaxed)),
        "Active oldu ama yakalama akışı veri almıyor (capturing=false)"
    );
    assert!(
        poll(TIMEOUT, || engine.shared.block_frames.load(Relaxed) > 0),
        "block_frames hiç güncellenmedi"
    );

    // Giriş ölçeri sinüsü görmeli (genlik ~0.25).
    let mut in_peak = 0.0f32;
    assert!(
        poll(TIMEOUT, || {
            in_peak = in_peak.max(engine.shared.in_peak.take());
            in_peak > 0.1
        }),
        "in_peak sinüse rağmen yükselmedi: {in_peak}"
    );
    // Spektrum tap'leri örnek taşımalı.
    assert!(
        poll(TIMEOUT, || engine.tap_in.slots() > 0
            && engine.tap_out.slots() > 0),
        "tap_in/tap_out halkalarına örnek gelmedi"
    );

    // Uçtan uca: sanal mikrofonu okuyan biri sessizlik dışında bir şey duymalı,
    // underrun sayısı sınırlı kalmalı.
    let reader = VirtualMicReader::new();
    assert!(
        poll(TIMEOUT, || node_exists(VIRTUAL_NODE_NAME)),
        "sanal mikrofon düğümü yok"
    );
    let heard = reader.peak_within(Duration::from_secs(4));
    assert!(
        heard > 0.01,
        "sanal mikrofondan okunan sinyal sessiz (tepe {heard}); çıkış yolu çalışmıyor"
    );
    assert!(
        engine.shared.out_peak.take() > 0.0,
        "out_peak sıfır: DSP çıkışı ölçülmedi"
    );

    // Okuma süren ~2 sn boyunca underrun artışı sınırlı olmalı (başlangıçtaki
    // birkaç blok dışında). Bloklar ~5.3 ms (256/48000): 2 sn ≈ 375 blok.
    let before = engine.shared.underruns.load(Relaxed);
    let _ = reader.peak_within(Duration::from_secs(2));
    let grown = engine.shared.underruns.load(Relaxed) - before;
    assert!(grown <= 20, "kararlı akışta çok fazla underrun: +{grown}");
}

#[test]
#[ignore = "needs a running PipeWire session"]
fn hot_unplug_waits_and_replug_reconnects() {
    require_tools();
    let fake = FakeSource::new("hot");
    let name = fake.name.clone();
    let engine = start_engine(Some(&name));
    wait_capture(&engine, CaptureStatus::Active(name.clone()));
    assert!(poll(TIMEOUT, || engine.shared.capturing.load(Relaxed)));

    // Cihaz çıkarıldı → Waiting, akış durur.
    drop(fake);
    wait_capture(&engine, CaptureStatus::Waiting(name.clone()));
    assert!(
        poll(TIMEOUT, || !engine.shared.capturing.load(Relaxed)),
        "kaynak kalktı ama capturing=true kaldı"
    );

    // Aynı adla geri takıldı → otomatik yeniden bağlanır.
    let _fake2 = FakeSource::new("hot");
    assert_eq!(_fake2.name, name);
    wait_capture(&engine, CaptureStatus::Active(name.clone()));
    assert!(
        poll(TIMEOUT, || engine.shared.capturing.load(Relaxed)),
        "yeniden bağlandı ama akış veri almıyor"
    );
}
