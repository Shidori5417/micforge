//! Ses thread'i ile UI arasında paylaşılan kilitsiz durum.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

/// `f32` taşıyan atomik. Pozitif `f32` bitleri `u32` olarak monoton sıralıdır,
/// bu yüzden tepe birikimi için `fetch_max` doğrudan kullanılabilir.
#[derive(Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
    /// Pozitif değerler için tepe birikimi.
    pub fn fetch_max(&self, v: f32) {
        self.0.fetch_max(v.max(0.0).to_bits(), Relaxed);
    }
    /// Değeri okuyup sıfırlar.
    pub fn take(&self) -> f32 {
        f32::from_bits(self.0.swap(0, Relaxed))
    }
}

/// Metre ve durum verisi. Ses thread'i yazar, UI okur.
#[derive(Default)]
pub struct Shared {
    /// Son okumadan beri giriş tepe değeri (doğrusal).
    pub in_peak: AtomicF32,
    /// Giriş RMS (yumuşatılmış, doğrusal).
    pub in_rms: AtomicF32,
    pub out_peak: AtomicF32,
    pub out_rms: AtomicF32,
    /// Çıkışta 0 dBFS'e ulaşan blok sayısı.
    pub clip_count: AtomicU64,
    /// Sessiz (mute) bayrağı. UI yazar, ses thread'i okur.
    pub mute: AtomicBool,
    /// Son yakalama bloğunun örnek sayısı.
    pub block_frames: AtomicU32,
    /// Sanal mikrofon tarafında tamponda bekleyen örnek (kare) sayısı.
    pub ring_frames: AtomicU32,
    /// Yakalama işleminin blok süresine oranı (0..1), yumuşatılmış.
    pub cpu_load: AtomicF32,
    /// Sanal mikrofonun veri bulamadığı (underrun) blok sayısı.
    pub underruns: AtomicU64,
    /// Son okumadan beri en büyük kazanç azaltması (pozitif dB):
    /// gate, expander, compressor, de-esser, limiter, spektral kapı.
    pub gain_reduction: [AtomicF32; 6],
    /// UI her "profil öğren" isteğinde artırır; ses thread'i değişimi görünce başlatır.
    pub learn_request: AtomicU32,
    /// Spektral kapı şu an gürültü profili öğreniyor mu.
    pub noise_learning: AtomicBool,
    /// Gürültü profili var mı.
    pub noise_profile: AtomicBool,
    /// RNNoise ses aktivitesi olasılığı (0..1).
    pub vad: AtomicF32,
    /// Zincirin eklediği gecikme (limiter lookahead), örnek.
    pub dsp_latency: AtomicU32,
    /// Yakalama akışı veri alıyor mu.
    pub capturing: AtomicBool,
    /// Sanal mikrofonun kanal sayısı (1 veya 2).
    pub out_channels: AtomicU32,
}
