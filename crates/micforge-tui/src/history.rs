//! Geri al / yinele geçmişi.
//!
//! Parametre düzenlemelerini kaydeder. Fare sürüklemesi ve basılı ok tuşu gibi
//! art arda gelen küçük değişiklikler, bir `coalesce` penceresi içinde aynı
//! parametre için tek bir adım olarak birleştirilir.

use std::time::{Duration, Instant};

/// Tek bir geri alınabilir adım: `(anahtar, eski değer, yeni değer)` listesi.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<K> {
    pub changes: Vec<(K, f32, f32)>,
    at: Instant,
    bulk: bool,
}

pub struct History<K> {
    undo_stack: Vec<Entry<K>>,
    redo_stack: Vec<Entry<K>>,
    max_entries: usize,
    coalesce: Duration,
    /// `true` ise bir sonraki `record` yeni bir adım açar, mevcut en üst adıma birleşmez.
    sealed: bool,
}

/// İki değer, bit bit eşitse ya da farkı çok küçükse "değişmemiş" sayılır.
fn same(a: f32, b: f32) -> bool {
    a.to_bits() == b.to_bits() || (a - b).abs() < 1e-9
}

impl<K: Copy + Eq> History<K> {
    pub fn new(max_entries: usize, coalesce: Duration) -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_entries,
            coalesce,
            sealed: true,
        }
    }

    /// Tek bir düzenleme. `old == new` ise yok sayılır. Redo yığınını temizler.
    ///
    /// Son adım aynı anahtar için tekli bir adımsa ve `now - at <= coalesce` ise
    /// birleşir: özgün `old` korunur, `new` güncellenir, `at` kayar. Birleşme
    /// sonucu `old == new` olursa adım tamamen silinir.
    pub fn record(&mut self, key: K, old: f32, new: f32, now: Instant) {
        if same(old, new) {
            return;
        }
        self.redo_stack.clear();

        let window = self.coalesce;
        let sealed = self.sealed;
        if !sealed
            && let Some(last) = self.undo_stack.last_mut()
            && !last.bulk
            && last.changes.len() == 1
            && last.changes[0].0 == key
            && now.saturating_duration_since(last.at) <= window
        {
            last.changes[0].2 = new;
            last.at = now;
            if same(last.changes[0].1, new) {
                self.undo_stack.pop();
            }
            return;
        }

        self.undo_stack.push(Entry {
            changes: vec![(key, old, new)],
            at: now,
            bulk: false,
        });
        self.sealed = false;
        self.trim();
    }

    /// Birden çok anahtarı aynı anda değiştiren adım (preset yükleme, hepsini sıfırla).
    /// Asla birleşmez. `old == new` olan kayıtlar süzülür; hiçbiri kalmazsa hiçbir şey
    /// kaydedilmez ve redo yığını olduğu gibi kalır.
    pub fn record_bulk(&mut self, changes: Vec<(K, f32, f32)>, now: Instant) {
        let changes: Vec<(K, f32, f32)> = changes
            .into_iter()
            .filter(|&(_, old, new)| !same(old, new))
            .collect();
        if changes.is_empty() {
            return;
        }
        self.redo_stack.clear();
        self.undo_stack.push(Entry {
            changes,
            at: now,
            bulk: true,
        });
        self.sealed = true;
        self.trim();
    }

    /// Son adımı geri almak için uygulanacak `(anahtar, eski değer)` çiftleri.
    /// Sıra, adımın değişikliklerinin tersidir. Adım redo yığınına taşınır.
    pub fn undo(&mut self) -> Option<Vec<(K, f32)>> {
        let entry = self.undo_stack.pop()?;
        let out = entry
            .changes
            .iter()
            .rev()
            .map(|&(key, old, _)| (key, old))
            .collect();
        self.redo_stack.push(entry);
        self.sealed = true;
        Some(out)
    }

    /// Geri alınmış son adımı yeniden uygular: `(anahtar, yeni değer)` çiftleri, özgün sırada.
    pub fn redo(&mut self) -> Option<Vec<(K, f32)>> {
        let entry = self.redo_stack.pop()?;
        let out = entry
            .changes
            .iter()
            .map(|&(key, _, new)| (key, new))
            .collect();
        self.undo_stack.push(entry);
        self.sealed = true;
        Some(out)
    }

    #[cfg(test)]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    #[cfg(test)]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_len(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo_stack.len()
    }

    #[cfg(test)]
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.sealed = true;
    }

    /// Birleşmeyi keser: bir sonraki `record` yeni bir adım açar.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    /// Üst sınırı aşan en eski adımları siler.
    fn trim(&mut self) {
        while self.undo_stack.len() > self.max_entries {
            self.undo_stack.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum P {
        A,
        B,
    }

    const WINDOW: Duration = Duration::from_millis(100);

    fn ms(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn single_record_undo_then_redo() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);

        assert!(h.can_undo());
        assert_eq!(h.undo(), Some(vec![(P::A, 0.0)]));
        assert!(h.can_redo());
        assert_eq!(h.redo(), Some(vec![(P::A, 1.0)]));
        assert!(h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn coalesce_within_window_keeps_first_old_and_last_new() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.record(P::A, 1.0, 2.0, ms(t0, 50));
        h.record(P::A, 2.0, 3.0, ms(t0, 100));

        assert_eq!(h.undo_len(), 1);
        assert_eq!(h.undo_stack[0].changes, vec![(P::A, 0.0, 3.0)]);
    }

    #[test]
    fn outside_window_makes_two_entries() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.record(P::A, 1.0, 2.0, ms(t0, 200));

        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn different_key_makes_two_entries() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.record(P::B, 0.0, 1.0, t0);

        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn seal_breaks_coalescing() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.seal();
        h.record(P::A, 1.0, 2.0, t0);

        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn coalescing_back_to_original_removes_entry() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.record(P::A, 1.0, 0.0, ms(t0, 10));

        assert_eq!(h.undo_len(), 0);
        assert!(!h.can_undo());
    }

    #[test]
    fn record_clears_redo() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.undo();
        assert!(h.can_redo());

        h.record(P::B, 0.0, 1.0, ms(t0, 500));
        assert!(!h.can_redo());
        assert_eq!(h.redo_len(), 0);
    }

    #[test]
    fn undo_then_record_does_not_coalesce_into_moved_entry() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.undo();
        h.record(P::A, 0.0, 2.0, t0);

        assert_eq!(h.undo_len(), 1);
        assert_eq!(h.undo_stack[0].changes, vec![(P::A, 0.0, 2.0)]);
        assert!(!h.can_redo());
    }

    #[test]
    fn record_bulk_filters_noops_and_undo_reverses_order() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record_bulk(
            vec![(P::A, 0.0, 1.0), (P::B, 5.0, 5.0), (P::B, 2.0, 3.0)],
            t0,
        );

        assert_eq!(h.undo_len(), 1);
        assert_eq!(
            h.undo_stack[0].changes,
            vec![(P::A, 0.0, 1.0), (P::B, 2.0, 3.0)]
        );
        assert_eq!(h.undo(), Some(vec![(P::B, 2.0), (P::A, 0.0)]));
        assert_eq!(h.redo(), Some(vec![(P::A, 1.0), (P::B, 3.0)]));
    }

    #[test]
    fn record_bulk_all_noops_records_nothing() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.undo();
        h.record_bulk(vec![(P::A, 3.0, 3.0), (P::B, 0.0, 0.0)], t0);

        assert_eq!(h.undo_len(), 0);
        assert!(h.can_redo());
    }

    #[test]
    fn record_bulk_is_never_coalesced() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record_bulk(vec![(P::A, 0.0, 1.0)], t0);
        h.record(P::A, 1.0, 2.0, t0);

        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn max_entries_drops_oldest() {
        let t0 = Instant::now();
        let mut h = History::new(2, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.record(P::B, 0.0, 1.0, ms(t0, 500));
        h.record(P::A, 1.0, 2.0, ms(t0, 1000));

        assert_eq!(h.undo_len(), 2);
        assert_eq!(h.undo(), Some(vec![(P::A, 1.0)]));
        assert_eq!(h.undo(), Some(vec![(P::B, 0.0)]));
        assert_eq!(h.undo(), None);
    }

    #[test]
    fn equal_values_are_ignored() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 1.0, 1.0, t0);
        h.record(P::A, 1.0, 1.0 + 1e-12, t0);

        assert_eq!(h.undo_len(), 0);
        assert!(!h.can_redo());
    }

    #[test]
    fn undo_and_redo_on_empty_return_none() {
        let mut h: History<P> = History::new(10, WINDOW);

        assert_eq!(h.undo(), None);
        assert_eq!(h.redo(), None);
        assert!(!h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn clear_empties_both_stacks() {
        let t0 = Instant::now();
        let mut h = History::new(10, WINDOW);
        h.record(P::A, 0.0, 1.0, t0);
        h.record(P::B, 0.0, 1.0, ms(t0, 500));
        h.undo();
        h.clear();

        assert_eq!(h.undo_len(), 0);
        assert_eq!(h.redo_len(), 0);
    }
}
