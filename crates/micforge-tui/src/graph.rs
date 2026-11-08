//! EQ eğrisi + spektrum grafiği.
//!
//! Tek bir çizimde log frekans ekseni (f_min..f_max) üzerinde:
//! (a) giriş/çıkış gerçek zamanlı spektrumu (dBFS),
//! (b) EQ toplam frekans cevabı (dB) eğrisi,
//! (c) numaralı bant işaretçileri.
//!
//! Yüksek çözünürlük için braille hücreleri doğrudan `Buffer`'a yazılır
//! (hücre başına 2x4 nokta). İşaretçi / fare eşlemesi hücre çözünürlüğündedir:
//! hücre merkezi esas alınır, böylece `freq_to_x` ∘ `x_to_freq` tam tur döner.

use crate::theme::th;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Widget};

/// Sol eksen (EQ dB etiketleri) genişliği: 3 karakter + 1 boşluk.
const LEFT_MARGIN: u16 = 4;
/// Sağ eksen (spektrum dBFS etiketleri) genişliği: 1 boşluk + 3 karakter.
const RIGHT_MARGIN: u16 = 4;
/// Çizim için gereken en küçük iç alan (kenarlık hariç).
const MIN_INNER_W: u16 = 16;
const MIN_INNER_H: u16 = 4;

/// Etiket konulacak frekanslar (Hz).
const FREQ_TICKS: [f32; 8] = [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0];
/// dB ızgarası için aday adım değerleri.
const DB_STEPS: [f32; 10] = [1.0, 2.0, 3.0, 6.0, 10.0, 12.0, 20.0, 30.0, 60.0, 100.0];

/// Braille nokta bitleri: `DOT_BITS[dx][dy]` (dx 0..2, dy 0..4).
const DOT_BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpectrumMode {
    /// Giriş + çıkış aynı çizimde (farklı renk, yalnızca çizgi; dolgu iz örtmesin diye yok).
    Overlay,
    /// Dikey ikiye bölünmüş: üstte giriş, altta çıkış (EQ eğrisi + işaretçiler altta).
    Split,
    /// Spektrum yok; yalnızca eğri + işaretçiler.
    Off,
}

#[derive(Clone, Copy, Debug)]
pub struct GraphView {
    pub f_min: f32,
    pub f_max: f32,
    pub eq_db_min: f32,
    pub eq_db_max: f32,
    pub spec_db_min: f32,
    pub spec_db_max: f32,
}

impl Default for GraphView {
    fn default() -> Self {
        Self {
            f_min: 20.0,
            f_max: 20000.0,
            eq_db_min: -24.0,
            eq_db_max: 24.0,
            spec_db_min: -90.0,
            spec_db_max: 0.0,
        }
    }
}

pub struct BandMarker {
    pub freq: f32,
    pub gain_db: f32,
    pub label: String,
    pub selected: bool,
    pub enabled: bool,
}

pub struct GraphData<'a> {
    /// Sütun başına dBFS; sütunlar f_min..f_max arasında log aralıklı (uzunluk serbest).
    pub spectrum_in: Option<&'a [f32]>,
    pub spectrum_out: Option<&'a [f32]>,
    /// Frekans (Hz) → EQ cevabı (dB).
    pub curve: &'a dyn Fn(f32) -> f32,
    pub markers: &'a [BandMarker],
}

// ---------------------------------------------------------------------------
// Koordinat eşlemesi
// ---------------------------------------------------------------------------

/// Frekansın log ekseni üzerindeki konumu, 0..=1.
fn log_frac(view: &GraphView, freq: f32) -> f32 {
    let lo = view.f_min.max(1e-3);
    let hi = view.f_max.max(lo * 1.0001);
    let f = if freq.is_finite() { freq.max(lo) } else { lo };
    ((f / lo).ln() / (hi / lo).ln()).clamp(0.0, 1.0)
}

/// 0..=1 konumdan frekans.
fn frac_to_freq(view: &GraphView, t: f32) -> f32 {
    let lo = view.f_min.max(1e-3);
    let hi = view.f_max.max(lo * 1.0001);
    lo * (hi / lo).powf(t.clamp(0.0, 1.0))
}

