//! Preset dosyaları: `~/.config/micforge/presets/*.toml`.
//!
//! Bir preset yalnızca varsayılandan farklı parametreleri taşır; yüklerken
//! önce her şey varsayılana döner, sonra preset değerleri uygulanır
//! (`Preset::resolved_values`). Bozuk dosya uygulamayı çökertmez: listede
//! atlanır ve uyarı olarak bildirilir.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use micforge_dsp::params::{ParamId, SPECS};
use serde::{Deserialize, Serialize};

use crate::i18n::{l, tr};

/// Zincirde yeri değiştirilebilen modüllerin anahtarları (varsayılan işleme sırası).
pub use micforge_dsp::chain::MODULE_KEYS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Modül sırası; yoksa varsayılan sıra kullanılır.
    #[serde(default)]
    pub chain_order: Option<Vec<String>>,
    /// Parametre anahtarı → değer. Yalnızca varsayılandan farklılar bulunur.
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
}

impl Preset {
    /// Doğrular; asla başarısız olmaz. Bilinmeyen anahtarlar düşürülür, değerler
    /// aralığa sıkıştırılır, geçersiz `chain_order` `None` yapılır. Her değişiklik
    /// için bir uyarı döner.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut clean = BTreeMap::new();
        for (key, &value) in &self.params {
            let Some(id) = ParamId::from_key(key) else {
                warnings.push(tr!(
                    format!("Bilinmeyen parametre atlandı: {key}"),
                    format!("Unknown parameter skipped: {key}")
                ));
                continue;
            };
            let fixed = id.spec().clamp(value);
            if value.is_nan() || (fixed - value).abs() > 1e-6 {
                warnings.push(tr!(
                    format!("{key}: {value} geçersiz, {fixed} yapıldı"),
                    format!("{key}: {value} is invalid, set to {fixed}")
                ));
            }
            clean.insert(key.clone(), fixed);
        }
        self.params = clean;

        // Eski sürümlerde olmayan modüller (ör. efektler) varsayılan sırayla eklenir.
        if let Some(order) = &mut self.chain_order {
            let known_unique = order.iter().all(|k| MODULE_KEYS.contains(&k.as_str()))
                && order
                    .iter()
                    .enumerate()
                    .all(|(i, k)| !order[..i].contains(k));
            if known_unique {
                for k in MODULE_KEYS {
                    if !order.iter().any(|o| o == k) {
                        order.push(k.to_string());
                    }
                }
            }
        }
        if let Some(order) = &self.chain_order {
            let mut sorted: Vec<&str> = order.iter().map(String::as_str).collect();
            sorted.sort_unstable();
            let mut expected = MODULE_KEYS;
            expected.sort_unstable();
            if sorted != expected {
                warnings.push(
                    l(
                        "chain_order geçersiz (tüm modülleri tam bir kez içermeli); \
                         varsayılan sıra kullanılacak",
                        "chain_order is invalid (it must contain every module exactly once); \
                         the default order will be used",
                    )
                    .to_string(),
                );
                self.chain_order = None;
            }
        }
        warnings
    }

    /// Her parametre için tam değer listesi: varsayılan, preset değeriyle ezilmiş.
    pub fn resolved_values(&self) -> Vec<(ParamId, f32)> {
        SPECS
            .iter()
            .filter(|spec| spec.id.is_sound())
            .map(|spec| {
                let v = self
                    .params
                    .get(spec.key.as_str())
                    .map_or(spec.default, |&v| spec.clamp(v));
                (spec.id, v)
            })
            .collect()
    }

    /// Mevcut değerlerden preset üretir; yalnızca varsayılandan farklı anahtarları saklar.
    pub fn from_values(
        name: &str,
        description: &str,
        values: &[(ParamId, f32)],
        chain_order: Option<Vec<String>>,
    ) -> Preset {
        let mut params = BTreeMap::new();
        for &(id, v) in values {
            let spec = id.spec();
            let v = spec.clamp(v);
            if (v - spec.default).abs() > 1e-6 {
                params.insert(spec.key.clone(), v);
            }
        }
        Preset {
            name: name.to_string(),
            description: description.to_string(),
            chain_order,
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PresetEntry {
    pub name: String,
    pub path: PathBuf,
}

/// Küçük harfe çevirip Türkçe harfleri ASCII karşılığına indirger (sıralama anahtarı).
fn fold_lower(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'ç' | 'Ç' => 'c',
            'ğ' | 'Ğ' => 'g',
            'ı' | 'İ' => 'i',
            'ö' | 'Ö' => 'o',
            'ş' | 'Ş' => 's',
            'ü' | 'Ü' => 'u',
            c => c.to_lowercase().next().unwrap_or(c),
        })
        .collect()
}

