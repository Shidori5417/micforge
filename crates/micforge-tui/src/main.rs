mod app;
mod config;
mod graph;
mod history;
mod i18n;
mod preset;
mod theme;
mod ui;

use std::io::stdout;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use micforge_audio::AudioEngine;
use micforge_dsp::params::{ParamId, ParamStore};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use i18n::{l, tr};

/// Hedef ekran yenileme aralığı (~30 FPS).
const FRAME: Duration = Duration::from_millis(33);

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture);
}

/// Aynı anda tek MicForge: ikinci örnek aynı adla ikinci bir sanal mikrofon açardı.
/// Kilit süreç bitince (çökme dahil) çekirdek tarafından bırakılır.
fn single_instance_lock() -> Result<std::fs::File, String> {
    let path = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => std::path::PathBuf::from(d).join("micforge.lock"),
        None => {
            #[cfg(unix)]
            let name = {
                use std::os::unix::fs::MetadataExt;
                match std::fs::metadata("/proc/self") {
                    Ok(m) => format!("micforge-{}.lock", m.uid()),
                    Err(_) => "micforge.lock".to_string(),
                }
            };
            #[cfg(not(unix))]
            let name = "micforge.lock";
            std::env::temp_dir().join(name)
        }
    };
    let file = std::fs::File::create(&path).map_err(|e| {
        tr!(
            format!("Kilit dosyası açılamadı ({}): {e}", path.display()),
            format!("Could not open lock file ({}): {e}", path.display())
        )
    })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(l(
            "MicForge zaten çalışıyor (başka bir terminalde açık olabilir).",
            "MicForge is already running (it may be open in another terminal).",
        )
        .into()),
        Err(std::fs::TryLockError::Error(e)) => Err(tr!(
            format!("Kilit alınamadı: {e}"),
            format!("Could not acquire lock: {e}")
        )),
    }
}

fn handle_cli_args() {
    let Some(arg) = std::env::args().nth(1) else {
        return;
    };
    match arg.as_str() {
        "-h" | "--help" => {
            println!("micforge {}", env!("CARGO_PKG_VERSION"));
            println!(
                "{}",
                l(
                    "PipeWire için gerçek zamanlı mikrofon işleme TUI arayüzü.\n\nKullanım: micforge [SEÇENEK]\n\nSeçenekler:\n  -h, --help     Yardımı göster ve çık\n  -V, --version  Sürümü göster ve çık",
                    "Real-time microphone processing TUI for PipeWire.\n\nUsage: micforge [OPTION]\n\nOptions:\n  -h, --help     Show help and exit\n  -V, --version  Show version and exit"
                )
            );
            std::process::exit(0);
        }
        "-V" | "--version" => {
            println!("micforge {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        other => {
            eprintln!(
                "micforge: {}",
                tr!(
                    format!("bilinmeyen seçenek: {other} (yardım için --help)"),
                    format!("unknown option: {other} (see --help)")
                )
            );
            std::process::exit(2);
        }
    }
}

fn main() {
    // Config okunmadan önceki iletiler (kilit, bozuk config uyarısı) sistem dilinde.
    i18n::set_en(i18n::resolve(None));
    handle_cli_args();
    let _lock = match single_instance_lock() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("micforge: {e}");
            std::process::exit(1);
        }
    };
    let (config, warning) = config::load();

    // Kayıtlı parametreleri yükle (bilinmeyen anahtarlar yok sayılır, değerler sıkıştırılır).
    let params = Arc::new(ParamStore::default());
    for (key, v) in &config.params {
        if let Some(id) = ParamId::from_key(key) {
            params.set(id, *v);
        }
    }

    // Monitor her açılışta kapalı başlar (hoparlörde ani geri besleme olmasın).
    params.set(ParamId::MonitorOn, 0.0);

    // Kayıtlı modül sırası (geçersizse varsayılan).
    if let Some(o) = config.chain_order.as_deref().and_then(app::order_from_keys) {
        params.set_order(&o);
    }

    // Ses motoru terminal ayarlanmadan önce başlar: PipeWire yoksa düz hata yazılır.
    // (Motor hataları şimdilik yalnızca Türkçe.)
    i18n::set_en(i18n::resolve(config.language.as_deref()));
    let engine = match AudioEngine::start(
        config.last_device.clone(),
        params.clone(),
        config.out_channels(),
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("micforge: {e}");
            std::process::exit(1);
        }
    };

    // SIGTERM / SIGINT / SIGHUP: döngüden temiz çık (sanal mikrofon kaldırılır).
    // Terminal kapanınca (SIGHUP) crossterm okuması bloklanıp/dönüp bayrağı hiç
    // görmeyebilir; bu yüzden bekçi thread süre dolunca süreci sonlandırır.
    // Süreç bitince PipeWire sanal mikrofonu kendiliğinden kaldırır.
    let term = Arc::new(AtomicBool::new(false));
    match signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ]) {
        Ok(mut signals) => {
            let term = term.clone();
            std::thread::spawn(move || {
                if signals.forever().next().is_some() {
                    term.store(true, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(1500));
                    restore_terminal();
                    std::process::exit(130);
                }
            });
        }
        Err(e) => eprintln!(
            "micforge: {}",
            tr!(
                format!("sinyal işleyici kurulamadı: {e}"),
                format!("could not install signal handler: {e}")
            )
        ),
    }

    // Panic'te terminali geri yükle. Süreç bitince PipeWire istemcinin
    // düğümlerini (sanal mikrofon dahil) kendiliğinden kaldırır.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    if let Err(e) = run(engine, params, config, warning, &term) {
        restore_terminal();
        eprintln!("micforge: {e}");
        std::process::exit(1);
    }
}

fn run(
    engine: AudioEngine,
    params: Arc<ParamStore>,
    config: config::Config,
    warning: Option<String>,
    term: &AtomicBool,
) -> anyhow::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    let mut app = app::App::new(engine, params, config, warning);
    app.init_profiles();
    app.init_presets();

    let mut next_frame = Instant::now();
    while !app.quit && !term.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= next_frame {
            app.tick();
            terminal.draw(|f| ui::draw(f, &mut app))?;
            next_frame = now + FRAME;
        }
        let timeout = next_frame.saturating_duration_since(Instant::now());
        if event::poll(timeout)? {
            app.handle(event::read()?);
            // Girdiden sonra hemen yeniden çiz.
            next_frame = Instant::now();
        }
    }

    restore_terminal();
    app.save_config();
    // `app` düşünce AudioEngine kapanır ve sanal mikrofon kaldırılır.
    drop(app);
    Ok(())
}