/// Değeri dikey hücre satırına çevirir (hi üstte, lo altta).
fn val_to_y(plot: Rect, lo: f32, hi: f32, v: f32) -> u16 {
    if plot.height == 0 {
        return plot.y;
    }
    let span = (hi - lo).max(1e-6);
    let v = if v.is_finite() { v } else { lo };
    let t = ((hi - v) / span).clamp(0.0, 1.0);
    let row = (t * plot.height as f32).floor() as u16;
    plot.y + row.min(plot.height - 1)
}

fn y_to_val(plot: Rect, lo: f32, hi: f32, y: u16) -> f32 {
    if plot.height == 0 {
        return hi;
    }
    let row = y.clamp(plot.y, plot.y + plot.height - 1) - plot.y;
    let t = (row as f32 + 0.5) / plot.height as f32;
    hi - t * (hi - lo)
}

pub fn freq_to_x(plot: Rect, view: &GraphView, freq: f32) -> u16 {
    if plot.width == 0 {
        return plot.x;
    }
    let col = (log_frac(view, freq) * plot.width as f32).floor() as u16;
    plot.x + col.min(plot.width - 1)
}

pub fn x_to_freq(plot: Rect, view: &GraphView, x: u16) -> f32 {
    if plot.width == 0 {
        return view.f_min;
    }
    let col = x.clamp(plot.x, plot.x + plot.width - 1) - plot.x;
    frac_to_freq(view, (col as f32 + 0.5) / plot.width as f32)
}

pub fn gain_to_y(plot: Rect, view: &GraphView, db: f32) -> u16 {
    val_to_y(plot, view.eq_db_min, view.eq_db_max, db)
}

pub fn y_to_gain(plot: Rect, view: &GraphView, y: u16) -> f32 {
    y_to_val(plot, view.eq_db_min, view.eq_db_max, y)
}

/// (x, y) hücresinin 1 hücre yakınındaki en yakın işaretçinin indeksi.
pub fn hit_marker(
    plot: Rect,
    view: &GraphView,
    markers: &[BandMarker],
    x: u16,
    y: u16,
) -> Option<usize> {
    if plot.width == 0 || plot.height == 0 {
        return None;
    }
    markers
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            let mx = freq_to_x(plot, view, m.freq) as i32;
            let my = gain_to_y(plot, view, m.gain_db) as i32;
            let dx = mx - x as i32;
            let dy = my - y as i32;
            (dx.abs() <= 1 && dy.abs() <= 1).then_some((dx * dx + dy * dy, i))
        })
        .min()
        .map(|(_, i)| i)
}

/// Çağıranın hesaplaması gereken spektrum sütun sayısı (braille: hücre başına 2).
pub fn spectrum_columns(plot: Rect) -> usize {
    plot.width as usize * 2
}

// ---------------------------------------------------------------------------
// Braille nokta ızgarası
// ---------------------------------------------------------------------------

const L_IN: usize = 0;
const L_OUT: usize = 1;
const L_CURVE: usize = 2;

/// Üç katmanlı (giriş, çıkış, eğri) braille nokta tamponu.
struct Dots {
    w: usize,
    h: usize,
    bits: [Vec<u8>; 3],
}

impl Dots {
    fn new(plot: Rect) -> Self {
        let (w, h) = (plot.width as usize, plot.height as usize);
        Self {
            w,
            h,
            bits: [vec![0; w * h], vec![0; w * h], vec![0; w * h]],
        }
    }

    /// Nokta genişliği / yüksekliği.
    fn dot_w(&self) -> usize {
        self.w * 2
    }

    fn dot_h(&self) -> usize {
        self.h * 4
    }

    fn set(&mut self, layer: usize, dx: usize, dy: usize) {
        if dx >= self.dot_w() || dy >= self.dot_h() {
            return;
        }
        let idx = (dy / 4) * self.w + dx / 2;
        self.bits[layer][idx] |= DOT_BITS[dx % 2][dy % 4];
    }

    /// Aynı sütunda iki satır arası dikey çizgi (dahil).
    fn vline(&mut self, layer: usize, dx: usize, y0: usize, y1: usize) {
        for dy in y0.min(y1)..=y0.max(y1) {
            self.set(layer, dx, dy);
        }
    }