/// ASCII dosya adı: küçük harf, Türkçe harfler katlanır, diğer her şey `-`
/// (ardışıklar tek, baş/son kırpılır); boşsa `"preset"`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in fold_lower(name).chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "preset".to_string()
    } else {
        out.to_string()
    }
}

fn empty_name() -> String {
    l("Preset adı boş olamaz", "Preset name cannot be empty").to_string()
}

/// `~` ile başlayan yolu ev dizinine açar.
fn expand_tilde(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// TOML hatasının yalnızca ilk satırı.
fn short_err(e: impl std::fmt::Display) -> String {
    e.to_string().lines().next().unwrap_or_default().to_string()
}

fn to_toml(preset: &Preset) -> Result<String, String> {
    toml::to_string_pretty(preset).map_err(|e| {
        tr!(
            format!("Preset yazılamadı: {}", short_err(e)),
            format!("Could not write preset: {}", short_err(e))
        )
    })
}

/// Yarım dosya kalmasın diye önce geçici dosyaya yazar, sonra yeniden adlandırır.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("toml.tmp");
    let fail = |e: std::io::Error| {
        tr!(
            format!("{} yazılamadı: {e}", path.display()),
            format!("Could not write {}: {e}", path.display())
        )
    };
    std::fs::write(&tmp, text).map_err(fail)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })
}

/// Dosyayı okur, ayrıştırır, doğrular. Doğrulama uyarıları da döner.
fn read_preset(path: &Path) -> Result<(Preset, Vec<String>), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| tr!(format!("okunamadı: {e}"), format!("could not read: {e}")))?;
    let mut preset: Preset = toml::from_str(&text).map_err(|e| {
        tr!(
            format!("geçerli bir preset değil: {}", short_err(e)),
            format!("not a valid preset: {}", short_err(e))
        )
    })?;
    let warnings = preset.sanitize();
    Ok((preset, warnings))
}

pub struct PresetStore {
    dir: PathBuf,
}

