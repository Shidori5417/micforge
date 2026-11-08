//! MicForge DSP modülleri. Saf Rust, PipeWire bağımlılığı yok.
//!
//! Tüm modüller [`Processor`] trait'ini uygular. `process` gerçek zamanlı
//! ses thread'inden çağrılır: heap allocation, kilit, I/O ve log yasaktır.

pub mod biquad;
pub mod chain;
pub mod denormal;
pub mod dynamics;
pub mod effects;
pub mod eq;
pub mod feedback;
pub mod limiter;
pub mod meter;
pub mod modules;
pub mod params;
pub mod pitch;
pub mod rnnoise;
pub mod smooth;
pub mod spectral_gate;
pub mod spectrum;

/// Ortak DSP modülü arayüzü. Tamponlar mono, `f32` örneklerdir.
pub trait Processor {
    /// Tamponu yerinde işler. Gerçek zamanlı güvenli olmalıdır.
    fn process(&mut self, buf: &mut [f32]);
    /// İç durumu (filtre geçmişi, zarflar vb.) sıfırlar.
    fn reset(&mut self);
    /// Örnekleme hızını ayarlar; katsayılar yeniden hesaplanır.
    fn set_sample_rate(&mut self, sample_rate: f32);
}

/// Doğrusal genliği dBFS'e çevirir (0 → -inf yerine -120 dB).
#[inline]
pub fn lin_to_db(x: f32) -> f32 {
    if x <= 1e-6 { -120.0 } else { 20.0 * x.log10() }
}

/// dB değerini doğrusal genliğe çevirir.
#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_roundtrip() {
        for db in [-60.0f32, -6.0, 0.0, 12.0, 30.0] {
            assert!((lin_to_db(db_to_lin(db)) - db).abs() < 1e-3);
        }
        assert_eq!(lin_to_db(0.0), -120.0);
    }
}
