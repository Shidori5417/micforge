//! `~/.config/micforge/config.toml`. Bozuk dosya uygulamayı çökertmez:
//! yedeği alınır, varsayılana dönülür ve uyarı gösterilir.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::i18n::{l, tr};

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// "Kapanınca işleme durur" notu gösterildi mi.
    pub first_run_notice_shown: bool,
    /// Son seçilen giriş cihazının `node.name` değeri.
    pub last_device: Option<String>,
    /// Son parametre değerleri (anahtar → değer). Bilinmeyen anahtarlar yok sayılır.
    pub params: BTreeMap<String, f32>,
    /// Spektral kapının öğrenilmiş gürültü profili (bin başına genlik).
    pub noise_profile: Option<Vec<f32>>,
    /// Modül işlem sırası (modül anahtarları).
    pub chain_order: Option<Vec<String>>,
    /// Son yüklenen/kaydedilen preset adı.
    pub last_preset: Option<String>,
    /// Hazır presetler preset klasörüne bir kez yazıldı mı (silinenler geri gelmesin).
    pub builtin_presets_installed: bool,
    /// Arayüz dili: "tr" veya "en". Yoksa sistem diline göre seçilir.
    pub language: Option<String>,
    /// Tema: "dark" (varsayılan) veya "light".
    pub theme: Option<String>,
    /// Sanal mikrofon kanal sayısı: 1 (mono) veya 2 (stereo, varsayılan).
    pub output_channels: Option<u32>,
    /// Özel renkler: rol → renk ("#rrggbb", "red", "lightblue", "236" …).
    /// Roller: bg, fg, dim, muted, accent, sel_bg, badge_fg, ok, warn, danger,
    /// special, spec_in, spec_out, curve, grid.
    pub theme_colors: BTreeMap<String, String>,
}

impl Config {
    pub fn light_theme(&self) -> bool {
        self.theme.as_deref() == Some("light")
    }

    pub fn out_channels(&self) -> u32 {
        if self.output_channels == Some(1) {
            1
        } else {
            2
        }
    }
}

pub fn dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("micforge"))
}

fn path() -> Option<PathBuf> {
    dir().map(|d| d.join("config.toml"))
}

/// Config'i yükler. İkinci değer kullanıcıya gösterilecek uyarıdır.
pub fn load() -> (Config, Option<String>) {
    let Some(path) = path() else {
        return (Config::default(), None);
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Config::default(), None),
        Err(e) => {
            return (
                Config::default(),
                Some(tr!(
                    format!("Config okunamadı ({e}); varsayılanlar kullanılıyor."),
                    format!("Could not read config ({e}); using defaults.")
                )),
            );
        }
    };
    match toml::from_str(&text) {
        Ok(c) => (c, None),
        Err(e) => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let backup = path.with_extension(format!("toml.bak-{stamp}"));
            let saved = std::fs::rename(&path, &backup).is_ok();
            let first = e.to_string().lines().next().unwrap_or_default().to_string();
            let msg = if saved {
                tr!(
                    format!(
                        "Config bozuk ({first}). Yedek: {}. Varsayılanlar yüklendi.",
                        backup.display()
                    ),
                    format!(
                        "Config is corrupt ({first}). Backup: {}. Defaults loaded.",
                        backup.display()
                    )
                )
            } else {
                tr!(
                    format!("Config bozuk ({first}). Varsayılanlar yüklendi."),
                    format!("Config is corrupt ({first}). Defaults loaded.")
                )
            };
            (Config::default(), Some(msg))
        }
    }
}

impl Config {
    pub fn save(&self) -> Result<(), String> {
        let path = path().ok_or_else(|| {
            l("Config dizini bulunamadı", "Config directory not found").to_string()
        })?;
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Yarım yazılmış dosya kalmasın diye önce geçici dosyaya yaz.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
}