    /// Spektrum: üst kenar çizgisi (+ isteğe bağlı satranç tahtası dolgu).
    fn spectrum(&mut self, layer: usize, data: &[f32], view: &GraphView, fill: bool) {
        let (dw, dh) = (self.dot_w(), self.dot_h());
        if data.is_empty() || dw == 0 || dh == 0 {
            return;
        }
        let span = (view.spec_db_max - view.spec_db_min).max(1e-6);
        let mut prev: Option<usize> = None;
        for d in 0..dw {
            let v = sample_column(data, d, dw, view.spec_db_min);
            let t = ((v - view.spec_db_min) / span).clamp(0.0, 1.0);
            if t <= 0.0 {
                prev = None;
                continue;
            }
            let ytop = (((1.0 - t) * (dh - 1) as f32).round() as usize).min(dh - 1);
            match prev {
                Some(p) => self.vline(layer, d, p, ytop),
                None => self.set(layer, d, ytop),
            }
            if fill {
                for dy in ytop..dh {
                    if (d + dy) % 2 == 0 {
                        self.set(layer, d, dy);
                    }
                }
            }
            prev = Some(ytop);
        }
    }

    /// EQ eğrisi: her nokta sütununda değerlendirilir, komşular dikey çizgiyle bağlanır.
    fn curve(&mut self, view: &GraphView, curve: &dyn Fn(f32) -> f32) {
        let (dw, dh) = (self.dot_w(), self.dot_h());
        if dw == 0 || dh == 0 {
            return;
        }
        let span = (view.eq_db_max - view.eq_db_min).max(1e-6);
        let mut prev: Option<usize> = None;
        for d in 0..dw {
            let f = frac_to_freq(view, (d as f32 + 0.5) / dw as f32);
            let db = curve(f);
            if !db.is_finite() {
                prev = None;
                continue;
            }
            let t = ((view.eq_db_max - db) / span).clamp(0.0, 1.0);
            let y = ((t * dh as f32).floor() as usize).min(dh - 1);
            match prev {
                Some(p) => self.vline(L_CURVE, d, p, y),
                None => self.set(L_CURVE, d, y),
            }
            prev = Some(y);
        }
    }

    /// Katmanları birleştirip hücrelere yazar. Öncelik: eğri > çıkış > giriş.
    fn flush(&self, buf: &mut Buffer, plot: Rect, colors: [Color; 3]) {
        for cy in 0..self.h {
            for cx in 0..self.w {
                let i = cy * self.w + cx;
                let (inp, out, cur) = (
                    self.bits[L_IN][i],
                    self.bits[L_OUT][i],
                    self.bits[L_CURVE][i],
                );
                // Eğri varsa yalnızca eğri görünür (net kalsın).
                let (bits, color) = if cur != 0 {
                    (cur, colors[L_CURVE])
                } else if out != 0 {
                    (out | inp, colors[L_OUT])
                } else if inp != 0 {
                    (inp, colors[L_IN])
                } else {
                    continue;
                };
                if let Some(ch) = char::from_u32(0x2800 + bits as u32) {
                    put(
                        buf,
                        plot.x + cx as u16,
                        plot.y + cy as u16,
                        ch,
                        Style::new().fg(color),
                    );
                }
            }
        }
    }
}

/// Nokta sütunu `d` için spektrum değeri: veri yoğunsa aralık maksimumu,
/// seyrekse doğrusal ara değer.
fn sample_column(data: &[f32], d: usize, dw: usize, floor_db: f32) -> f32 {
    let n = data.len();
    let val = |i: usize| {
        let v = data[i];
        if v.is_finite() { v } else { floor_db }
    };
    if n >= dw {
        let lo = d * n / dw;
        let hi = ((d + 1) * n / dw).max(lo + 1).min(n);
        (lo..hi).map(val).fold(f32::NEG_INFINITY, f32::max)
    } else {
        let pos = ((d as f32 + 0.5) / dw as f32 * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
        let i0 = pos.floor() as usize;
        let i1 = (i0 + 1).min(n - 1);
        let f = pos - i0 as f32;
        val(i0) * (1.0 - f) + val(i1) * f
    }
}

// ---------------------------------------------------------------------------
// Hücre yardımcıları
// ---------------------------------------------------------------------------

fn put(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(ch).set_style(style);
    }
}

/// `max_w` karakterle sınırlı metin yazar.
fn text(buf: &mut Buffer, x: u16, y: u16, s: &str, max_w: usize, style: Style) {
    buf.set_stringn(x, y, s, max_w, style);
}

