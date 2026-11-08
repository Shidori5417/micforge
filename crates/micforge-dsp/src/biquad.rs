//! RBJ Audio EQ Cookbook biquad filtreleri (Transposed Direct Form II).
//! Katsayı ve durum hassasiyet için `f64`.

use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterType {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
    BandPass,
}

/// Normalize edilmiş katsayılar (a0 = 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coeffs {
    pub const IDENTITY: Coeffs = Coeffs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// RBJ cookbook formülleri. `freq` Nyquist'in altına sıkıştırılır.
    pub fn new(kind: FilterType, sample_rate: f32, freq: f32, q: f32, gain_db: f32) -> Self {
        let fs = sample_rate as f64;
        let f0 = (freq as f64).clamp(1.0, fs * 0.499);
        let q = (q as f64).max(0.01);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let w0 = 2.0 * PI * f0 / fs;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            FilterType::Peak => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            FilterType::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - k),
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            FilterType::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - k),
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
            FilterType::LowPass => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            FilterType::HighPass => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            FilterType::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            // Sabit 0 dB tepe kazançlı band-pass.
            FilterType::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        Coeffs {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Verilen frekanstaki genlik tepkisi (dB). EQ eğrisi ve testler için.
    pub fn magnitude_db(&self, freq: f32, sample_rate: f32) -> f32 {
        let w = 2.0 * PI * freq as f64 / sample_rate as f64;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        let mag2 = (nr * nr + ni * ni) / (dr * dr + di * di);
        (10.0 * mag2.max(1e-30).log10()) as f32
    }
}

#[derive(Clone, Debug)]
pub struct Biquad {
    c: Coeffs,
    z1: f64,
    z2: f64,
}

impl Default for Biquad {
    fn default() -> Self {
        Self {
            c: Coeffs::IDENTITY,
            z1: 0.0,
            z2: 0.0,
        }
    }
}

impl Biquad {
    #[inline]
    pub fn set_coeffs(&mut self, c: Coeffs) {
        self.c = c;
    }

    pub fn coeffs(&self) -> &Coeffs {
        &self.c
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    pub fn tick(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let c = &self.c;
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y as f32
    }

    #[inline]
    pub fn process(&mut self, buf: &mut [f32]) {
        for s in buf {
            *s = self.tick(*s);
        }
    }
}

/// N. dereceden Butterworth filtresini oluşturan 2. derece katların Q değerleri.
pub fn butterworth_qs(order: usize) -> impl Iterator<Item = f32> {
    (0..order / 2).map(move |k| {
        let theta = PI * (2 * k + 1) as f64 / (2 * order) as f64;
        (1.0 / (2.0 * theta.cos())) as f32
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const FS: f32 = 48_000.0;

    fn approx(a: f32, b: f32, tol: f32) {
        assert!((a - b).abs() <= tol, "{a} ≉ {b} (±{tol})");
    }

    #[test]
    fn peak_gain_at_center() {
        for g in [-12.0, 6.0, 18.0] {
            let c = Coeffs::new(FilterType::Peak, FS, 1000.0, 1.0, g);
            approx(c.magnitude_db(1000.0, FS), g, 0.01);
            approx(c.magnitude_db(20.0, FS), 0.0, 0.1);
        }
    }

    #[test]
    fn shelves_reach_gain() {
        let lo = Coeffs::new(FilterType::LowShelf, FS, 200.0, 0.707, 9.0);
        approx(lo.magnitude_db(10.0, FS), 9.0, 0.1);
        approx(lo.magnitude_db(200.0, FS), 4.5, 0.1);
        approx(lo.magnitude_db(10_000.0, FS), 0.0, 0.1);
        let hi = Coeffs::new(FilterType::HighShelf, FS, 4000.0, 0.707, -6.0);
        approx(hi.magnitude_db(20_000.0, FS), -6.0, 0.2);
        approx(hi.magnitude_db(50.0, FS), 0.0, 0.05);
    }

    #[test]
    fn pass_filters() {
        let hp = Coeffs::new(
            FilterType::HighPass,
            FS,
            100.0,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        approx(hp.magnitude_db(100.0, FS), -3.01, 0.05);
        approx(hp.magnitude_db(50.0, FS), -12.3, 0.2); // 12 dB/okt
        approx(hp.magnitude_db(5000.0, FS), 0.0, 0.01);
        let lp = Coeffs::new(
            FilterType::LowPass,
            FS,
            2000.0,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        approx(lp.magnitude_db(2000.0, FS), -3.01, 0.05);
        approx(lp.magnitude_db(50.0, FS), 0.0, 0.01);
    }

    #[test]
    fn notch_and_bandpass() {
        let n = Coeffs::new(FilterType::Notch, FS, 1000.0, 2.0, 0.0);
        assert!(n.magnitude_db(1000.0, FS) < -60.0);
        approx(n.magnitude_db(100.0, FS), 0.0, 0.1);
        let b = Coeffs::new(FilterType::BandPass, FS, 1000.0, 2.0, 0.0);
        approx(b.magnitude_db(1000.0, FS), 0.0, 0.01);
        assert!(b.magnitude_db(100.0, FS) < -20.0);
    }

    #[test]
    fn processing_matches_analytic_response() {
        let c = Coeffs::new(FilterType::Peak, FS, 1000.0, 1.0, 12.0);
        let mut f = Biquad::default();
        f.set_coeffs(c);
        let mut peak = 0.0f32;
        for i in 0..48_000 {
            let x = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / FS).sin();
            let y = f.tick(x);
            if i > 24_000 {
                peak = peak.max(y.abs());
            }
        }
        approx(20.0 * peak.log10(), 12.0, 0.05);
    }

    #[test]
    fn butterworth_q_values() {
        let q: Vec<f32> = butterworth_qs(4).collect();
        approx(q[0], 0.5412, 1e-3);
        approx(q[1], 1.3066, 1e-3);
        approx(
            butterworth_qs(2).next().unwrap(),
            std::f32::consts::FRAC_1_SQRT_2,
            1e-3,
        );
    }
}
