//! Parametre yumuşatma: ani değişimlerde "zipper noise" oluşmasın diye
//! tek kutuplu (one-pole) üstel yaklaşım.

/// Varsayılan yumuşatma süresi (zaman sabiti).
pub const DEFAULT_TIME_S: f32 = 0.02;

#[derive(Clone, Debug)]
pub struct Smoother {
    current: f32,
    target: f32,
    /// Örnek başına çarpan: `current = target + (current - target) * coef`
    coef: f32,
    time_s: f32,
}

impl Smoother {
    pub fn new(value: f32, sample_rate: f32) -> Self {
        let mut s = Self {
            current: value,
            target: value,
            coef: 0.0,
            time_s: DEFAULT_TIME_S,
        };
        s.set_sample_rate(sample_rate);
        s
    }

    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.coef = (-1.0 / (self.time_s * sample_rate)).exp();
    }

    #[inline]
    pub fn set_target(&mut self, v: f32) {
        self.target = v;
    }

    /// Yumuşatmayı atlayıp doğrudan değere geçer.
    pub fn snap(&mut self, v: f32) {
        self.current = v;
        self.target = v;
    }

    #[inline]
    pub fn value(&self) -> f32 {
        self.current
    }

    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    /// Hedefe henüz ulaşılmadıysa `true`.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.current != self.target
    }

    /// Bir örnek ilerler ve yeni değeri döndürür.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        self.advance(1)
    }

    /// `n` örnek ilerler (blok tabanlı katsayı güncellemesi için).
    #[inline]
    pub fn advance(&mut self, n: usize) -> f32 {
        if self.current != self.target {
            let d = (self.current - self.target) * self.coef.powi(n as i32);
            let next = self.target + d;
            // Hedefe çok yaklaşınca ya da f32 çözünürlüğü yüzünden ilerleme
            // durunca yapış: sonsuza dek "aktif" kalmasın.
            self.current = if d.abs() <= 1e-6 || next == self.current {
                self.target
            } else {
                next
            };
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converges_and_settles() {
        let mut s = Smoother::new(0.0, 48_000.0);
        s.set_target(1.0);
        let first = s.next_value();
        assert!(
            first > 0.0 && first < 0.01,
            "ilk adım küçük olmalı: {first}"
        );
        for _ in 0..48_000 {
            s.next_value();
        }
        assert_eq!(s.value(), 1.0);
        assert!(!s.is_active());
    }

    #[test]
    fn advance_matches_steps() {
        let mut a = Smoother::new(0.0, 48_000.0);
        let mut b = a.clone();
        a.set_target(1.0);
        b.set_target(1.0);
        for _ in 0..32 {
            a.next_value();
        }
        b.advance(32);
        assert!((a.value() - b.value()).abs() < 1e-5);
    }
}