fn fmt_db_signed(v: f32) -> String {
    let r = v.round() as i32;
    if r > 0 {
        format!("+{r}")
    } else {
        r.to_string()
    }
}

/// Eşit aralıklı dB ızgara değerleri; her çentik en az 3 satır arayla.
fn tick_values(lo: f32, hi: f32, rows: u16) -> Vec<f32> {
    let span = hi - lo;
    if span.is_nan() || span <= 0.0 || rows == 0 {
        return Vec::new();
    }
    let step = DB_STEPS
        .iter()
        .copied()
        .find(|s| rows as f32 * s / span >= 3.0)
        .unwrap_or(DB_STEPS[DB_STEPS.len() - 1]);
    let k0 = (lo / step).ceil() as i32;
    let k1 = (hi / step).floor() as i32;
    if k1 - k0 > 200 {
        return Vec::new();
    }
    (k0..=k1).map(|k| k as f32 * step).collect()
}

/// Sol eksen etiketi: sağa yaslı, çizim alanının solunda.
fn label_left(buf: &mut Buffer, plot: Rect, y: u16, s: &str, style: Style) {
    let s: String = s.chars().take(LEFT_MARGIN as usize - 1).collect();
    let w = s.chars().count() as u16;
    text(buf, plot.x - 1 - w, y, &s, w as usize, style);
}

/// Sağ eksen etiketi: sola yaslı, çizim alanının sağında.
fn label_right(buf: &mut Buffer, plot: Rect, y: u16, s: &str, style: Style) {
    text(
        buf,
        plot.right() + 1,
        y,
        s,
        RIGHT_MARGIN as usize - 1,
        style,
    );
}

// ---------------------------------------------------------------------------
// Çizim
// ---------------------------------------------------------------------------

/// Tek bir çizim alanının içeriği.
struct PlotSpec<'a> {
    input: Option<(&'a [f32], bool)>,
    output: Option<(&'a [f32], bool)>,
    curve: Option<&'a dyn Fn(f32) -> f32>,
    /// true: yatay ızgara + sol etiketler EQ dB ölçeğinde; false: spektrum ölçeğinde.
    eq_grid: bool,
    /// Sağ kenarda spektrum dBFS etiketleri.
    spec_labels: bool,
    legend: &'a [(&'a str, Color)],
}

fn draw_grid(buf: &mut Buffer, plot: Rect, view: &GraphView, eq_grid: bool) {
    let (lo, hi) = if eq_grid {
        (view.eq_db_min, view.eq_db_max)
    } else {
        (view.spec_db_min, view.spec_db_max)
    };
    for v in tick_values(lo, hi, plot.height) {
        let y = val_to_y(plot, lo, hi, v);
        let zero = eq_grid && v == 0.0;
        let (ch, color) = if zero {
            ('─', th().muted)
        } else {
            ('┈', th().grid)
        };
        for x in plot.x..plot.right() {
            put(buf, x, y, ch, Style::new().fg(color));
        }
    }
    for f in FREQ_TICKS {
        if f > view.f_min && f < view.f_max {
            let x = freq_to_x(plot, view, f);
            for y in plot.y..plot.bottom() {
                put(buf, x, y, '┊', Style::new().fg(th().grid));
            }
        }
    }
}

fn draw_plot(buf: &mut Buffer, plot: Rect, view: &GraphView, spec: &PlotSpec) {
    draw_grid(buf, plot, view, spec.eq_grid);

    let mut dots = Dots::new(plot);
    if let Some((d, fill)) = spec.input {
        dots.spectrum(L_IN, d, view, fill);
    }
    if let Some((d, fill)) = spec.output {
        dots.spectrum(L_OUT, d, view, fill);
    }
    if let Some(c) = spec.curve {
        dots.curve(view, c);
    }
    dots.flush(buf, plot, [th().spec_in, th().spec_out, th().curve]);

    // Sol eksen: EQ ölçeği (yalnızca eğri çizimi) ya da spektrum ölçeği.
    let left_style = Style::new().fg(if spec.eq_grid { th().curve } else { th().muted });
    let (lo, hi) = if spec.eq_grid {
        (view.eq_db_min, view.eq_db_max)
    } else {
        (view.spec_db_min, view.spec_db_max)
    };
    if spec.eq_grid {
        for v in tick_values(lo, hi, plot.height) {
            let y = val_to_y(plot, lo, hi, v);
            label_left(buf, plot, y, &fmt_db_signed(v), left_style);
        }
    }
    // Sağ eksen: spektrum dBFS.
    if spec.spec_labels {
        let style = Style::new().fg(th().muted);
        for v in tick_values(view.spec_db_min, view.spec_db_max, plot.height) {
            let y = val_to_y(plot, view.spec_db_min, view.spec_db_max, v);
            label_right(buf, plot, y, &(v.round() as i32).to_string(), style);
        }
    }

    // Gösterge (sağ üst köşe).
    let total: usize = spec.legend.iter().map(|(s, _)| s.len() + 1).sum();
    if !spec.legend.is_empty() && plot.width as usize >= total + 8 {
        let mut x = plot.right() - total as u16;
        for (s, color) in spec.legend {
            text(
                buf,
                x,
                plot.y,
                s,
                s.len(),
                Style::new().fg(*color).add_modifier(Modifier::BOLD),
            );
            x += s.len() as u16 + 1;
        }
    }
}

