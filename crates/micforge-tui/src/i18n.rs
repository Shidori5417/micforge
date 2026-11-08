//! Arayüz dili: Türkçe / İngilizce.
//!
//! Metinler kaynakta iki dilde yan yana yazılır: `l("Kaydet", "Save")`.
//! Biçimli metinler için `tr!(format!(…), format!(…))` (yalnızca seçili dal
//! hesaplanır). Metinler çizim/kullanım anında aranır; statik önbellek yok.

use micforge_dsp::params::ParamSpec;

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static EN: AtomicBool = AtomicBool::new(false);

/// İngilizce mi etkin.
#[inline]
pub fn en() -> bool {
    EN.load(Relaxed)
}

pub fn set_en(v: bool) {
    EN.store(v, Relaxed);
}

/// Etkin dile göre metin.
#[inline]
pub fn l(tr: &'static str, en_text: &'static str) -> &'static str {
    if en() { en_text } else { tr }
}

/// `tr!(türkçe, english)`: etkin dile göre ifade (yalnızca seçilen dal çalışır).
macro_rules! tr {
    ($tr:expr, $en:expr) => {
        if $crate::i18n::en() { $en } else { $tr }
    };
}
pub(crate) use tr;

/// Parametrenin görünen adı.
pub fn pname(spec: &ParamSpec) -> &str {
    spec.name_lang(en())
}

/// Parametre değeri, etkin dildeki birimlerle.
pub fn pvalue(spec: &ParamSpec, v: f32) -> String {
    spec.format_lang(v, en())
}

/// Seçenek parametresinin etiketleri.
pub fn pchoices(spec: &ParamSpec) -> &'static [&'static str] {
    spec.choices_lang(en())
}

/// Zincir modülünün görünen adı.
pub fn module_name(i: usize) -> &'static str {
    use micforge_dsp::chain::{MODULE_NAMES, MODULE_NAMES_EN};
    if en() {
        MODULE_NAMES_EN[i]
    } else {
        MODULE_NAMES[i]
    }
}

/// Sistem dili Türkçe mi (`LC_ALL` > `LC_MESSAGES` > `LANG`).
pub fn system_is_turkish() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("tr"))
}

/// Config değeri ("tr"/"en"); yoksa sistem diline göre.
pub fn resolve(config: Option<&str>) -> bool {
    match config {
        Some("en") => true,
        Some("tr") => false,
        _ => !system_is_turkish(),
    }
}
