//! Renk temaları: koyu, açık ve config'deki `[theme_colors]` ile özel renkler.
//!
//! Arayüz yalnızca UI thread'inden çizildiği için etkin tema thread-local
//! tutulur; çizim kodu `th()` ile kopyasını alır.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::str::FromStr;

use ratatui::style::Color;

/// Anlamsal renkler. Çizim kodu doğrudan `Color::X` yerine bunları kullanır.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// Genel arka plan ve yazı.
    pub bg: Color,
    pub fg: Color,
    /// Soluk yazı (ipuçları, ızgara, boş metre).
    pub dim: Color,
    /// İkincil yazı (kapalı rozetler, oklar).
    pub muted: Color,
    /// Vurgu: başlık, odak çerçevesi, seçili değer.
    pub accent: Color,
    /// Seçili satır arka planı.
    pub sel_bg: Color,
    /// Renkli rozetlerin üstündeki yazı.
    pub badge_fg: Color,
    pub ok: Color,
    pub warn: Color,
    pub danger: Color,
    /// Kazanç azaltma göstergeleri.
    pub special: Color,
    /// Spektrum: giriş, çıkış; EQ eğrisi; ızgara.
    pub spec_in: Color,
    pub spec_out: Color,
    pub curve: Color,
    pub grid: Color,
}

impl Theme {
    pub const DARK: Theme = Theme {
        bg: Color::Reset,
        fg: Color::Reset,
        dim: Color::DarkGray,
        muted: Color::Gray,
        accent: Color::Cyan,
        sel_bg: Color::DarkGray,
        badge_fg: Color::Black,
        ok: Color::Green,
        warn: Color::Yellow,
        danger: Color::Red,
        special: Color::Magenta,
        spec_in: Color::LightBlue,
        spec_out: Color::Green,
        curve: Color::Yellow,
        grid: Color::DarkGray,
    };

    pub const LIGHT: Theme = Theme {
        bg: Color::Rgb(250, 250, 247),
        fg: Color::Rgb(30, 30, 30),
        dim: Color::Rgb(140, 140, 140),
        muted: Color::Rgb(90, 90, 90),
        accent: Color::Rgb(0, 105, 160),
        sel_bg: Color::Rgb(215, 220, 228),
        badge_fg: Color::Rgb(255, 255, 255),
        ok: Color::Rgb(20, 135, 60),
        warn: Color::Rgb(185, 120, 0),
        danger: Color::Rgb(200, 30, 30),
        special: Color::Rgb(150, 40, 150),
        spec_in: Color::Rgb(40, 100, 220),
        spec_out: Color::Rgb(20, 135, 60),
        curve: Color::Rgb(200, 110, 0),
        grid: Color::Rgb(200, 200, 200),
    };

    /// `[theme_colors]` anahtarları (config'de geçerli adlar).
    pub const KEYS: [&'static str; 15] = [
        "bg", "fg", "dim", "muted", "accent", "sel_bg", "badge_fg", "ok", "warn", "danger",
        "special", "spec_in", "spec_out", "curve", "grid",
    ];

    fn slot(&mut self, key: &str) -> Option<&mut Color> {
        Some(match key {
            "bg" => &mut self.bg,
            "fg" => &mut self.fg,
            "dim" => &mut self.dim,
            "muted" => &mut self.muted,
            "accent" => &mut self.accent,
            "sel_bg" => &mut self.sel_bg,
            "badge_fg" => &mut self.badge_fg,
            "ok" => &mut self.ok,
            "warn" => &mut self.warn,
            "danger" => &mut self.danger,
            "special" => &mut self.special,
            "spec_in" => &mut self.spec_in,
            "spec_out" => &mut self.spec_out,
            "curve" => &mut self.curve,
            "grid" => &mut self.grid,
            _ => return None,
        })
    }

    /// Temel tema + özel renkler. Geçersiz girdiler atlanır ve döndürülür.
    pub fn build(light: bool, custom: &BTreeMap<String, String>) -> (Theme, Vec<String>) {
        let mut t = if light { Self::LIGHT } else { Self::DARK };
        let mut bad = Vec::new();
        for (k, v) in custom {
            match (t.slot(k), Color::from_str(v.trim())) {
                (Some(slot), Ok(c)) => *slot = c,
                _ => bad.push(format!("{k} = \"{v}\"")),
            }
        }
        (t, bad)
    }
}

thread_local! {
    static CURRENT: Cell<Theme> = const { Cell::new(Theme::DARK) };
}

/// Etkin tema.
pub fn th() -> Theme {
    CURRENT.with(Cell::get)
}

pub fn set(t: Theme) {
    CURRENT.with(|c| c.set(t));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_colors_override_and_report_errors() {
        let custom = BTreeMap::from([
            ("accent".to_string(), "#ff8800".to_string()),
            ("ok".to_string(), "lightgreen".to_string()),
            ("nope".to_string(), "red".to_string()),
            ("warn".to_string(), "not-a-color".to_string()),
        ]);
        let (t, bad) = Theme::build(true, &custom);
        assert_eq!(t.accent, Color::Rgb(255, 136, 0));
        assert_eq!(t.ok, Color::LightGreen);
        assert_eq!(t.warn, Theme::LIGHT.warn);
        assert_eq!(bad.len(), 2);
        let mut t2 = Theme::DARK;
        for k in Theme::KEYS {
            assert!(t2.slot(k).is_some(), "{k}");
        }
    }
}