fn draw_freq_labels(buf: &mut Buffer, plot: Rect, view: &GraphView, y: u16, min_x: u16) {
    let mut next_free = min_x;
    for f in FREQ_TICKS {
        if f <= view.f_min || f >= view.f_max {
            continue;
        }
        let label = if f >= 1000.0 {
            format!("{}k", (f / 1000.0).round() as u32)
        } else {
            (f.round() as u32).to_string()
        };
        let w = label.len() as u16;
        let cx = freq_to_x(plot, view, f);
        let start = cx.saturating_sub(w / 2).max(min_x);
        if start < next_free || start + w > plot.right() + RIGHT_MARGIN {
            continue;
        }
        text(
            buf,
            start,
            y,
            &label,
            w as usize,
            Style::new().fg(th().muted),
        );
        next_free = start + w + 1;
    }
}

fn draw_markers(buf: &mut Buffer, plot: Rect, view: &GraphView, markers: &[BandMarker]) {
    // Seçili işaretçi en son (üstte) çizilir.
    let order = markers
        .iter()
        .filter(|m| !m.selected)
        .chain(markers.iter().filter(|m| m.selected));
    for m in order {
        let label: String = if m.label.is_empty() {
            "*".to_string()
        } else {
            m.label.chars().take(plot.width as usize).collect()
        };
        let w = label.chars().count() as u16;
        let x = freq_to_x(plot, view, m.freq);
        let y = gain_to_y(plot, view, m.gain_db);
        let start = x.saturating_sub(w / 2).max(plot.x).min(plot.right() - w);
        let style = if m.selected {
            Style::new()
                .fg(th().badge_fg)
                .bg(th().curve)
                .add_modifier(Modifier::BOLD)
        } else if m.enabled {
            Style::new().fg(th().badge_fg).bg(th().accent)
        } else {
            Style::new().fg(th().dim).add_modifier(Modifier::DIM)
        };
        text(buf, start, y, &label, w as usize, style);
    }
}