impl PresetStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `<config_dir>/micforge/presets`
    pub fn default_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("micforge").join("presets"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.toml", slug(name)))
    }

    /// Yolun gerçekten bu dizinin içindeki bir dosya olduğunu doğrular
    /// (`..` ve dışarıya giden sembolik bağlar reddedilir); gerçek yolu döner.
    fn inside(&self, path: &Path) -> Result<PathBuf, String> {
        let real = std::fs::canonicalize(path).map_err(|e| {
            tr!(
                format!("{} bulunamadı: {e}", path.display()),
                format!("{} not found: {e}", path.display())
            )
        })?;
        let dir = std::fs::canonicalize(&self.dir).map_err(|e| {
            tr!(
                format!("Preset dizini bulunamadı: {e}"),
                format!("Preset directory not found: {e}")
            )
        })?;
        if real.parent() == Some(dir.as_path()) && real.is_file() {
            Ok(real)
        } else {
            Err(tr!(
                format!(
                    "{} preset dizininin dışında; işlem reddedildi",
                    path.display()
                ),
                format!(
                    "{} is outside the preset directory; operation refused",
                    path.display()
                )
            ))
        }
    }

    fn create_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| {
            tr!(
                format!("Preset dizini oluşturulamadı: {e}"),
                format!("Could not create preset directory: {e}")
            )
        })
    }

    /// Bu ada sahip bir preset zaten var mı (dosya adı veya görünen ad).
    pub fn name_taken(&self, name: &str) -> bool {
        if self.path_for(name).exists() {
            return true;
        }
        let key = fold_lower(name);
        self.list().0.iter().any(|e| fold_lower(&e.name) == key)
    }

    /// `"<ad> (kopya)"`, `"<ad> (kopya 2)"`, ... biçiminde boş olan ilk ad.
    fn unique_copy_name(&self, name: &str) -> String {
        let mut n = 1u32;
        loop {
            let word = l("kopya", "copy");
            let candidate = if n == 1 {
                format!("{name} ({word})")
            } else {
                format!("{name} ({word} {n})")
            };
            if !self.name_taken(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Henüz dosyası olmayan yerleşik presetleri yazar; yazılan sayısını döner.
    pub fn install_builtins(&self) -> Result<usize, String> {
        self.create_dir()?;
        let mut written = 0;
        for preset in builtin_presets() {
            let path = self.path_for(&preset.name);
            if path.exists() {
                continue;
            }
            write_atomic(&path, &to_toml(&preset)?)?;
            written += 1;
        }
        Ok(written)
    }

    /// Tüm `*.toml` presetleri ada göre sıralı (büyük/küçük harf ve Türkçe harf
    /// duyarsız) döner. Bozuk dosyalar atlanır; `"dosya.toml: sebep"` uyarısı eklenir.
    pub fn list(&self) -> (Vec<PresetEntry>, Vec<String>) {
        let mut entries = Vec::new();
        let mut warnings = Vec::new();
        let Ok(read_dir) = std::fs::read_dir(&self.dir) else {
            return (entries, warnings);
        };
        for item in read_dir.flatten() {
            let path = item.path();
            let is_toml = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("toml"));
            if !is_toml || !path.is_file() {
                continue;
            }
            let file = item.file_name().to_string_lossy().into_owned();
            match read_preset(&path) {
                Ok((preset, _)) => entries.push(PresetEntry {
                    name: preset.name,
                    path,
                }),
                Err(reason) => warnings.push(format!("{file}: {reason}")),
            }
        }
        entries.sort_by_cached_key(|e| (fold_lower(&e.name), e.name.clone()));
        warnings.sort();
        (entries, warnings)
    }

    /// Presetı okur ve doğrular. Doğrulama uyarıları yok sayılır (bilinmeyen
    /// anahtarlar düşer, değerler sıkıştırılır); yalnızca okuma/ayrıştırma hatası döner.
    pub fn load(&self, path: &Path) -> Result<Preset, String> {
        read_preset(path)
            .map(|(p, _)| p)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Atomik kaydeder; dosya adı `slug(ad).toml`, aynı slug'lı preset ezilir.
    pub fn save(&self, preset: &Preset) -> Result<PathBuf, String> {
        let mut preset = preset.clone();
        preset.name = preset.name.trim().to_string();
        if preset.name.is_empty() {
            return Err(empty_name());
        }
        preset.sanitize();
        self.create_dir()?;
        let path = self.path_for(&preset.name);
        write_atomic(&path, &to_toml(&preset)?)?;
        Ok(path)
    }

    /// Yalnızca preset dizinindeki bir dosyayı siler.
    pub fn delete(&self, path: &Path) -> Result<(), String> {
        let real = self.inside(path)?;
        std::fs::remove_file(&real).map_err(|e| {
            tr!(
                format!("{} silinemedi: {e}", path.display()),
                format!("Could not delete {}: {e}", path.display())
            )
        })
    }

    /// Kopyasını `"<ad> (kopya)"`, `"<ad> (kopya 2)"`, ... adıyla kaydeder.
    pub fn duplicate(&self, path: &Path) -> Result<PathBuf, String> {
        let real = self.inside(path)?;
        let mut preset = self.load(&real)?;
        preset.name = self.unique_copy_name(&preset.name);
        self.save(&preset)
    }

    /// Dışarıdan bir `.toml` presetı içe aktarır. Ad çakışırsa kopya gibi
    /// benzersizleştirilir.
    pub fn import(&self, src: &Path) -> Result<PathBuf, String> {
        let src = expand_tilde(src);
        let (mut preset, _) = read_preset(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        preset.name = preset.name.trim().to_string();
        if preset.name.is_empty() {
            return Err(empty_name());
        }
        if self.name_taken(&preset.name) {
            preset.name = self.unique_copy_name(&preset.name);
        }
        self.save(&preset)
    }

    /// Kayıtlı presetı `dest`'e yazar. `dest` var olan bir dizinse içine
    /// `<slug>.toml` yazılır. `overwrite` yoksa var olan dosya ezilmez.
    pub fn export(&self, path: &Path, dest: &Path, overwrite: bool) -> Result<PathBuf, String> {
        let real = self.inside(path)?;
        let preset = self.load(&real)?;
        let mut target = expand_tilde(dest);
        if target.is_dir() {
            target = target.join(format!("{}.toml", slug(&preset.name)));
        }
        if target.exists() && !overwrite {
            return Err(tr!(
                format!(
                    "{} zaten var; üzerine yazmak için onay gerekli",
                    target.display()
                ),
                format!(
                    "{} already exists; confirmation is required to overwrite",
                    target.display()
                )
            ));
        }
        write_atomic(&target, &to_toml(&preset)?)?;
        Ok(target)
    }
}

/// Yerleşik presetlerin İngilizce açıklaması: açıklama dosyada Türkçe
/// yazılıdır; kullanıcı değiştirmediyse İngilizce arayüzde bu gösterilir.
pub fn localized_description(desc: &str) -> String {
    if !crate::i18n::en() {
        return desc.to_string();
    }
    builtin_presets()
        .iter()
        .zip(BUILTIN_DESCRIPTIONS_EN)
        .find(|(p, _)| p.description == desc)
        .map_or_else(|| desc.to_string(), |(_, en)| en.to_string())
}

/// `builtin_presets()` ile aynı sırada İngilizce açıklamalar.
const BUILTIN_DESCRIPTIONS_EN: [&str; 6] = [
    "Warm, clear and balanced broadcast sound: light gate, 3:1 compression, de-esser.",
    "For chat: RNNoise noise suppression, light compression and limiter.",
    "For noisy rooms: full RNNoise, tight gate, firm compression.",
    "Bass-heavy radio voice: bass boost, low shelf, presence and heavy compression.",
    "For clear speech: RNNoise, 120 Hz high-pass, 8 kHz low-pass, light compression.",
    "No processing: all modules and the limiter are off; audio passes through unchanged.",
];

fn builtin(name: &str, description: &str, params: &[(&str, f32)]) -> Preset {
    Preset {
        name: name.to_string(),
        description: description.to_string(),
        chain_order: None,
        params: params.iter().map(|&(k, v)| (k.to_string(), v)).collect(),
    }
}

/// Yerleşik 6 preset. EQ bantlarında tip indeksi: 0 Peak, 1 Low shelf,
/// 2 High shelf, 3 Low-pass, 4 High-pass, 5 Notch, 6 Band-pass.
pub fn builtin_presets() -> Vec<Preset> {
    vec![
        builtin(
            "Yayın/Podcast",
            "Sıcak, net ve dengeli yayın sesi: hafif gate, 3:1 sıkıştırma, de-esser.",
            &[
                ("hpf_on", 1.0),
                ("hpf_freq", 80.0),
                ("gate_on", 1.0),
                ("gate_threshold", -55.0),
                ("gate_range", -25.0),
                ("exp_on", 1.0),
                ("exp_threshold", -45.0),
                ("exp_ratio", 1.5),
                ("exp_range", -12.0),
                // Alt-orta çamurluğu kıs, 4 kHz civarında hafif presence ekle.
                ("eq1_type", 0.0),
                ("eq1_freq", 300.0),
                ("eq1_gain", -3.0),
                ("eq1_q", 1.0),
                ("eq2_type", 0.0),
                ("eq2_freq", 4000.0),
                ("eq2_gain", 2.5),
                ("eq2_q", 0.9),
                ("comp_on", 1.0),
                ("comp_threshold", -20.0),
                ("comp_ratio", 3.0),
                ("comp_knee", 6.0),
                ("comp_attack", 10.0),
                ("comp_release", 120.0),
                ("comp_makeup", 4.0),
                ("deess_on", 1.0),
                ("deess_freq", 6500.0),
                ("deess_threshold", -32.0),
                ("deess_range", 8.0),
                ("lim_ceiling", -1.0),
            ],
        ),
        builtin(
            "Discord",
            "Sohbet için: RNNoise gürültü bastırma, hafif sıkıştırma ve limiter.",
            &[
                ("rnnoise_on", 1.0),
                ("rnnoise_amount", 85.0),
                ("hpf_on", 1.0),
                ("hpf_freq", 100.0),
                ("comp_on", 1.0),
                ("comp_threshold", -22.0),
                ("comp_ratio", 2.5),
                ("comp_makeup", 3.0),
                ("lim_ceiling", -1.5),
            ],
        ),
        builtin(
            "Oyun",
            "Gürültülü ortam için: tam RNNoise, sıkı gate, belirgin sıkıştırma.",
            &[
                ("rnnoise_on", 1.0),
                ("rnnoise_amount", 100.0),
                ("hpf_on", 1.0),
                ("hpf_freq", 120.0),
                ("gate_on", 1.0),
                ("gate_threshold", -45.0),
                ("gate_range", -50.0),
                ("gate_hold", 80.0),
                ("gate_release", 120.0),
                ("comp_on", 1.0),
                ("comp_threshold", -20.0),
                ("comp_ratio", 3.5),
                ("comp_makeup", 4.0),
                ("lim_ceiling", -1.0),
            ],
        ),
        builtin(
            "Derin Radyo Sesi",
            "Bas ağırlıklı radyo sesi: bass boost, alçak raf, presence ve ağır sıkıştırma.",
            &[
                ("hpf_on", 1.0),
                ("hpf_freq", 60.0),
                ("gate_on", 1.0),
                ("gate_threshold", -55.0),
                ("gate_range", -25.0),
                ("bass", 2.0),
                ("bass_boost_on", 1.0),
                ("bass_boost_freq", 100.0),
                ("bass_boost_amount", 6.0),
                // Alçak raf ile sıcaklık, 3,5 kHz'de presence, 250 Hz'de hafif kısma.
                ("eq1_type", 1.0),
                ("eq1_freq", 120.0),
                ("eq1_gain", 3.0),
                ("eq2_type", 0.0),
                ("eq2_freq", 250.0),
                ("eq2_gain", -2.0),
                ("eq2_q", 1.2),
                ("eq3_type", 0.0),
                ("eq3_freq", 3500.0),
                ("eq3_gain", 2.0),
                ("eq3_q", 1.0),
                ("comp_on", 1.0),
                ("comp_threshold", -24.0),
                ("comp_ratio", 5.0),
                ("comp_knee", 8.0),
                ("comp_attack", 8.0),
                ("comp_release", 150.0),
                ("comp_makeup", 6.0),
                ("deess_on", 1.0),
                ("deess_threshold", -34.0),
                ("deess_range", 10.0),
                ("lim_ceiling", -1.0),
            ],
        ),
        builtin(
            "Toplantı",
            "Net konuşma için: RNNoise, 120 Hz high-pass, 8 kHz low-pass, hafif sıkıştırma.",
            &[
                ("rnnoise_on", 1.0),
                ("rnnoise_amount", 90.0),
                ("hpf_on", 1.0),
                ("hpf_freq", 120.0),
                // Ses bandını sınırla: 8 kHz üstünü yumuşakça kes.
                ("eq1_type", 3.0),
                ("eq1_freq", 8000.0),
                ("eq1_q", 0.71),
                ("eq1_slope", 0.0),
                ("comp_on", 1.0),
                ("comp_threshold", -22.0),
                ("comp_ratio", 2.0),
                ("comp_makeup", 2.0),
                ("lim_ceiling", -1.5),
            ],
        ),
        builtin(
            "Ham",
            "Hiç işleme yok: tüm modüller ve limiter kapalı, ses olduğu gibi geçer.",
            &[("tone_on", 0.0), ("eq_on", 0.0), ("lim_on", 0.0)],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use micforge_dsp::chain::Chain;
    use micforge_dsp::params::ParamStore;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test sonunda silinen benzersiz geçici dizin.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "micforge-preset-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn store(&self) -> PresetStore {
            PresetStore::new(self.0.join("presets"))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn simple(name: &str) -> Preset {
        builtin(name, "deneme", &[("input_gain", 3.0), ("comp_on", 1.0)])
    }

    #[test]
    fn slug_cases() {
        assert_eq!(slug("Yayın/Podcast"), "yayin-podcast");
        assert_eq!(slug("Derin Radyo Sesi"), "derin-radyo-sesi");
        assert_eq!(slug("Toplantı"), "toplanti");
        assert_eq!(slug(""), "preset");
        assert_eq!(slug("  ??  "), "preset");
        assert_eq!(slug("ÇĞİÖŞÜ çğıöşü"), "cgiosu-cgiosu");
        assert_eq!(slug("Oyun (kopya 2)"), "oyun-kopya-2");
    }

    #[test]
    fn builtins_are_valid() {
        let list = builtin_presets();
        let names: Vec<&str> = list.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Yayın/Podcast",
                "Discord",
                "Oyun",
                "Derin Radyo Sesi",
                "Toplantı",
                "Ham"
            ]
        );
        for mut p in list {
            assert!(!p.description.is_empty(), "{}", p.name);
            let before = p.clone();
            let warnings = p.sanitize();
            assert!(warnings.is_empty(), "{}: {warnings:?}", p.name);
            assert_eq!(p, before, "{} sanitize ile değişmemeli", p.name);
        }
    }

    #[test]
    fn builtin_descriptions_have_english() {
        assert_eq!(builtin_presets().len(), BUILTIN_DESCRIPTIONS_EN.len());
        assert!(BUILTIN_DESCRIPTIONS_EN.iter().all(|d| !d.is_empty()));
        // Türkçe arayüzde ve bilinmeyen açıklamada metin değişmez.
        assert_eq!(localized_description("kendi açıklamam"), "kendi açıklamam");
        let first = builtin_presets().remove(0).description;
        assert_eq!(localized_description(&first), first);
    }

    #[test]
    fn ham_is_transparent() {
        let ham = builtin_presets().pop().unwrap();
        assert_eq!(ham.name, "Ham");
        let store = ParamStore::default();
        for (id, v) in ham.resolved_values() {
            store.set(id, v);
        }
        let mut chain = Chain::new(48_000.0, &store);
        assert_eq!(chain.latency_samples(), 0);
        let input: Vec<f32> = (0..9600)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin())
            .collect();
        let mut out = input.clone();
        for block in out.chunks_mut(480) {
            chain.update(&store);
            chain.process(block);
        }
        for (i, (a, b)) in input.iter().zip(&out).enumerate() {
            assert!((a - b).abs() < 1e-5, "örnek {i}: {a} != {b}");
        }
    }

    #[test]
    fn resolved_and_from_values() {
        let p = simple("X");
        let values = p.resolved_values();
        // Oturum ayarları (monitor) presete girmez.
        assert_eq!(values.len(), ParamId::sound().count());
        assert!(values.iter().all(|(id, _)| id.is_sound()));
        let get = |id| values.iter().find(|(i, _)| *i == id).unwrap().1;
        assert_eq!(get(ParamId::InputGain), 3.0);
        assert_eq!(get(ParamId::CompOn), 1.0);
        assert_eq!(get(ParamId::HpfFreq), 80.0); // varsayılan

        // Tam liste → yalnızca farklı olanlar saklanır.
        let back = Preset::from_values("X", "d", &values, None);
        assert_eq!(back.params.len(), 2);
        assert_eq!(back.params["input_gain"], 3.0);
        assert_eq!(back.params["comp_on"], 1.0);
        assert_eq!(back.description, "d");

        let eps = Preset::from_values(
            "Y",
            "",
            &[(ParamId::Bass, 1e-7), (ParamId::Mid, 2.0)],
            Some(MODULE_KEYS.iter().map(|s| s.to_string()).collect()),
        );
        assert_eq!(eps.params.len(), 1);
        assert_eq!(eps.params["mid"], 2.0);
        assert!(eps.chain_order.is_some());
    }

    #[test]
    fn sanitize_fixes_everything() {
        let mut p = Preset {
            name: "S".into(),
            description: String::new(),
            chain_order: Some(vec!["hpf".into(), "hpf".into()]),
            params: BTreeMap::from([
                ("input_gain".to_string(), 99.0),
                ("nonexistent".to_string(), 1.0),
                ("hpf_on".to_string(), 0.8),
                ("bass".to_string(), 2.5),
            ]),
        };
        let warnings = p.sanitize();
        assert_eq!(warnings.len(), 4, "{warnings:?}"); // bilinmeyen + 2 sıkıştırma + sıra
        assert!(!p.params.contains_key("nonexistent"));
        assert_eq!(p.params["input_gain"], 30.0);
        assert_eq!(p.params["hpf_on"], 1.0);
        assert_eq!(p.params["bass"], 2.5);
        assert_eq!(p.chain_order, None);

        // Geçerli permütasyon korunur.
        let mut order: Vec<String> = MODULE_KEYS.iter().rev().map(|s| s.to_string()).collect();
        let mut q = simple("Q");
        q.chain_order = Some(order.clone());
        assert!(q.sanitize().is_empty());
        assert_eq!(q.chain_order, Some(order.clone()));

        // Bilinmeyen modül adı da geçersiz.
        order[0] = "bilinmeyen".into();
        q.chain_order = Some(order);
        assert_eq!(q.sanitize().len(), 1);
        assert_eq!(q.chain_order, None);
    }

    #[test]
    fn install_builtins_twice() {
        let t = TempDir::new();
        let s = t.store();
        assert_eq!(s.install_builtins().unwrap(), 6);
        assert_eq!(s.install_builtins().unwrap(), 0);
        let (list, warnings) = s.list();
        assert_eq!(list.len(), 6);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(s.dir().join("yayin-podcast.toml").is_file());
        assert!(s.dir().join("derin-radyo-sesi.toml").is_file());
        // Silinen yerleşik yeniden yazılır, mevcutlar dokunulmaz.
        std::fs::remove_file(s.dir().join("ham.toml")).unwrap();
        assert_eq!(s.install_builtins().unwrap(), 1);
    }

    #[test]
    fn builtins_roundtrip_through_files() {
        let t = TempDir::new();
        let s = t.store();
        s.install_builtins().unwrap();
        for expected in builtin_presets() {
            let path = s.dir().join(format!("{}.toml", slug(&expected.name)));
            assert_eq!(s.load(&path).unwrap(), expected);
        }
    }

    #[test]
    fn save_list_load_roundtrip() {
        let t = TempDir::new();
        let s = t.store();
        let mut p = simple("Çalışma Odası");
        p.chain_order = Some(MODULE_KEYS.iter().rev().map(|s| s.to_string()).collect());
        p.params.insert("eq1_q".into(), 0.71);
        let path = s.save(&p).unwrap();
        assert_eq!(path, s.dir().join("calisma-odasi.toml"));
        assert!(!s.dir().join("calisma-odasi.toml.tmp").exists());
        assert_eq!(s.load(&path).unwrap(), p);

        // Aynı slug ezilir.
        let mut p2 = simple("calisma odasi");
        p2.params.clear();
        assert_eq!(s.save(&p2).unwrap(), path);
        assert_eq!(s.list().0.len(), 1);

        // Sıralama: büyük/küçük harf ve Türkçe harf duyarsız.
        s.save(&simple("zeta")).unwrap();
        s.save(&simple("Alfa")).unwrap();
        s.save(&simple("Çay")).unwrap();
        let names: Vec<String> = s.list().0.into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Alfa", "calisma odasi", "Çay", "zeta"]);

        assert!(s.save(&simple("   ")).is_err());
    }

    #[test]
    fn list_skips_corrupt_files() {
        let t = TempDir::new();
        let s = t.store();
        s.save(&simple("İyi")).unwrap();
        std::fs::write(s.dir().join("bozuk.toml"), "name = [[[ bu toml degil").unwrap();
        std::fs::write(s.dir().join("adsiz.toml"), "description = \"ad yok\"").unwrap();
        std::fs::write(s.dir().join("not.txt"), "yok sayılır").unwrap();
        let (list, warnings) = s.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "İyi");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.starts_with("bozuk.toml:")));
        assert!(warnings.iter().any(|w| w.starts_with("adsiz.toml:")));
        assert!(s.load(&s.dir().join("bozuk.toml")).is_err());

        // Dizin yoksa boş döner.
        let (none, w) = PresetStore::new(t.0.join("yok")).list();
        assert!(none.is_empty() && w.is_empty());
    }

    #[test]
    fn load_sanitizes() {
        let t = TempDir::new();
        let s = t.store();
        std::fs::create_dir_all(s.dir()).unwrap();
        let path = s.dir().join("x.toml");
        std::fs::write(
            &path,
            "name = \"X\"\nchain_order = [\"hpf\"]\n[params]\ninput_gain = 100.0\nfoo = 1.0\n",
        )
        .unwrap();
        let p = s.load(&path).unwrap();
        assert_eq!(p.params.len(), 1);
        assert_eq!(p.params["input_gain"], 30.0);
        // Eksik modüller (eski sürüm presetleri) varsayılan sırayla eklenir.
        let order = p.chain_order.unwrap();
        assert_eq!(order.len(), MODULE_KEYS.len());
        assert_eq!(order[0], "hpf");
        assert_eq!(p.description, "");
    }

    #[test]
    fn duplicate_names() {
        let t = TempDir::new();
        let s = t.store();
        let orig = s.save(&simple("Discord")).unwrap();
        let c1 = s.duplicate(&orig).unwrap();
        let c2 = s.duplicate(&orig).unwrap();
        let c3 = s.duplicate(&orig).unwrap();
        assert_eq!(s.load(&c1).unwrap().name, "Discord (kopya)");
        assert_eq!(s.load(&c2).unwrap().name, "Discord (kopya 2)");
        assert_eq!(s.load(&c3).unwrap().name, "Discord (kopya 3)");
        assert_eq!(s.list().0.len(), 4);
        // Kopya içerik olarak aynıdır.
        assert_eq!(s.load(&c1).unwrap().params, simple("x").params);
        assert!(s.duplicate(&t.0.join("yok.toml")).is_err());
    }

    #[test]
    fn delete_refuses_outside_paths() {
        let t = TempDir::new();
        let s = t.store();
        let inside = s.save(&simple("A")).unwrap();
        let outside = t.0.join("disarida.toml");
        std::fs::write(&outside, "name = \"D\"").unwrap();
        assert!(s.delete(&outside).is_err());
        assert!(outside.exists());
        // `..` ile kaçış.
        let sneaky = s.dir().join("..").join("disarida.toml");
        assert!(s.delete(&sneaky).is_err());
        assert!(outside.exists());
        // Sembolik bağ dışarıyı gösteriyorsa reddedilir.
        let link = s.dir().join("link.toml");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(s.delete(&link).is_err());
        assert!(outside.exists());
        // Dizinin kendisi de silinemez.
        assert!(s.delete(s.dir()).is_err());

        s.delete(&inside).unwrap();
        assert!(!inside.exists());
        assert!(s.delete(&inside).is_err());
    }

    #[test]
    fn import_unique_and_rejects_garbage() {
        let t = TempDir::new();
        let s = t.store();
        let src = t.0.join("dis.toml");
        std::fs::write(
            &src,
            "name = \"Harici\"\n[params]\nbass = 3.0\nbilinmeyen = 1.0\n",
        )
        .unwrap();
        let p1 = s.import(&src).unwrap();
        let p2 = s.import(&src).unwrap();
        let p3 = s.import(&src).unwrap();
        let l = |p: &Path| s.load(p).unwrap();
        assert_eq!(l(&p1).name, "Harici");
        assert_eq!(l(&p2).name, "Harici (kopya)");
        assert_eq!(l(&p3).name, "Harici (kopya 2)");
        assert_eq!(l(&p1).params.len(), 1); // bilinmeyen anahtar düştü

        let garbage = t.0.join("cop.toml");
        std::fs::write(&garbage, "title = \"preset degil\"\n").unwrap();
        assert!(s.import(&garbage).is_err());
        std::fs::write(&garbage, "bu hic toml degil [[").unwrap();
        assert!(s.import(&garbage).is_err());
        assert!(s.import(&t.0.join("yok.toml")).is_err());
        assert_eq!(s.list().0.len(), 3);
    }

    #[test]
    fn export_overwrite_rules() {
        let t = TempDir::new();
        let s = t.store();
        let path = s.save(&simple("Yayın/Podcast")).unwrap();

        // Dosya olarak.
        let dest = t.0.join("cikti.toml");
        assert_eq!(s.export(&path, &dest, false).unwrap(), dest);
        assert_eq!(s.load(&dest).unwrap(), s.load(&path).unwrap());
        assert!(s.export(&path, &dest, false).is_err());
        assert!(s.export(&path, &dest, true).is_ok());

        // Dizin olarak: <slug>.toml.
        let dir = t.0.join("hedef");
        std::fs::create_dir_all(&dir).unwrap();
        let out = s.export(&path, &dir, false).unwrap();
        assert_eq!(out, dir.join("yayin-podcast.toml"));
        assert!(s.export(&path, &dir, false).is_err());

        // Depo dışındaki kaynak reddedilir.
        assert!(s.export(&dest, &t.0.join("baska.toml"), false).is_err());
    }

    #[test]
    fn tilde_expands_to_home() {
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expand_tilde(Path::new("~/a/b.toml")), home.join("a/b.toml"));
            assert_eq!(expand_tilde(Path::new("~")), home);
        }
        assert_eq!(expand_tilde(Path::new("/x/~y")), PathBuf::from("/x/~y"));
        assert_eq!(expand_tilde(Path::new("~user/x")), PathBuf::from("~user/x"));
    }
}
