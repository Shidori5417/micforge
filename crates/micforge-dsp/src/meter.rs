//! Seviye metreleri: blok başına tepe ve RMS. Allocation yapmaz.

/// Bir bloğun tepe (mutlak maksimum) ve RMS değerini döndürür.
#[inline]
pub fn peak_rms(buf: &[f32]) -> (f32, f32) {
    if buf.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0.0f32;
    let mut sum = 0.0f32;
    for &s in buf {
        peak = peak.max(s.abs());
        sum += s * s;
    }
    (peak, (sum / buf.len() as f32).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_peak_and_rms() {
        let buf: Vec<f32> = (0..4800)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 100.0 / 48000.0).sin())
            .collect();
        let (p, r) = peak_rms(&buf);
        assert!((p - 1.0).abs() < 1e-3);
        assert!((r - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(peak_rms(&[]), (0.0, 0.0));
    }
}
