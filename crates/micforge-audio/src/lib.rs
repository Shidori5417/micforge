//! PipeWire bağlantısı: fiziksel mikrofondan okur, DSP'den geçirir ve sonucu
//! "MicForge Mic" adlı sanal bir kaynak (Audio/Source, node.virtual) olarak sunar.
//!
//! PipeWire nesneleri `!Send` olduğu için hepsi kendi thread'inde yaşar.
//! UI ile iletişim:
//! - UI → PipeWire: [`Command`] (pipewire channel, ana döngüde işlenir)
//! - PipeWire → UI: [`Event`] (std mpsc, gerçek zamanlı olmayan thread'den)
//! - Ses thread'i → UI: [`Shared`] içindeki atomikler (kilitsiz)

mod engine;
mod shared;

pub use engine::{AudioEngine, CaptureStatus, Device, Event, ProfileBox, ProfileKind};
pub use shared::Shared;

/// Sanal mikrofonun PipeWire düğüm adı.
pub const VIRTUAL_NODE_NAME: &str = "micforge_mic";
/// Sanal mikrofonun uygulamalarda görünen adı.
pub const VIRTUAL_DESCRIPTION: &str = "MicForge Mic";
/// Sabit çalışma örnekleme hızı.
pub const SAMPLE_RATE: u32 = 48_000;
/// Spektral kapı gürültü profilinin uzunluğu (FFT_SIZE/2 + 1).
pub const NOISE_PROFILE_LEN: usize = micforge_dsp::spectral_gate::SpectralGate::FFT_SIZE / 2 + 1;
/// "Profil öğren" süresi (saniye).
pub const NOISE_LEARN_SECONDS: f32 = 3.0;
/// İstenen işlem bloğu (quantum) boyutu, örnek cinsinden.
pub const QUANTUM: u32 = 256;