/// Kenarlıklı bir blok ve içinde grafiği çizer. EQ eğrisi + işaretçilerin bulunduğu
/// çizim alanını döndürür (fare eşlemesi için). Alan çok küçükse yalnızca blok çizilir
/// ve boyutu sıfır bir `Rect` döner.
pub fn render(
    buf: &mut Buffer,
    area: Rect,
    title: &str,
    view: &GraphView,
    mode: SpectrumMode,
    data: &GraphData,
) -> Rect {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().fg(th().muted));
    let inner = block.inner(area);
    block.render(area, buf);

    if inner.width < MIN_INNER_W || inner.height < MIN_INNER_H {
        if inner.width > 0 && inner.height > 0 {
            text(
                buf,
                inner.x,
                inner.y,
                crate::i18n::l("Alan çok küçük", "Area too small"),
                inner.width as usize,
                Style::new().fg(th().dim),
            );
        }
        return Rect::new(inner.x, inner.y, 0, 0);
    }

    // Çizim alanı: solda/sağda eksen payı, altta frekans etiketi satırı.
    let all = Rect::new(
        inner.x + LEFT_MARGIN,
        inner.y,
        inner.width - LEFT_MARGIN - RIGHT_MARGIN,
        inner.height - 1,
    );
    let label_y = all.bottom();

    let main = match mode {
        SpectrumMode::Overlay => {
            let spec = PlotSpec {
                input: data.spectrum_in.map(|d| (d, false)),
                output: data.spectrum_out.map(|d| (d, false)),
                curve: Some(data.curve),
                eq_grid: true,
                spec_labels: true,
                legend: &[("IN", th().spec_in), ("OUT", th().spec_out)],
            };
            draw_plot(buf, all, view, &spec);
            all
        }
        SpectrumMode::Split => {
            let top_h = all.height / 2;
            let top = Rect::new(all.x, all.y, all.width, top_h);
            let bottom = Rect::new(all.x, all.y + top_h, all.width, all.height - top_h);
            let top_spec = PlotSpec {
                input: data.spectrum_in.map(|d| (d, true)),
                output: None,
                curve: None,
                eq_grid: false,
                spec_labels: true,
                legend: &[("IN", th().spec_in)],
            };
            draw_plot(buf, top, view, &top_spec);
            let bottom_spec = PlotSpec {
                input: None,
                output: data.spectrum_out.map(|d| (d, true)),
                curve: Some(data.curve),
                eq_grid: true,
                spec_labels: true,
                legend: &[("OUT", th().spec_out)],
            };
            draw_plot(buf, bottom, view, &bottom_spec);
            bottom
        }
        SpectrumMode::Off => {
            let spec = PlotSpec {
                input: None,
                output: None,
                curve: Some(data.curve),
                eq_grid: true,
                spec_labels: false,
                legend: &[],
            };
            draw_plot(buf, all, view, &spec);
            all
        }
    };

    draw_freq_labels(buf, all, view, label_y, inner.x);
    draw_markers(buf, main, view, data.markers);
    main
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bell(f: f32) -> f32 {
        let x = (f / 1000.0).ln();
        8.0 * (-x * x).exp()
    }

    fn markers() -> Vec<BandMarker> {
        vec![
            BandMarker {
                freq: 100.0,
                gain_db: -6.0,
                label: "1".into(),
                selected: false,
                enabled: true,
            },
            BandMarker {
                freq: 1000.0,
                gain_db: 8.0,
                label: "2".into(),
                selected: true,
                enabled: true,
            },
            BandMarker {
                freq: 8000.0,
                gain_db: 0.0,
                label: "B".into(),
                selected: false,
                enabled: false,
            },
        ]
    }

    fn spectrum(n: usize, offset: f32) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f32 / n as f32;
                -30.0 - 50.0 * t + offset + 10.0 * (t * 40.0).sin()
            })
            .collect()
    }

    fn do_render(buf: &mut Buffer, area: Rect, mode: SpectrumMode) -> Rect {
        let (sin, sout) = (spectrum(300, 0.0), spectrum(300, 6.0));
        let m = markers();
        let data = GraphData {
            spectrum_in: Some(&sin),
            spectrum_out: Some(&sout),
            curve: &bell,
            markers: &m,
        };
        render(buf, area, " Grafik ", &GraphView::default(), mode, &data)
    }

    fn dump(buf: &Buffer) -> String {
        let a = buf.area;
        (a.y..a.bottom())
            .map(|y| {
                (a.x..a.right())
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn freq_round_trip_within_one_cell() {
        let view = GraphView::default();
        let plot = Rect::new(10, 2, 80, 20);
        for x in plot.x..plot.right() {
            let f = x_to_freq(plot, &view, x);
            assert!((view.f_min..=view.f_max).contains(&f), "f={f}");
            assert_eq!(freq_to_x(plot, &view, f), x);
        }
        assert_eq!(freq_to_x(plot, &view, 0.0), plot.x);
        assert_eq!(freq_to_x(plot, &view, f32::NAN), plot.x);
        assert_eq!(freq_to_x(plot, &view, 1e9), plot.right() - 1);
        // Monoton: 1 kHz yaklaşık ortada.
        let mid = freq_to_x(plot, &view, 1000.0);
        assert!(mid > plot.x + 30 && mid < plot.x + 50, "mid={mid}");
    }

    #[test]
    fn gain_round_trip_within_one_cell() {
        let view = GraphView::default();
        let plot = Rect::new(10, 2, 80, 20);
        for y in plot.y..plot.bottom() {
            let g = y_to_gain(plot, &view, y);
            assert!(g > view.eq_db_min && g < view.eq_db_max);
            assert_eq!(gain_to_y(plot, &view, g), y);
        }
        assert_eq!(gain_to_y(plot, &view, 100.0), plot.y);
        assert_eq!(gain_to_y(plot, &view, -100.0), plot.bottom() - 1);
        // Yukarı = daha yüksek kazanç.
        assert!(gain_to_y(plot, &view, 12.0) < gain_to_y(plot, &view, -12.0));
    }

    #[test]
    fn mapping_on_degenerate_plot() {
        let view = GraphView::default();
        let plot = Rect::new(3, 4, 0, 0);
        assert_eq!(freq_to_x(plot, &view, 1000.0), 3);
        assert_eq!(gain_to_y(plot, &view, 0.0), 4);
        let _ = x_to_freq(plot, &view, 7);
        let _ = y_to_gain(plot, &view, 7);
        assert_eq!(hit_marker(plot, &view, &markers(), 3, 4), None);
    }

    #[test]
    fn hit_marker_nearest_within_one_cell() {
        let view = GraphView::default();
        let plot = Rect::new(10, 2, 80, 20);
        let m = markers();
        let (x1, y1) = (freq_to_x(plot, &view, 1000.0), gain_to_y(plot, &view, 8.0));
        assert_eq!(hit_marker(plot, &view, &m, x1, y1), Some(1));
        assert_eq!(hit_marker(plot, &view, &m, x1 + 1, y1 - 1), Some(1));
        assert_eq!(hit_marker(plot, &view, &m, x1 + 2, y1), None);
        assert_eq!(hit_marker(plot, &view, &m, x1, y1 + 2), None);
        let (x0, y0) = (freq_to_x(plot, &view, 100.0), gain_to_y(plot, &view, -6.0));
        assert_eq!(hit_marker(plot, &view, &m, x0, y0), Some(0));
        // İki işaretçi komşu hücrelerde: en yakın olan seçilir.
        let close = vec![
            BandMarker {
                freq: 1000.0,
                gain_db: 0.0,
                label: "a".into(),
                selected: false,
                enabled: true,
            },
            BandMarker {
                freq: x_to_freq(plot, &view, x1 + 1),
                gain_db: 0.0,
                label: "b".into(),
                selected: false,
                enabled: true,
            },
        ];
        let y = gain_to_y(plot, &view, 0.0);
        assert_eq!(hit_marker(plot, &view, &close, x1, y), Some(0));
        assert_eq!(hit_marker(plot, &view, &close, x1 + 1, y), Some(1));
    }

    #[test]
    fn spectrum_columns_is_double_width() {
        assert_eq!(spectrum_columns(Rect::new(0, 0, 50, 10)), 100);
        assert_eq!(spectrum_columns(Rect::new(0, 0, 0, 0)), 0);
    }

    #[test]
    fn render_all_modes_draw_curve_and_marker() {
        let view = GraphView::default();
        for mode in [
            SpectrumMode::Overlay,
            SpectrumMode::Split,
            SpectrumMode::Off,
        ] {
            let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
            let plot = do_render(&mut buf, Rect::new(0, 0, 100, 30), mode);
            assert!(
                plot.width >= 8 && plot.height >= 3,
                "{mode:?} plot={plot:?}"
            );
            assert!(plot.right() <= 100 && plot.bottom() <= 30);

            // Eğri: sarı braille hücreleri.
            let curve_cells = (plot.y..plot.bottom())
                .flat_map(|y| (plot.x..plot.right()).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let c = &buf[(x, y)];
                    c.fg == th().curve
                        && c.symbol()
                            .chars()
                            .next()
                            .is_some_and(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
                })
                .count();
            assert!(
                curve_cells >= plot.width as usize / 2,
                "{mode:?} curve cells={curve_cells}"
            );

            // İşaretçi etiketi tam kendi hücresinde.
            let (mx, my) = (freq_to_x(plot, &view, 1000.0), gain_to_y(plot, &view, 8.0));
            assert_eq!(buf[(mx, my)].symbol(), "2", "{mode:?}");
            assert_eq!(buf[(mx, my)].bg, th().curve);
            let (bx, by) = (freq_to_x(plot, &view, 8000.0), gain_to_y(plot, &view, 0.0));
            assert_eq!(buf[(bx, by)].symbol(), "B");

            // Spektrum renkleri moda göre.
            let count = |col: Color| {
                buf.content()
                    .iter()
                    .filter(|c| {
                        c.fg == col
                            && c.symbol() != " "
                            && c.symbol()
                                .chars()
                                .next()
                                .is_some_and(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
                    })
                    .count()
            };
            match mode {
                SpectrumMode::Off => {
                    assert_eq!(count(th().spec_out), 0);
                    assert_eq!(count(th().spec_in), 0);
                }
                _ => {
                    assert!(count(th().spec_out) > 0, "{mode:?} çıkış spektrumu yok");
                    assert!(count(th().spec_in) > 0, "{mode:?} giriş spektrumu yok");
                }
            }
        }
    }

    #[test]
    fn split_returns_bottom_half() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        let overlay = do_render(&mut buf, Rect::new(0, 0, 100, 30), SpectrumMode::Overlay);
        let split = do_render(&mut buf, Rect::new(0, 0, 100, 30), SpectrumMode::Split);
        assert_eq!(split.x, overlay.x);
        assert_eq!(split.width, overlay.width);
        assert_eq!(split.bottom(), overlay.bottom());
        assert!(split.y > overlay.y && split.height < overlay.height);
    }

    #[test]
    fn labels_stay_outside_plot() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        let plot = do_render(&mut buf, Rect::new(0, 0, 100, 30), SpectrumMode::Overlay);
        let all = dump(&buf);
        for needle in ["+24", "-24", "1k", "10k", "100", "-90"] {
            assert!(all.contains(needle), "etiket yok: {needle}");
        }
        // Sol etiketler çizim alanının solunda, frekans etiketleri altında.
        let y = gain_to_y(plot, &GraphView::default(), 12.0);
        let left: String = (0..plot.x)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect();
        assert!(left.contains("+12"), "left={left:?}");
        let bottom: String = (0..100)
            .map(|x| buf[(x, plot.bottom())].symbol().to_string())
            .collect();
        assert!(bottom.contains("1k"), "bottom={bottom:?}");
    }

    #[test]
    fn tiny_and_empty_areas_do_not_panic() {
        let sizes = [
            (0, 0),
            (1, 1),
            (2, 2),
            (5, 3),
            (9, 9),
            (15, 5),
            (16, 4),
            (19, 6),
            (30, 3),
            (30, 4),
            (40, 5),
        ];
        for mode in [
            SpectrumMode::Overlay,
            SpectrumMode::Split,
            SpectrumMode::Off,
        ] {
            for (w, h) in sizes {
                let mut buf = Buffer::empty(Rect::new(0, 0, w.max(1), h.max(1)));
                let plot = do_render(&mut buf, Rect::new(0, 0, w, h), mode);
                assert!(plot.right() <= w && plot.bottom() <= h, "{w}x{h} {plot:?}");
                // Eşleme fonksiyonları da çökmemeli.
                let view = GraphView::default();
                let _ = freq_to_x(plot, &view, 440.0);
                let _ = gain_to_y(plot, &view, 3.0);
                let _ = hit_marker(plot, &view, &markers(), 1, 1);
            }
            // Buffer'ın dışına taşan alan kırpılır.
            let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
            let plot = do_render(&mut buf, Rect::new(20, 5, 100, 100), mode);
            assert!(plot.right() <= 40 && plot.bottom() <= 10);
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, 0, 0));
        assert_eq!(
            do_render(&mut buf, Rect::new(0, 0, 0, 0), SpectrumMode::Overlay).width,
            0
        );
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let nan = |_f: f32| f32::NAN;
        let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 500.0];
        let m = vec![BandMarker {
            freq: f32::NAN,
            gain_db: f32::INFINITY,
            label: String::new(),
            selected: true,
            enabled: true,
        }];
        let data = GraphData {
            spectrum_in: Some(&bad),
            spectrum_out: Some(&[]),
            curve: &nan,
            markers: &m,
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 20));
        for mode in [
            SpectrumMode::Overlay,
            SpectrumMode::Split,
            SpectrumMode::Off,
        ] {
            render(
                &mut buf,
                Rect::new(0, 0, 60, 20),
                "t",
                &GraphView::default(),
                mode,
                &data,
            );
        }
    }

    #[test]
    fn print_overlay_frame() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        let plot = do_render(&mut buf, Rect::new(0, 0, 100, 30), SpectrumMode::Overlay);
        println!("plot = {plot:?}\n{}", dump(&buf));
    }
}
