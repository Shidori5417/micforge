use std::sync::atomic::Ordering::Relaxed;

use crate::theme::th;
use micforge_audio::{CaptureStatus, QUANTUM, SAMPLE_RATE, VIRTUAL_DESCRIPTION};
use micforge_dsp::chain::response_db;
use micforge_dsp::params::{ParamId, Scale};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, Paragraph, Wrap};

use crate::app::{
    App, Focus, METER_FLOOR_DB, Meter, PromptKind, TABS, gr_label, tab_name, tab_panels,
};
use crate::graph::{self, GraphData, SpectrumMode};
use crate::i18n::{self, l, tr};

pub fn draw(f: &mut Frame, app: &mut App) {
    let [tabs, body, status, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let t = th();
    f.render_widget(Block::new().style(Style::new().bg(t.bg).fg(t.fg)), f.area());
    draw_tabs(f, app, tabs);
    app.areas.settings_rows.clear();
    app.areas.devices = Rect::default();
    app.areas.params.clear();
    app.areas.graph = Rect::default();
    app.areas.chain_rows.clear();
    app.areas.preset_rows.clear();
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
    match app.tab {
        0 => {
            let dev_h = (app.devices.len() as u16 + 2).clamp(3, 8);
            let [dev, params] =
                Layout::vertical([Constraint::Length(dev_h), Constraint::Min(3)]).areas(left);
            let [io, mon] =
                Layout::vertical([Constraint::Length(7), Constraint::Min(4)]).areas(params);
            let [mon, _] = Layout::vertical([Constraint::Length(4), Constraint::Min(0)]).areas(mon);
            draw_devices(f, app, dev);
            draw_panel(f, app, io, 0);
            draw_panel(f, app, mon, 1);
            draw_meters(f, app, right);
        }
        1 => {
            let [top, bottom] =
                Layout::vertical([Constraint::Min(8), Constraint::Length(13)]).areas(body);
            draw_graph(f, app, top);
            let [l, r] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(bottom);
            draw_panel(f, app, l, 0);
            draw_panel(f, app, r, 1);
        }
        2 => {
            let [top, bottom] =
                Layout::vertical([Constraint::Length(7), Constraint::Min(10)]).areas(body);
            let [gr, lv] =
                Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .areas(top);
            draw_gain_reduction(f, app, gr);
            draw_levels_compact(f, app, lv);
            let [c1, c2, c3] = Layout::horizontal([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .areas(bottom);
            let [gate, exp] =
                Layout::vertical([Constraint::Length(9), Constraint::Min(3)]).areas(c1);
            let [comp, deess] =
                Layout::vertical([Constraint::Length(10), Constraint::Min(3)]).areas(c2);
            let [lim, _] = Layout::vertical([Constraint::Length(6), Constraint::Min(0)]).areas(c3);
            draw_panel(f, app, gate, 0);
            draw_panel(f, app, exp, 1);
            draw_panel(f, app, comp, 2);
            draw_panel(f, app, deess, 3);
            draw_panel(f, app, lim, 4);
        }
        4 => {
            let [c1, c2, c3] = Layout::horizontal([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .areas(body);
            let [pitch, voice, _] = Layout::vertical([
                Constraint::Length(6),
                Constraint::Length(6),
                Constraint::Min(0),
            ])
            .areas(c1);
            let [chorus, delay, _] = Layout::vertical([
                Constraint::Length(6),
                Constraint::Length(7),
                Constraint::Min(0),
            ])
            .areas(c2);
            let [reverb, info] =
                Layout::vertical([Constraint::Length(7), Constraint::Min(0)]).areas(c3);
            draw_panel(f, app, pitch, 0);
            draw_panel(f, app, voice, 1);
            draw_panel(f, app, chorus, 2);
            draw_panel(f, app, delay, 3);
            draw_panel(f, app, reverb, 4);
            let text = l(
                concat!(
                    "Efektler zincirde de-esser'dan sonra, limiter'dan önce gelir; ",
                    "sırayı Zincir sekmesinden değiştirebilirsiniz.\n\n",
                    "Pitch shift ve \"Derin ses\" ~20 ms gecikme ekler.\n",
                    "Kendi sesinizi duymak için Genel sekmesinde Monitor'ü açın ",
                    "(kulaklık kullanın).",
                ),
                concat!(
                    "Effects run after the de-esser and before the limiter in the chain; ",
                    "you can change the order in the Chain tab.\n\n",
                    "Pitch shift and \"Deep voice\" add ~20 ms of latency.\n",
                    "To hear yourself, turn on Monitor in the General tab ",
                    "(use headphones).",
                ),
            );
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .style(Style::new().fg(th().dim))
                    .block(Block::bordered().title(l(" Bilgi ", " Info "))),
                info,
            );
        }
        5 => draw_chain(f, app, body),
        6 => draw_presets(f, app, body),
        3 => {
            let [top, bottom] =
                Layout::vertical([Constraint::Length(8), Constraint::Min(6)]).areas(body);
            let [st, lv] =
                Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .areas(top);
            draw_noise_status(f, app, st);
            draw_levels_compact(f, app, lv);
            let [l, r] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(bottom);
            let [rn, _] = Layout::vertical([Constraint::Length(4), Constraint::Min(0)]).areas(l);
            let [sg, _] = Layout::vertical([Constraint::Length(6), Constraint::Min(0)]).areas(r);
            draw_panel(f, app, rn, 0);
            draw_panel(f, app, sg, 1);
        }
        _ => draw_settings(f, app, body),
    }
    draw_status(f, app, status);
    let keys = if app.tab == 7 {
        l(
            " j/k: seç  ←/→/Enter/tık: değiştir  ?: yardım  q: çık",
            " j/k: select  ←/→/Enter/click: change  ?: help  q: quit",
        )
    } else if app.tab == 6 {
        l(
            " j/k: seç  Enter/çift tık: yükle  n: yeni kaydet  s: üzerine kaydet  c: kopyala  d: sil  i: içe aktar  e: dışa aktar  u/^R: geri al/yinele",
            " j/k: select  Enter/dbl-click: load  n: save new  s: overwrite  c: duplicate  d: delete  i: import  e: export  u/^R: undo/redo",
        )
    } else if app.tab == 5 {
        l(
            " j/k: seç  J/K veya Shift+↑/↓ ya da fareyle sürükle: taşı  b/Enter: aç/kapat  r: varsayılan sıra  q: çık",
            " j/k: select  J/K, Shift+↑/↓ or drag with mouse: move  b/Enter: on/off  r: default order  q: quit",
        )
    } else if app.tab == 3 {
        l(
            " p / tık: gürültü profili öğren (3 sn sessiz kalın)  h/l: panel  j/k: seç  ←/→: ayarla  Enter: değer  b: aç/kapat  q: çık",
            " p / click: learn noise profile (stay silent 3 s)  h/l: panel  j/k: select  ←/→: adjust  Enter: value  b: on/off  q: quit",
        )
    } else if app.tab == 2 {
        l(
            " h/l: modül  j/k: seç  ←/→: ayarla (Shift: ince)  Enter: değer gir  b: modülü aç/kapat  r: varsayılan  m: sessiz  q: çık",
            " h/l: module  j/k: select  ←/→: adjust (Shift: fine)  Enter: type value  b: module on/off  r: default  m: mute  q: quit",
        )
    } else if app.tab == 1 {
        l(
            " h/l: panel  j/k: seç  ←/→: ayarla (Shift: ince)  Enter: değer  [ ]: bant  s: spektrum  b: bypass  r: sıfırla  fare: nokta sürükle, tekerlek: Q",
            " h/l: panel  j/k: select  ←/→: adjust (Shift: fine)  Enter: value  [ ]: band  s: spectrum  b: bypass  r: reset  mouse: drag point, wheel: Q",
        )
    } else if app.tab == 0 && app.focus == Focus::Devices {
        l(
            " j/k: cihaz  Enter: bağlan  d: kes  l: ayarlar  m: sessiz  Tab/1-8: sekme  q: çık",
            " j/k: device  Enter: connect  d: disconnect  l: settings  m: mute  Tab/1-8: tab  q: quit",
        )
    } else {
        l(
            " j/k: seç  ←/→: ayarla (Shift: ince)  Enter: değer gir  b: bypass  r: varsayılan  m: sessiz  Tab: sekme  q: çık",
            " j/k: select  ←/→: adjust (Shift: fine)  Enter: type value  b: bypass  r: default  m: mute  Tab: tab  q: quit",
        )
    };
    f.render_widget(Paragraph::new(keys).style(Style::new().fg(th().dim)), help);

    if app.show_help {
        draw_help(f);
    } else if let Some(msg) = app.fatal.clone().or_else(|| app.message.clone()) {
        let title = if app.fatal.is_some() {
            l(" Hata ", " Error ")
        } else {
            l(" Uyarı ", " Warning ")
        };
        popup(
            f,
            title,
            &msg,
            l("[Enter] Kapat", "[Enter] Close"),
            th().danger,
            64,
            9,
        );
    } else if let Some(p) = &app.prompt {
        let confirm_footer = l("[e/Enter] Evet  [h/Esc] Hayır", "[y/Enter] Yes  [n/Esc] No");
        let (title, body, footer) = match &p.kind {
            PromptKind::SaveAs => (
                l(" Yeni preset ", " New preset "),
                format!("{} {}_", l("Ad:", "Name:"), p.text),
                l("[Enter] Kaydet  [Esc] İptal", "[Enter] Save  [Esc] Cancel"),
            ),
            PromptKind::ConfirmSaveAs(name) => (
                l(" Üzerine yazılsın mı? ", " Overwrite? "),
                tr!(
                    format!("\"{name}\" adında bir preset zaten var. Üzerine yazılsın mı?"),
                    format!("A preset named \"{name}\" already exists. Overwrite it?")
                ),
                confirm_footer,
            ),
            PromptKind::Import => (
                l(" İçe aktar ", " Import "),
                format!(
                    "{} {}_",
                    l("Dosya yolu (.toml):", "File path (.toml):"),
                    p.text
                ),
                l("[Enter] Aktar  [Esc] İptal", "[Enter] Import  [Esc] Cancel"),
            ),
            PromptKind::Export(_) => (
                l(" Dışa aktar ", " Export "),
                format!(
                    "{} {}_",
                    l("Hedef dosya veya klasör:", "Target file or folder:"),
                    p.text
                ),
                l("[Enter] Aktar  [Esc] İptal", "[Enter] Export  [Esc] Cancel"),
            ),
            PromptKind::ConfirmOverwriteExport(_, dest) => (
                l(" Üzerine yazılsın mı? ", " Overwrite? "),
                tr!(
                    format!("{} zaten var.", dest.display()),
                    format!("{} already exists.", dest.display())
                ),
                confirm_footer,
            ),
            PromptKind::ConfirmSave(path) => (
                l(" Üzerine kaydet ", " Overwrite preset "),
                tr!(
                    format!(
                        "\"{}\" presetinin üzerine şu anki ayarlar yazılsın mı?",
                        preset_name(app, path)
                    ),
                    format!(
                        "Overwrite preset \"{}\" with the current settings?",
                        preset_name(app, path)
                    )
                ),
                confirm_footer,
            ),
            PromptKind::ConfirmDelete(path) => (
                l(" Sil ", " Delete "),
                tr!(
                    format!("\"{}\" preseti silinsin mi?", preset_name(app, path)),
                    format!("Delete preset \"{}\"?", preset_name(app, path))
                ),
                confirm_footer,
            ),
        };
        popup(f, title, &body, footer, th().accent, 64, 7);
    } else if let Some((id, text)) = &app.entry {
        let s = id.spec();
        popup(
            f,
            l(" Değer gir ", " Enter value "),
            &format!(
                "{}: {}_\n\n{}: {} … {}",
                i18n::pname(s),
                text,
                l("Aralık", "Range"),
                i18n::pvalue(s, s.min),
                i18n::pvalue(s, s.max)
            ),
            l("[Enter] Uygula  [Esc] İptal", "[Enter] Apply  [Esc] Cancel"),
            th().accent,
            48,
            7,
        );
    } else if app.show_notice {
        popup(
            f,
            l(" Bilgi ", " Info "),
            &tr!(
                format!(
                    "MicForge yalnızca açıkken ses işler.\n\n\
                     Uygulamayı kapattığınızda işleme durur ve \"{VIRTUAL_DESCRIPTION}\" sanal \
                     mikrofonu kaldırılır. Discord, OBS gibi uygulamalarda bu mikrofonu \
                     kullanırken MicForge'u açık tutun."
                ),
                format!(
                    "MicForge only processes audio while it is running.\n\n\
                     When you close the app, processing stops and the \"{VIRTUAL_DESCRIPTION}\" \
                     virtual microphone is removed. Keep MicForge open while you use this \
                     microphone in apps like Discord or OBS."
                )
            ),
            l("[Enter] Tamam", "[Enter] OK"),
            th().accent,
            64,
            11,
        );
    }
}

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let mut spans = vec![Span::raw(" MicForge ").bold().fg(th().accent)];
    let mut x = area.x + 10;
    app.areas.tabs.clear();
    for i in 0..TABS.len() {
        let label = format!(" {} {} ", i + 1, tab_name(i));
        let w = label.chars().count() as u16;
        app.areas.tabs.push(Rect::new(x, area.y, w, 1));
        x += w + 1;
        let style = if i == app.tab {
            Style::new().fg(th().badge_fg).bg(th().accent).bold()
        } else if !tab_panels(i, 0).is_empty() || i == 5 || i == 6 {
            Style::new()
        } else {
            Style::new().fg(th().dim)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    f.render_widget(Line::from(spans), area);
}

fn focus_block(title: String, focused: bool) -> Block<'static> {
    let b = Block::bordered().title(title);
    if focused {
        b.border_style(Style::new().fg(th().accent))
    } else {
        b
    }
}

fn draw_devices(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Devices;
    let active = match &app.status {
        CaptureStatus::Active(n) => Some(n.as_str()),
        _ => None,
    };
    let mut title = l(" Giriş cihazı ", " Input device ").to_string();
    if let CaptureStatus::Waiting(w) = &app.status {
        title = tr!(
            format!(" Giriş cihazı — bekleniyor: {w} "),
            format!(" Input device — waiting for: {w} ")
        );
    }
    app.areas.devices = area;
    if app.devices.is_empty() {
        f.render_widget(
            Paragraph::new(l(
                "Mikrofon bulunamadı. Bir giriş cihazı takın.",
                "No microphone found. Plug in an input device.",
            ))
            .wrap(Wrap { trim: true })
            .block(focus_block(title, focused)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = app
        .devices
        .iter()
        .map(|d| {
            let mark = if Some(d.name.as_str()) == active {
                Span::styled("● ", Style::new().fg(th().ok))
            } else {
                Span::raw("  ")
            };
            ListItem::new(Line::from(vec![mark, Span::raw(d.description.clone())]))
        })
        .collect();
    let mut list = List::new(items).block(focus_block(title, focused));
    if focused {
        list = list
            .highlight_style(Style::new().bg(th().sel_bg).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
    } else {
        list = list.highlight_symbol("  ");
    }
    f.render_stateful_widget(list, area, &mut app.list);
}

/// Sekmenin `index` numaralı parametre panelini çizer.
fn draw_panel(f: &mut Frame, app: &mut App, area: Rect, index: usize) {
    let Some(panel) = tab_panels(app.tab, app.eq_band).into_iter().nth(index) else {
        return;
    };
    draw_param_list(f, app, area, &panel.title, &panel.ids, index);
}

fn draw_param_list(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    title: &str,
    ids: &[ParamId],
    panel: usize,
) {
    let focused = app.focus == Focus::Panel(panel);
    let block = focus_block(title.to_string(), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let selected = app.selected_param();
    let buf = f.buffer_mut();
    for (i, &id) in ids.iter().enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, y, inner.width, 1);
        let bar = param_row(buf, row, app, id, selected == Some(id));
        app.areas.params.push((id, row, bar, panel, i));
    }
}

fn draw_graph(f: &mut Frame, app: &mut App, area: Rect) {
    let mode = app.spectrum_mode;
    // Sütun sayısı önceki karenin çizim alanından; ilk karede tahmin.
    let plot_guess = if app.areas_graph_last.width > 0 {
        app.areas_graph_last
    } else {
        area
    };
    let n = graph::spectrum_columns(plot_guess).max(1);
    let (vmin, vmax) = (app.view.f_min, app.view.f_max);
    if mode != SpectrumMode::Off {
        app.spec_in.resize(n, -120.0);
        app.spec_out.resize(n, -120.0);
        app.analyzer_in
            .compute(&mut app.spec_in, vmin, vmax, app.frame_dt);
        app.analyzer_out
            .compute(&mut app.spec_out, vmin, vmax, app.frame_dt);
    }
    let params = app.params.clone();
    let curve = move |freq: f32| response_db(&params, freq, SAMPLE_RATE as f32);
    let markers = app.band_markers();
    let on = mode != SpectrumMode::Off;
    let data = GraphData {
        spectrum_in: on.then_some(app.spec_in.as_slice()),
        spectrum_out: on.then_some(app.spec_out.as_slice()),
        curve: &curve,
        markers: &markers,
    };
    let mode_name = match mode {
        SpectrumMode::Overlay => l("üst üste", "overlay"),
        SpectrumMode::Split => l("ayrı", "split"),
        SpectrumMode::Off => l("kapalı", "off"),
    };
    let title = tr!(
        format!(" EQ eğrisi — spektrum: {mode_name} (s) │ mavi: giriş, yeşil: çıkış "),
        format!(" EQ curve — spectrum: {mode_name} (s) │ blue: input, green: output ")
    );
    let plot = graph::render(f.buffer_mut(), area, &title, &app.view, mode, &data);
    app.areas.graph = plot;
    app.areas_graph_last = plot;
}

/// Bir parametre satırı çizer; kaydırıcı çubuğunun alanını döndürür.
fn param_row(buf: &mut Buffer, row: Rect, app: &App, id: ParamId, selected: bool) -> Rect {
    let spec = id.spec();
    let v = app.params.get(id);
    // Modülü kapalıysa (bypass) ayarları soluk göster.
    let bypassed = id
        .module_toggle()
        .is_some_and(|t| t != id && !app.params.on(t));
    let base = if bypassed {
        Style::new().fg(th().dim)
    } else {
        Style::new()
    };
    let label_style = if selected {
        base.bg(th().sel_bg).add_modifier(Modifier::BOLD)
    } else {
        base
    };
    let label_w = 20u16.min(row.width);
    let marker = if selected { "▶ " } else { "  " };
    buf.set_stringn(
        row.x,
        row.y,
        format!("{marker}{:<18}", i18n::pname(spec)),
        label_w as usize,
        label_style,
    );
    let value = i18n::pvalue(spec, v);
    let value_w = 11u16;
    if row.width <= label_w + value_w + 4 {
        return Rect::default();
    }
    let bar = Rect::new(row.x + label_w, row.y, row.width - label_w - value_w - 1, 1);
    match spec.scale {
        Scale::Toggle => {
            let (txt, st) = if v >= 0.5 {
                (
                    l(" AÇIK ", " ON "),
                    Style::new().fg(th().badge_fg).bg(th().ok),
                )
            } else {
                (
                    l(" KAPALI ", " OFF "),
                    Style::new().fg(th().muted).bg(th().sel_bg),
                )
            };
            buf.set_string(bar.x, bar.y, txt, st);
        }
        Scale::Choice(_) => {
            let c = i18n::pchoices(spec);
            let total: u16 = c.iter().map(|n| n.chars().count() as u16 + 3).sum();
            if total > bar.width {
                // Hepsi sığmıyor: yalnızca seçili seçeneği oklarla göster.
                let name = c.get(v.round() as usize).copied().unwrap_or("?");
                let st = base.fg(th().badge_fg).bg(th().accent);
                buf.set_string(bar.x, bar.y, "◀", base.fg(th().muted));
                buf.set_stringn(
                    bar.x + 2,
                    bar.y,
                    format!(" {name} "),
                    bar.width as usize - 4,
                    st,
                );
                let end = (bar.x + 4 + name.chars().count() as u16).min(bar.right() - 1);
                buf.set_string(end, bar.y, "▶", base.fg(th().muted));
            } else {
                let mut x = bar.x;
                for (i, name) in c.iter().enumerate() {
                    let st = if i == v.round() as usize {
                        base.fg(th().badge_fg).bg(th().accent)
                    } else {
                        base.fg(th().muted)
                    };
                    let w = name.chars().count() as u16 + 2;
                    buf.set_string(x, bar.y, format!(" {name} "), st);
                    x += w + 1;
                }
            }
        }
        _ => {
            let n = spec.to_norm(v);
            // Sıfır noktası (ör. 0 dB, orta denge) aralığın içindeyse oradan doldur.
            let zero = if spec.min < 0.0 && spec.max > 0.0 {
                spec.to_norm(0.0)
            } else {
                0.0
            };
            let w = bar.width as f32;
            let (a, b) = if n >= zero { (zero, n) } else { (n, zero) };
            let (a, b) = ((a * w).round() as u16, (b * w).round() as u16);
            let color = if bypassed { th().dim } else { th().accent };
            for i in 0..bar.width {
                let (sym, st) = if i >= a && i < b.max(a + 1) {
                    ("━", Style::new().fg(color))
                } else {
                    ("─", Style::new().fg(th().dim))
                };
                buf.set_string(bar.x + i, bar.y, sym, st);
            }
            let knob = ((n * w) as u16).min(bar.width - 1);
            buf.set_string(bar.x + knob, bar.y, "●", Style::new().fg(color));
        }
    }
    buf.set_stringn(
        bar.right() + 1,
        row.y,
        format!("{value:>10}"),
        value_w as usize,
        base,
    );
    match spec.scale {
        Scale::Toggle | Scale::Choice(_) => Rect::default(),
        _ => bar,
    }
}

fn draw_gain_reduction(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(l(" Kazanç azaltma (dB) ", " Gain reduction (dB) "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let buf = f.buffer_mut();
    // Spektral kapı Gürültü sekmesinde gösterilir.
    for (i, m) in app.gr.iter().take(5).enumerate() {
        let y = inner.y + i as u16;
        if y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, y, inner.width, 1);
        gr_row(buf, row, gr_label(i), *m);
    }
}

/// Kazanç azaltma çubuğu: 0..24 dB, soldan sağa büyür.
fn gr_row(buf: &mut Buffer, area: Rect, label: &str, db: f32) {
    let label_w = 12u16;
    let value_w = 9u16;
    if area.width <= label_w + value_w + 2 {
        return;
    }
    buf.set_string(area.x, area.y, label, Style::new());
    let bar_x = area.x + label_w;
    let bar_w = area.width - label_w - value_w;
    let filled = ((db / 24.0).clamp(0.0, 1.0) * bar_w as f32).round() as u16;
    for i in 0..bar_w {
        let (sym, st) = if i < filled {
            ("█", Style::new().fg(th().special))
        } else {
            ("·", Style::new().fg(th().dim))
        };
        buf.set_string(bar_x + i, area.y, sym, st);
    }
    let v = if db < 0.05 {
        "   0.0".to_string()
    } else {
        format!("{:6.1}", -db)
    };
    buf.set_string(bar_x + bar_w + 1, area.y, v, Style::new());
}

fn preset_name(app: &App, path: &std::path::Path) -> String {
    app.preset_list
        .iter()
        .find(|(e, _)| e.path == path)
        .map(|(e, _)| e.name.clone())
        .unwrap_or_else(|| path.display().to_string())
}

fn draw_presets(f: &mut Frame, app: &mut App, area: Rect) {
    let [left, r] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
    let block = focus_block(l(" Presetler ", " Presets ").into(), true);
    let inner = block.inner(left);
    f.render_widget(block, left);
    let buf = f.buffer_mut();
    if app.preset_list.is_empty() {
        buf.set_string(
            inner.x,
            inner.y,
            l(
                "Preset yok. n ile şu anki ayarları kaydedin.",
                "No presets. Press n to save the current settings.",
            ),
            Style::new().fg(th().warn),
        );
    }
    // Seçili satır görünür kalsın.
    let visible = inner.height as usize;
    let first = app.preset_sel.saturating_sub(visible.saturating_sub(1));
    for (row_i, (i, (e, _))) in app
        .preset_list
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .enumerate()
    {
        let y = inner.y + row_i as u16;
        let row = Rect::new(inner.x, y, inner.width, 1);
        app.areas.preset_rows.push(row);
        let sel = i == app.preset_sel;
        let current = app.current_preset.as_deref() == Some(e.name.as_str());
        let st = if sel {
            Style::new().bg(th().sel_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let mark = if current { "●" } else { " " };
        let dirty = if current && app.preset_dirty {
            " *"
        } else {
            ""
        };
        buf.set_stringn(
            row.x,
            y,
            format!("{} {mark} {}{dirty}", if sel { "▶" } else { " " }, e.name),
            row.width as usize,
            st,
        );
    }
    // preset_rows, listedeki gerçek indekslere karşılık gelsin diye başa boş alan ekle.
    let mut rows = vec![Rect::default(); first];
    rows.append(&mut app.areas.preset_rows);
    app.areas.preset_rows = rows;

    let mut lines = vec![];
    if let Some((e, desc)) = app.preset_list.get(app.preset_sel) {
        lines.push(Line::from(Span::styled(
            e.name.clone(),
            Style::new().bold(),
        )));
        if !desc.is_empty() {
            lines.push(Line::from(crate::preset::localized_description(desc)));
        }
        lines.push(Line::from(Span::styled(
            e.path.display().to_string(),
            Style::new().fg(th().dim),
        )));
        lines.push(Line::from(""));
    }
    let cur = match &app.current_preset {
        Some(n) if app.preset_dirty => tr!(
            format!("Etkin: {n} (değiştirildi *)"),
            format!("Active: {n} (modified *)")
        ),
        Some(n) => tr!(format!("Etkin: {n}"), format!("Active: {n}")),
        None => l(
            "Etkin: — (kaydedilmemiş ayarlar)",
            "Active: — (unsaved settings)",
        )
        .into(),
    };
    lines.push(Line::from(cur));
    let (u, r_) = app.history_lens();
    let ab = if app.ab_is_b { "B" } else { "A" };
    lines.push(Line::from(tr!(
        format!("A/B: {ab} etkin (Boşluk ile geçiş) │ geri al: {u} │ yinele: {r_}"),
        format!("A/B: {ab} active (Space to switch) │ undo: {u} │ redo: {r_}")
    )));
    if let Some(store) = &app.presets {
        lines.push(Line::from(Span::styled(
            tr!(
                format!("Klasör: {}", store.dir().display()),
                format!("Folder: {}", store.dir().display())
            ),
            Style::new().fg(th().dim),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        l(
            "Yükleme geri alınabilir (u). Gürültü profili presetlere dahil değildir.",
            "Loading can be undone (u). The noise profile is not part of presets.",
        ),
        Style::new().fg(th().dim),
    )));
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(l(" Ayrıntılar ", " Details "))),
        r,
    );
}

fn draw_chain(f: &mut Frame, app: &mut App, area: Rect) {
    use micforge_dsp::chain::MODULE_TOGGLES;
    use micforge_dsp::params::MODULE_COUNT;
    let [left, r] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
    let block = focus_block(
        l(
            " İşleme zinciri (yukarıdan aşağı) ",
            " Processing chain (top to bottom) ",
        )
        .into(),
        true,
    );
    let inner = block.inner(left);
    f.render_widget(block, left);
    let order = app.params.order();
    let buf = f.buffer_mut();
    let dim = Style::new().fg(th().dim);
    if inner.height < MODULE_COUNT as u16 + 2 || inner.width < 30 {
        buf.set_string(inner.x, inner.y, l("Alan çok küçük", "Area too small"), dim);
        return;
    }
    buf.set_string(
        inner.x,
        inner.y,
        l(
            "   ⇣ Giriş (kazanç, faz, kanal) — sabit",
            "   ⇣ Input (gain, phase, channel) — fixed",
        ),
        dim,
    );
    for (pos, &m) in order.iter().enumerate() {
        let y = inner.y + 1 + pos as u16;
        let row = Rect::new(inner.x, y, inner.width, 1);
        app.areas.chain_rows.push(row);
        let on = app.params.on(MODULE_TOGGLES[m as usize]);
        let sel = pos == app.chain_sel;
        let base = if sel {
            Style::new().bg(th().sel_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let marker = if sel { "▶" } else { " " };
        buf.set_stringn(
            row.x,
            y,
            format!(
                "{marker} {:>2}. {:<22}",
                pos + 1,
                i18n::module_name(m as usize)
            ),
            row.width as usize,
            base,
        );
        let (txt, st) = if on {
            (
                l(" AÇIK ", " ON "),
                Style::new().fg(th().badge_fg).bg(th().ok),
            )
        } else {
            (
                l(" KAPALI ", " OFF "),
                Style::new().fg(th().muted).bg(th().bg),
            )
        };
        buf.set_string(row.x + 30, y, txt, st);
    }
    buf.set_string(
        inner.x,
        inner.y + 1 + order.len() as u16,
        l(
            "   ⇣ Çıkış kazancı → Limiter → Denge — sabit (tavan güvencesi)",
            "   ⇣ Output gain → Limiter → Balance — fixed (ceiling guarantee)",
        ),
        dim,
    );
    let text = l(
        concat!(
            "Modüller yukarıdan aşağı bu sırayla işlenir.\n\n",
            "Taşımak için: J/K, Shift+↑/↓ veya satırı fareyle sürükleyin.\n",
            "Aç/kapat: b veya Enter.\n\n",
            "Giriş kademesi her zaman başta; çıkış kazancı ve limiter her zaman ",
            "sondadır, böylece çıkış tavanı hiçbir sırada aşılmaz.\n\n",
            "Örnek: gürültü engellemeyi compressor'dan önce koymak, compressor'ın ",
            "arka plan gürültüsünü yükseltmesini önler.",
        ),
        concat!(
            "Modules are processed in this order, top to bottom.\n\n",
            "To move one: J/K, Shift+↑/↓, or drag the row with the mouse.\n",
            "Toggle on/off: b or Enter.\n\n",
            "The input stage is always first; output gain and the limiter are always ",
            "last, so the output ceiling can never be exceeded in any order.\n\n",
            "Example: placing noise suppression before the compressor stops the ",
            "compressor from boosting background noise.",
        ),
    );
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(l(" Bilgi ", " Info "))),
        r,
    );
}

fn draw_noise_status(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered().title(l(" Gürültü profili ", " Noise profile "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.learn_button = Rect::default();
    if inner.height < 5 || inner.width < 30 {
        return;
    }
    let sh = &app.engine.shared;
    let buf = f.buffer_mut();
    let has = sh.noise_profile.load(Relaxed);
    let (txt, st) = if has {
        (
            l("Profil: var ✓", "Profile: ready ✓"),
            Style::new().fg(th().ok),
        )
    } else {
        (
            l(
                "Profil: yok — spektral kapı için önce öğretin",
                "Profile: none — learn one first for the spectral gate",
            ),
            Style::new().fg(th().warn),
        )
    };
    buf.set_string(inner.x, inner.y, txt, st);
    let y = inner.y + 2;
    if let Some(rem) = app.learn_remaining() {
        buf.set_string(
            inner.x,
            y,
            tr!(
                format!("● Öğreniyor… sessiz kalın ({rem:.1} sn)"),
                format!("● Learning… stay silent ({rem:.1} s)")
            ),
            Style::new().fg(th().badge_fg).bg(th().warn).bold(),
        );
    } else {
        let label = l(
            " [ Gürültü profili öğren (p) ] ",
            " [ Learn noise profile (p) ] ",
        );
        let w = label.chars().count() as u16;
        buf.set_string(
            inner.x,
            y,
            label,
            Style::new().fg(th().badge_fg).bg(th().accent).bold(),
        );
        app.learn_button = Rect::new(inner.x, y, w.min(inner.width), 1);
    }
    let vad = sh.vad.load().clamp(0.0, 1.0);
    let bar_w = inner.width.saturating_sub(24).min(40);
    let filled = (vad * bar_w as f32).round() as u16;
    buf.set_string(
        inner.x,
        inner.y + 4,
        l("Ses aktivitesi (VAD)", "Voice activity (VAD)"),
        Style::new(),
    );
    for i in 0..bar_w {
        let (sym, st) = if i < filled {
            ("█", Style::new().fg(th().ok))
        } else {
            ("·", Style::new().fg(th().dim))
        };
        buf.set_string(inner.x + 22 + i, inner.y + 4, sym, st);
    }
    let gr = app.gr[5];
    buf.set_string(
        inner.x,
        inner.y + 5,
        format!(
            "{}  {}",
            l("Spektral azaltma", "Spectral reduction"),
            if gr < 0.05 {
                "0.0 dB".to_string()
            } else {
                format!("{:.1} dB", -gr)
            }
        ),
        Style::new().fg(th().special),
    );
}

fn draw_levels_compact(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(l(" Seviyeler (dBFS) ", " Levels (dBFS) "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Length(1); 5]).split(inner);
    let buf = f.buffer_mut();
    meter_row(buf, rows[0], l("Giriş  Tepe", "Input  Peak"), &app.in_peak);
    meter_row(buf, rows[1], l("Çıkış  Tepe", "Output Peak"), &app.out_peak);
    meter_row(buf, rows[2], l("Çıkış  RMS ", "Output RMS "), &app.out_rms);
    draw_flags(buf, rows[4], app);
}

fn draw_flags(buf: &mut Buffer, area: Rect, app: &App) {
    let mut flags = vec![];
    if app.clip_until.is_some() {
        flags.push(Span::styled(
            " CLIP ",
            Style::new().fg(th().badge_fg).bg(th().danger).bold(),
        ));
        flags.push(Span::raw(" "));
    }
    if app.muted() {
        flags.push(Span::styled(
            l(" SESSİZ ", " MUTED "),
            Style::new().fg(th().badge_fg).bg(th().warn).bold(),
        ));
        flags.push(Span::raw(" "));
    }
    if let Some((freq, _)) = app.feedback_hit {
        flags.push(Span::styled(
            tr!(
                format!(" GERİ BESLEME ~{freq:.0} Hz "),
                format!(" FEEDBACK ~{freq:.0} Hz ")
            ),
            Style::new().fg(th().badge_fg).bg(th().danger).bold(),
        ));
        flags.push(Span::raw(" "));
    }
    if app.params.on(micforge_dsp::params::ParamId::MonitorOn) {
        flags.push(Span::styled(
            l(" MONİTOR ", " MONITOR "),
            Style::new().fg(th().badge_fg).bg(th().accent).bold(),
        ));
    }
    buf.set_line(area.x, area.y, &Line::from(flags), area.width);
}

fn draw_meters(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered().title(l(" Seviyeler (dBFS) ", " Levels (dBFS) "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::vertical([Constraint::Length(1); 8]).split(inner);
    let buf = f.buffer_mut();
    meter_row(buf, rows[0], l("Giriş  Tepe", "Input  Peak"), &app.in_peak);
    meter_row(buf, rows[1], l("Giriş  RMS ", "Input  RMS "), &app.in_rms);
    meter_row(buf, rows[3], l("Çıkış  Tepe", "Output Peak"), &app.out_peak);
    meter_row(buf, rows[4], l("Çıkış  RMS ", "Output RMS "), &app.out_rms);
    // Gate, compressor ve limiter kazanç azaltması (ayrıntı: Dinamikler sekmesi).
    let gr = |i: usize| if app.gr[i] < 0.05 { 0.0 } else { -app.gr[i] };
    buf.set_string(
        rows[5].x,
        rows[5].y,
        format!(
            "{:<13}Gate {:5.1}  Comp {:5.1}  Lim {:5.1} dB",
            l("Azaltma", "Reduction"),
            gr(0),
            gr(2),
            gr(4)
        ),
        Style::new().fg(th().special),
    );
    let lufs = |v: f64| {
        if v.is_finite() {
            format!("{v:6.1}")
        } else {
            "   -- ".to_string()
        }
    };
    buf.set_string(
        rows[6].x,
        rows[6].y,
        format!(
            "LUFS         M {}  S {}  I {}   ({})",
            lufs(app.lufs_vals[0]),
            lufs(app.lufs_vals[1]),
            lufs(app.lufs_vals[2]),
            l("i: sıfırla", "i: reset")
        ),
        Style::new().fg(th().accent),
    );
    draw_flags(buf, rows[7], app);
}

/// Ayarlar sekmesi: dil, tema, sanal mikrofon kanalları ve özel renk bilgisi.
fn draw_settings(f: &mut Frame, app: &mut App, area: Rect) {
    let t = th();
    let [left, r] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
    let block = focus_block(l(" Ayarlar ", " Settings ").into(), true);
    let inner = block.inner(left);
    f.render_widget(block, left);
    let rows: [(&str, &str); crate::app::SETTINGS_COUNT] = [
        (
            "Dil / Language",
            if crate::i18n::en() {
                "English"
            } else {
                "Türkçe"
            },
        ),
        (
            l("Tema", "Theme"),
            if app.config.light_theme() {
                l("Açık", "Light")
            } else {
                l("Koyu", "Dark")
            },
        ),
        (
            l("Sanal mikrofon çıkışı", "Virtual mic output"),
            if app.config.out_channels() == 1 {
                "Mono"
            } else {
                "Stereo"
            },
        ),
    ];
    let buf = f.buffer_mut();
    for (i, (name, value)) in rows.iter().enumerate() {
        let y = inner.y + 1 + 2 * i as u16;
        if y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, y, inner.width, 1);
        app.areas.settings_rows.push(row);
        let sel = i == app.settings_sel;
        let base = if sel {
            Style::new().bg(t.sel_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        let marker = if sel { "▶" } else { " " };
        buf.set_stringn(
            row.x,
            y,
            format!("{marker} {name:<26}"),
            row.width as usize,
            base,
        );
        buf.set_string(
            row.x + 29,
            y,
            format!(" ◀ {value} ▶ "),
            Style::new().fg(t.badge_fg).bg(t.accent),
        );
    }
    let path = crate::config::dir()
        .map(|d| d.join("config.toml").display().to_string())
        .unwrap_or_default();
    let roles = crate::theme::Theme::KEYS.join(", ");
    let mut text = tr!(
        format!(
            "Özel renkler config dosyasında [theme_colors] tablosuyla verilir; \
             seçili temanın üstüne uygulanır:\n\n{path}\n\n[theme_colors]\naccent = \"#ff8800\"\n\
             sel_bg = \"236\"\nok = \"lightgreen\"\n\nRoller: {roles}\n\n\
             Değişiklikler uygulama yeniden başlatılınca okunur."
        ),
        format!(
            "Custom colors are set in the config file with a [theme_colors] table; \
             they are applied on top of the selected theme:\n\n{path}\n\n[theme_colors]\naccent = \"#ff8800\"\n\
             sel_bg = \"236\"\nok = \"lightgreen\"\n\nRoles: {roles}\n\n\
             Changes are read when the app restarts."
        )
    );
    if !app.config.theme_colors.is_empty() {
        let n = app.config.theme_colors.len() - app.theme_errors.len();
        text.push_str(&tr!(
            format!("\n\nEtkin özel renk: {n}"),
            format!("\n\nActive custom colors: {n}")
        ));
    }
    if !app.theme_errors.is_empty() {
        let bad = app.theme_errors.join(", ");
        text.push_str(&tr!(
            format!("\nGeçersiz (atlandı): {bad}"),
            format!("\nInvalid (skipped): {bad}")
        ));
    }
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(l(" Özel renkler ", " Custom colors "))),
        r,
    );
}

/// `?` yardım ekranı: tüm kısayollar.
fn draw_help(f: &mut Frame) {
    let text = l(
        concat!(
            "Genel\n",
            "  1-8 / Tab / Shift+Tab   sekme değiştir\n",
            "  h / l                   panel (odak) değiştir\n",
            "  j / k, ↑ / ↓            satır seç\n",
            "  ← / →                   değer ayarla (Shift: ince adım)\n",
            "  Enter                   sayısal değer gir / seçeneği değiştir\n",
            "  b                       seçili modülü aç/kapat (bypass)\n",
            "  r                       seçili değeri varsayılana döndür\n",
            "  m                       sessiz (mute)\n",
            "  Space                   A/B karşılaştırma\n",
            "  u / Ctrl+r              geri al / yinele\n",
            "  ?                       bu ekran        q / Esc  çık\n",
            "\n",
            "Sekmeye özel\n",
            "  Genel      Enter: cihaza bağlan  d: bağlantıyı kes  i: LUFS sıfırla\n",
            "  EQ         [ ]: bant  s: spektrum görünümü  fare: nokta sürükle, tekerlek: Q\n",
            "  Gürültü    p: gürültü profili öğren (3 sn sessizlik)\n",
            "  Zincir     J/K, Shift+↑/↓ veya sürükle: taşı  r: varsayılan sıra\n",
            "  Presetler  Enter: yükle  n: yeni  s: üzerine kaydet  c: kopyala  d: sil\n",
            "             i: içe aktar  e: dışa aktar\n",
            "\n",
            "Fare: kaydırıcıları sürükleyin, tekerlekle ayarlayın.",
        ),
        concat!(
            "General\n",
            "  1-8 / Tab / Shift+Tab   switch tab\n",
            "  h / l                   switch panel (focus)\n",
            "  j / k, ↑ / ↓            select row\n",
            "  ← / →                   adjust value (Shift: fine step)\n",
            "  Enter                   type a numeric value / change option\n",
            "  b                       toggle selected module on/off (bypass)\n",
            "  r                       reset selected value to default\n",
            "  m                       mute\n",
            "  Space                   A/B comparison\n",
            "  u / Ctrl+r              undo / redo\n",
            "  ?                       this screen        q / Esc  quit\n",
            "\n",
            "Tab-specific\n",
            "  General    Enter: connect to device  d: disconnect  i: reset LUFS\n",
            "  EQ         [ ]: band  s: spectrum view  mouse: drag point, wheel: Q\n",
            "  Noise      p: learn noise profile (3 s of silence)\n",
            "  Chain      J/K, Shift+↑/↓ or drag: move  r: default order\n",
            "  Presets    Enter: load  n: new  s: overwrite  c: duplicate  d: delete\n",
            "             i: import  e: export\n",
            "\n",
            "Mouse: drag the sliders, adjust with the wheel.",
        ),
    );
    popup(
        f,
        l(" Yardım ", " Help "),
        text,
        l("[?/Esc] Kapat", "[?/Esc] Close"),
        th().accent,
        84,
        27,
    );
}

/// Etiket + yatay çubuk + sayısal değer. Yeşil < -18, sarı < -6, kırmızı üstü.
fn meter_row(buf: &mut Buffer, area: Rect, label: &str, m: &Meter) {
    let label_w = 13u16;
    let value_w = 8u16;
    if area.width <= label_w + value_w + 2 {
        return;
    }
    buf.set_string(area.x, area.y, label, Style::new());
    let bar_x = area.x + label_w;
    let bar_w = area.width - label_w - value_w;
    let frac = |db: f32| ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
    let filled = (frac(m.db) * bar_w as f32).round() as u16;
    let hold = ((frac(m.hold_db) * bar_w as f32).round() as u16).min(bar_w.saturating_sub(1));
    for i in 0..bar_w {
        let cell_db = METER_FLOOR_DB + (i as f32 + 0.5) / bar_w as f32 * -METER_FLOOR_DB;
        let color = if cell_db < -18.0 {
            th().ok
        } else if cell_db < -6.0 {
            th().warn
        } else {
            th().danger
        };
        let (sym, style) = if i < filled {
            ("█", Style::new().fg(color))
        } else if i == hold && m.hold_db > METER_FLOOR_DB {
            ("│", Style::new().fg(color))
        } else {
            ("·", Style::new().fg(th().dim))
        };
        buf.set_string(bar_x + i, area.y, sym, style);
    }
    let v = if m.db <= METER_FLOOR_DB {
        "  -inf".to_string()
    } else {
        format!("{:6.1}", m.db)
    };
    buf.set_string(bar_x + bar_w + 1, area.y, v, Style::new());
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let sh = &app.engine.shared;
    let (state, color) = match &app.status {
        _ if app.engine_retry_at.is_some() => (
            l(
                "ses motoru durdu — yeniden bağlanılıyor",
                "audio engine stopped — reconnecting",
            ),
            th().danger,
        ),
        CaptureStatus::Idle => (l("cihaz seçilmedi", "no device selected"), th().warn),
        CaptureStatus::Waiting(_) => (l("cihaz bekleniyor", "waiting for device"), th().warn),
        CaptureStatus::Active(_) => (l("aktif", "active"), th().ok),
    };
    let block = sh.block_frames.load(Relaxed);
    let block = if block == 0 { QUANTUM } else { block };
    let ms = |frames: u32| frames as f32 * 1000.0 / SAMPLE_RATE as f32;
    let latency = ms(block) + ms(sh.ring_frames.load(Relaxed)) + ms(sh.dsp_latency.load(Relaxed));
    let cpu = sh.cpu_load.load() * 100.0;
    let ab = if app.ab_is_b { " B " } else { " A " };
    let mut spans = vec![
        Span::styled(ab, Style::new().fg(th().badge_fg).bg(th().accent).bold()),
        Span::raw(" "),
    ];
    if let Some((msg, until)) = &app.toast
        && std::time::Instant::now() < *until
    {
        spans.push(Span::styled(
            format!(" {msg} "),
            Style::new().fg(th().badge_fg).bg(th().warn),
        ));
        spans.push(Span::raw(" "));
    }
    if let Some(name) = &app.current_preset {
        let dirty = if app.preset_dirty { "*" } else { "" };
        spans.push(Span::styled(
            format!("[{name}{dirty}] "),
            Style::new().fg(th().accent),
        ));
    }
    let mut line = Line::from(spans);
    let rest = Line::from(vec![
        Span::raw(format!("{VIRTUAL_DESCRIPTION}: ")),
        Span::styled(state, Style::new().fg(color).bold()),
        Span::raw(tr!(
            format!(
                " │ blok {block} ({:.1} ms) │ ek gecikme ≈ {latency:.1} ms │ CPU {cpu:.1}% │ underrun {}",
                ms(block),
                sh.underruns.load(Relaxed)
            ),
            format!(
                " │ block {block} ({:.1} ms) │ latency ≈ {latency:.1} ms │ CPU {cpu:.1}% │ underruns {}",
                ms(block),
                sh.underruns.load(Relaxed)
            )
        )),
    ]);
    line.spans.extend(rest.spans);
    f.render_widget(Paragraph::new(line).style(Style::new().bg(th().bg)), area);
}

fn popup(f: &mut Frame, title: &str, text: &str, footer: &str, color: Color, w: u16, h: u16) {
    let area = f.area();
    let w = area.width.min(w);
    let h = area.height.min(h);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, r);
    let block = Block::bordered()
        .border_style(Style::new().fg(color))
        .title(title)
        .title_bottom(Line::from(footer).right_aligned());
    f.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .style(Style::new().bg(th().bg).fg(th().fg))
            .block(block),
        r,
    );
}
