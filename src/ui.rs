//! The window: a sidebar with three pages (Send, Listen, Settings).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, rgb, AnyElement, App, ClickEvent, Context, FocusHandle, FontWeight,
    KeyDownEvent, Rgba, SharedString, Window,
};

use crate::codec::Codec;
use crate::engine::{Engine, PlayerState, Quality, ServerState};
use crate::settings::{BUFFER_MAX_MS, BUFFER_MIN_MS};
use crate::settings::Settings;
use crate::usb::{Usb, FORWARD_OFFSET};

// ------------------------------------------------------------------ colours

const BG_SIDEBAR: u32 = 0x101218;
const BG_MAIN: u32 = 0x161920;
const CARD: u32 = 0x1d212a;
const CARD_HI: u32 = 0x252a35;
const BORDER: u32 = 0x2a303c;
const TEXT: u32 = 0xe9ecf2;
const MUTED: u32 = 0x8b93a4;
const FAINT: u32 = 0x5b6375;
const ACCENT: u32 = 0x47d1b8;
const ACCENT_DARK: u32 = 0x163a36;
const ON_ACCENT: u32 = 0x08201c;
const DANGER: u32 = 0xf06b6b;
const DANGER_DARK: u32 = 0x3a1d22;
const WARN: u32 = 0xf2c14e;

fn c(hex: u32) -> Rgba {
    rgb(hex)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    Send,
    Listen,
    Usb,
    Settings,
}

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    /// Sidebar with descriptions, cards side by side.
    Wide,
    /// Slim sidebar, cards stacked.
    Compact,
    /// Navigation across the top, cards stacked.
    Narrow,
}

pub const SHOT_PENDING: u8 = 0;
const SHOT_OK: u8 = 1;
const SHOT_FAILED: u8 = 2;

pub struct ShotState {
    pub result: std::sync::Arc<std::sync::atomic::AtomicU8>,
    pub path: PathBuf,
    pub title: String,
    pub frames: u32,
    pub started: bool,
}

pub struct AirwaveApp {
    engine: Engine,
    usb: Usb,
    usb_refreshed: Instant,
    settings: Settings,
    page: Page,
    address: String,
    address_focus: FocusHandle,
    server_meter: f32,
    player_meter: f32,
    last_bytes: u32,
    rate_kbps: f32,
    rate_at: Instant,
    shot: Option<ShotState>,
    persist: bool,
    layout: Layout,
}

impl AirwaveApp {
    pub fn new(shot: Option<ShotState>, start_page: Option<String>, cx: &mut Context<Self>) -> Self {
        let page = match start_page.as_deref() {
            Some("listen") => Page::Listen,
            Some("usb") => Page::Usb,
            Some("settings") => Page::Settings,
            _ => Page::Send,
        };
        let persist = shot.is_none();
        let settings = if persist {
            Settings::load()
        } else {
            Settings::default()
        };
        let mut engine = Engine::new(settings.volume, settings.buffer_ms, settings.auto_scan);
        if settings.auto_start_server && persist {
            engine.start_server(
                settings.source_device.clone(),
                settings.quality,
                settings.codec,
                settings.port,
            );
        }

        // Refresh the screen ~20 times a second so meters and lists stay live.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            if this
                .update(cx, |app, cx| {
                    app.tick();
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        })
        .detach();

        let usb = Usb::new();
        usb.refresh();
        AirwaveApp {
            address: settings.last_address.clone(),
            engine,
            usb,
            usb_refreshed: Instant::now(),
            settings,
            page,
            address_focus: cx.focus_handle(),
            server_meter: 0.0,
            player_meter: 0.0,
            last_bytes: 0,
            rate_kbps: 0.0,
            rate_at: Instant::now(),
            shot,
            persist,
            layout: Layout::Wide,
        }
    }

    fn tick(&mut self) {
        if self.page == Page::Usb && self.usb_refreshed.elapsed() > Duration::from_secs(3) {
            self.usb_refreshed = Instant::now();
            if !self.usb.state.lock().unwrap().busy {
                self.usb.refresh();
            }
        }
        let m = &self.engine.meters;
        self.server_meter = level_to_meter(m.server_level()).max(self.server_meter * 0.82);
        self.player_meter = level_to_meter(m.player_level()).max(self.player_meter * 0.82);
        let dt = self.rate_at.elapsed().as_secs_f32();
        if dt >= 1.0 {
            let b = m.bytes_sent.load(Ordering::Relaxed);
            self.rate_kbps = b.wrapping_sub(self.last_bytes) as f32 * 8.0 / 1000.0 / dt;
            self.last_bytes = b;
            self.rate_at = Instant::now();
        }
    }

    fn save(&self) {
        if self.persist {
            self.settings.save();
        }
    }

    fn toggle_server(&mut self) {
        if self.engine.server_running() {
            self.engine.stop_server();
        } else {
            self.engine.start_server(
                self.settings.source_device.clone(),
                self.settings.quality,
                self.settings.codec,
                self.settings.port,
            );
        }
    }

    fn connect_to(&mut self, address: String) {
        if address.trim().is_empty() {
            return;
        }
        self.settings.last_address = address.trim().to_string();
        self.save();
        self.engine
            .connect(address, self.settings.output_devices.clone());
    }

    /// Speakers changed while listening: reconnect so they take effect now.
    fn restart_player_if_playing(&mut self) {
        let addr = match &self.engine.status.lock().unwrap().player {
            PlayerState::Playing(a) | PlayerState::Connecting(a) => Some(a.clone()),
            _ => None,
        };
        if let Some(a) = addr {
            self.engine.connect(a, self.settings.output_devices.clone());
        }
    }

    fn set_buffer(&mut self, ms: u32) {
        self.settings.buffer_ms = ms.clamp(BUFFER_MIN_MS, BUFFER_MAX_MS);
        self.engine
            .meters
            .latency_ms
            .store(self.settings.buffer_ms, Ordering::Relaxed);
        self.save();
    }

    fn set_volume(&mut self, v: f32) {
        self.settings.volume = v.clamp(0.0, 1.0);
        self.engine.meters.set_volume(self.settings.volume);
        self.engine.meters.muted.store(false, Ordering::Relaxed);
        self.save();
    }

    fn on_address_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        match k.key.as_str() {
            "backspace" => {
                self.address.pop();
            }
            "enter" => {
                let a = self.address.clone();
                self.connect_to(a);
            }
            "escape" => self.address.clear(),
            _ => {
                if k.modifiers.control || k.modifiers.platform || k.modifiers.alt {
                    return;
                }
                if let Some(ch) = &k.key_char {
                    for c in ch.chars() {
                        if (c.is_ascii_alphanumeric() || ".:-[]".contains(c))
                            && self.address.len() < 64
                        {
                            self.address.push(c);
                        }
                    }
                }
            }
        }
        cx.notify();
    }
}

/// Peak (0..1) to meter position (0..1) on a 60 dB scale.
fn level_to_meter(peak: f32) -> f32 {
    if peak <= 0.000_001 {
        return 0.0;
    }
    ((20.0 * peak.log10() + 60.0) / 60.0).clamp(0.0, 1.0)
}

// ------------------------------------------------------------ small pieces

fn label(text: impl Into<SharedString>) -> impl IntoElement {
    div().text_xs().text_color(c(MUTED)).child(text.into())
}

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_5()
        .bg(c(CARD))
        .border_1()
        .border_color(c(BORDER))
        .rounded(px(14.))
}

fn card_title(title: &str, hint: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_grow()
        .min_w_0()
        .gap_0p5()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(TEXT))
                .child(title.to_string()),
        )
        .when(!hint.is_empty(), |d| d.child(label(hint.to_string())))
}

#[derive(Clone, Copy, PartialEq)]
enum Look {
    Primary,
    Danger,
    Quiet,
}

fn button(
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
    look: Look,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let (bg, fg, hover, border) = match look {
        Look::Primary => (ACCENT, ON_ACCENT, 0x5fe0c9, ACCENT),
        Look::Danger => (DANGER_DARK, DANGER, 0x4a232a, 0x5a2a31),
        Look::Quiet => (CARD_HI, TEXT, 0x2e3441, BORDER),
    };
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .px_4()
        .h(px(34.))
        .rounded(px(9.))
        .bg(c(bg))
        .border_1()
        .border_color(c(border))
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c(fg))
        .cursor_pointer()
        .hover(move |s| s.bg(c(hover)))
        .active(|s| s.opacity(0.85))
        .child(text.into())
        .on_click(on_click)
}

fn dot(color: u32) -> impl IntoElement {
    div().size(px(10.)).rounded_full().bg(c(color))
}

fn meter(id: &'static str, level: f32, active: bool) -> impl IntoElement {
    const N: usize = 32;
    let lit = (level * N as f32).round() as usize;
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_end()
        .gap(px(3.))
        .h(px(22.))
        .children((0..N).map(move |i| {
            let color = if !active || i >= lit {
                0x2a303c
            } else if i >= N - 3 {
                DANGER
            } else if i >= N - 9 {
                WARN
            } else {
                ACCENT
            };
            div()
                .w(px(7.))
                .h(px(10. + 12. * i as f32 / N as f32))
                .rounded(px(2.))
                .bg(c(color))
        }))
}

fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    current: T,
    cx: &mut Context<AirwaveApp>,
    on_pick: fn(&mut AirwaveApp, T),
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .p(px(3.))
        .gap(px(3.))
        .bg(c(BG_MAIN))
        .border_1()
        .border_color(c(BORDER))
        .rounded(px(10.))
        .children(options.iter().enumerate().map(|(i, (value, text))| {
            let value = *value;
            let selected = value == current;
            div()
                .id((id, i))
                .px_3()
                .h(px(28.))
                .flex()
                .items_center()
                .rounded(px(7.))
                .text_sm()
                .cursor_pointer()
                .when(selected, |d| {
                    d.bg(c(ACCENT_DARK))
                        .text_color(c(ACCENT))
                        .font_weight(FontWeight::SEMIBOLD)
                })
                .when(!selected, |d| {
                    d.text_color(c(MUTED)).hover(|s| s.bg(c(CARD_HI)))
                })
                .child(*text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    on_pick(this, value);
                    this.save();
                    cx.notify();
                }))
        }))
}

fn toggle(
    id: &'static str,
    on: bool,
    title: &str,
    hint: &str,
    cx: &mut Context<AirwaveApp>,
    flip: fn(&mut AirwaveApp),
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_4()
        .cursor_pointer()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_sm().text_color(c(TEXT)).child(title.to_string()))
                .child(label(hint.to_string())),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .w(px(42.))
                .h(px(24.))
                .p(px(3.))
                .rounded_full()
                .bg(c(if on { ACCENT } else { CARD_HI }))
                .border_1()
                .border_color(c(if on { ACCENT } else { BORDER }))
                .when(on, |d| d.justify_end())
                .child(
                    div()
                        .size(px(16.))
                        .rounded_full()
                        .bg(c(if on { ON_ACCENT } else { MUTED })),
                ),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            flip(this);
            this.save();
            cx.notify();
        }))
}

/// A list of devices where one row can be chosen. `None` = system default.
fn device_list(
    id: &'static str,
    names: &[String],
    default_name: Option<&String>,
    chosen: Option<&String>,
    loaded: bool,
    cx: &mut Context<AirwaveApp>,
    on_pick: fn(&mut AirwaveApp, Option<String>),
) -> impl IntoElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    let default_text = match default_name {
        Some(n) if n != "default" => format!("System default ({n})"),
        _ => "System default".to_string(),
    };
    let mut all: Vec<(Option<String>, String)> = vec![(None, default_text)];
    all.extend(
        names
            .iter()
            .filter(|n| n.as_str() != "default")
            .map(|n| (Some(n.clone()), n.clone())),
    );
    for (i, (value, text)) in all.into_iter().enumerate() {
        let selected = value.as_ref() == chosen;
        rows.push(
            div()
                .id((id, i))
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(px(8.))
                .cursor_pointer()
                .when(selected, |d| d.bg(c(ACCENT_DARK)))
                .when(!selected, |d| d.hover(|s| s.bg(c(CARD_HI))))
                .child(
                    div()
                        .flex_none()
                        .size(px(14.))
                        .rounded_full()
                        .border_2()
                        .border_color(c(if selected { ACCENT } else { FAINT }))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(selected, |d| {
                            d.child(div().size(px(6.)).rounded_full().bg(c(ACCENT)))
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_sm()
                        .text_color(c(if selected { TEXT } else { MUTED }))
                        .child(text),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    on_pick(this, value.clone());
                    this.save();
                    cx.notify();
                }))
                .into_any_element(),
        );
    }
    if !loaded {
        rows.push(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(c(FAINT))
                .child("Looking for sound devices…")
                .into_any_element(),
        );
    }
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_1()
        .h(px(196.))
        .overflow_y_scroll()
        .children(rows)
}

/// Like `device_list`, but several rows can be ticked at once.
fn multi_device_list(
    id: &'static str,
    names: &[String],
    default_name: Option<&String>,
    chosen: &[String],
    loaded: bool,
    cx: &mut Context<AirwaveApp>,
) -> impl IntoElement {
    let default_text = match default_name {
        Some(n) if n != "default" => format!("System default ({n})"),
        _ => "System default".to_string(),
    };
    let mut all: Vec<(Option<String>, String)> = vec![(None, default_text)];
    all.extend(
        names
            .iter()
            .filter(|n| n.as_str() != "default")
            .map(|n| (Some(n.clone()), n.clone())),
    );
    let mut rows: Vec<AnyElement> = Vec::new();
    for (i, (value, text)) in all.into_iter().enumerate() {
        let ticked = match &value {
            None => chosen.is_empty(),
            Some(v) => chosen.contains(v),
        };
        rows.push(
            div()
                .id((id, i))
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(px(8.))
                .cursor_pointer()
                .when(ticked, |d| d.bg(c(ACCENT_DARK)))
                .when(!ticked, |d| d.hover(|s| s.bg(c(CARD_HI))))
                .child(
                    div()
                        .flex_none()
                        .size(px(16.))
                        .rounded(px(4.))
                        .border_2()
                        .border_color(c(if ticked { ACCENT } else { FAINT }))
                        .when(ticked, |d| d.bg(c(ACCENT)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(ticked, |d| {
                            d.child(div().size(px(6.)).rounded(px(1.)).bg(c(ON_ACCENT)))
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_sm()
                        .text_color(c(if ticked { TEXT } else { MUTED }))
                        .child(text),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    match &value {
                        None => this.settings.output_devices.clear(),
                        Some(v) => {
                            let list = &mut this.settings.output_devices;
                            if let Some(pos) = list.iter().position(|x| x == v) {
                                list.remove(pos);
                            } else {
                                list.push(v.clone());
                            }
                        }
                    }
                    this.save();
                    this.restart_player_if_playing();
                    cx.notify();
                }))
                .into_any_element(),
        );
    }
    if !loaded {
        rows.push(
            div()
                .px_3()
                .py_2()
                .text_xs()
                .text_color(c(FAINT))
                .child("Looking for sound devices…")
                .into_any_element(),
        );
    }
    div()
        .id(id)
        .flex()
        .flex_col()
        .gap_1()
        .h(px(196.))
        .overflow_y_scroll()
        .children(rows)
}

/// Two blocks side by side, or one above the other when space is short.
fn pair(stack: bool, a: impl IntoElement, b: impl IntoElement) -> impl IntoElement {
    if stack {
        div()
            .flex()
            .flex_col()
            .gap_5()
            .w_full()
            .child(div().w_full().flex().flex_col().child(a))
            .child(div().w_full().flex().flex_col().child(b))
    } else {
        div()
            .flex()
            .flex_row()
            .gap_5()
            .w_full()
            .child(div().flex_1().min_w_0().flex().flex_col().child(a))
            .child(div().flex_1().min_w_0().flex().flex_col().child(b))
    }
}

fn ago(t: Instant) -> String {
    let s = t.elapsed().as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s / 60) % 60)
    }
}

// -------------------------------------------------------------------- views

impl AirwaveApp {
    fn nav_state(&self) -> (bool, bool) {
        let st = self.engine.status.lock().unwrap();
        (
            st.server == ServerState::Live,
            matches!(st.player, PlayerState::Playing(_)),
        )
    }

    fn nav_items(&self) -> [(Page, &'static str, &'static str, &'static str); 4] {
        [
            (Page::Send, "nav-send", "Send", "Share this computer's sound"),
            (Page::Listen, "nav-listen", "Listen", "Play sound from another device"),
            (Page::Usb, "nav-usb", "USB", "Connect a phone by cable"),
            (Page::Settings, "nav-settings", "Settings", "Quality, delay and more"),
        ]
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (server_live, playing) = self.nav_state();
        let show_sub = self.layout == Layout::Wide;
        let items: Vec<AnyElement> = self
            .nav_items()
            .into_iter()
            .map(|(page, id, title, sub)| {
                let selected = self.page == page;
                let live = match page {
                    Page::Send => server_live,
                    Page::Listen => playing,
                    Page::Usb => !self.usb.state.lock().unwrap().aoa_running.is_empty(),
                    Page::Settings => false,
                };
                div()
                    .id(id)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(10.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(c(CARD_HI)))
                    .when(!selected, |d| d.hover(|s| s.bg(c(CARD))))
                    .child(
                        div()
                            .flex_none()
                            .w(px(4.))
                            .h(px(22.))
                            .rounded_full()
                            .bg(c(if selected { ACCENT } else { BG_SIDEBAR })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(c(if selected { TEXT } else { MUTED }))
                                    .child(title),
                            )
                            .when(show_sub, |d| {
                                d.child(div().text_xs().text_color(c(FAINT)).child(sub))
                            }),
                    )
                    .when(live, |d| d.child(dot(ACCENT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(if show_sub { 240. } else { 196. }))
            .h_full()
            .bg(c(BG_SIDEBAR))
            .border_r_1()
            .border_color(c(BORDER))
            .p_4()
            .gap_6()
            .child(brand(show_sub))
            .child(div().flex().flex_col().gap_1().children(items))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .child(label(format!("This computer: {}", self.engine.server_name)))
                    .child(div().text_xs().text_color(c(FAINT)).child("Version 0.1.0")),
            )
    }

    /// Small windows: the navigation becomes a bar across the top.
    fn topbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (server_live, playing) = self.nav_state();
        let tabs: Vec<AnyElement> = self
            .nav_items()
            .into_iter()
            .map(|(page, id, title, _)| {
                let selected = self.page == page;
                let live = match page {
                    Page::Send => server_live,
                    Page::Listen => playing,
                    Page::Usb => !self.usb.state.lock().unwrap().aoa_running.is_empty(),
                    Page::Settings => false,
                };
                div()
                    .id(id)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(32.))
                    .rounded(px(9.))
                    .cursor_pointer()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(selected, |d| d.bg(c(CARD_HI)).text_color(c(TEXT)))
                    .when(!selected, |d| {
                        d.text_color(c(MUTED)).hover(|s| s.bg(c(CARD)))
                    })
                    .child(title)
                    .when(live, |d| d.child(dot(ACCENT)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .justify_between()
            .gap_3()
            .w_full()
            .px_4()
            .py_3()
            .bg(c(BG_SIDEBAR))
            .border_b_1()
            .border_color(c(BORDER))
            .child(brand(false))
            .child(div().flex().flex_row().gap_1().children(tabs))
    }

    fn page_header(&self, title: &str, sub: &str) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_2xl()
                    .font_weight(FontWeight::BOLD)
                    .text_color(c(TEXT))
                    .child(title.to_string()),
            )
            .child(div().text_sm().text_color(c(MUTED)).child(sub.to_string()))
    }

    fn send_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let stack = self.layout != Layout::Wide;
        let st = self.engine.status.lock().unwrap();
        let state = st.server.clone();
        let format = st.server_format.clone();
        let mut listeners = st.listeners.clone();
        drop(st);
        listeners.extend(self.engine.taps.lock().unwrap().iter().map(|t| {
            crate::engine::ListenerInfo {
                label: t.name.clone(),
                since: t.since,
            }
        }));
        let st = self.engine.status.lock().unwrap();
        let note = st.discovery_note.clone();
        let inputs = st.devices.inputs.clone();
        let default_in = st.devices.default_input.clone();
        let loaded = st.devices.loaded;
        drop(st);

        let live = state == ServerState::Live;
        let ip = crate::engine::local_ip()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "no network".to_string());
        let address = format!("{ip}:{}", self.settings.port);
        let (status_color, status_text, status_sub) = match &state {
            ServerState::Stopped => (FAINT, "Not sending".to_string(), "Press Start to share this computer's sound.".to_string()),
            ServerState::Starting => (WARN, "Starting…".to_string(), "Opening the sound source.".to_string()),
            ServerState::Live => (ACCENT, "Sending".to_string(), format!("{format} · {} listening", listeners.len())),
            ServerState::Error(e) => (DANGER, "Could not start".to_string(), e.clone()),
        };
        let running = matches!(state, ServerState::Live | ServerState::Starting);

        let hero = card()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .child(dot(status_color))
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(c(TEXT))
                                            .child(status_text),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(c(if matches!(state, ServerState::Error(_)) { DANGER } else { MUTED }))
                                    .child(status_sub),
                            ),
                    )
                    .child(if running {
                        button("server-toggle", "Stop", Look::Danger, cx.listener(|this, _, _, cx| {
                            this.toggle_server();
                            cx.notify();
                        }))
                    } else {
                        button("server-toggle", "Start sending", Look::Primary, cx.listener(|this, _, _, cx| {
                            this.toggle_server();
                            cx.notify();
                        }))
                    }),
            )
            .child(
                div()
                    .flex()
                    .when(stack, |d| d.flex_col().items_start())
                    .when(!stack, |d| d.flex_row().items_center().justify_between())
                    .gap_4()
                    .p_4()
                    .rounded(px(10.))
                    .bg(c(BG_MAIN))
                    .border_1()
                    .border_color(c(BORDER))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(label("Other devices connect to"))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(c(if live { ACCENT } else { TEXT }))
                                    .child(address),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap_2()
                            .child(meter("server-meter", self.server_meter, live))
                            .child(label(if live && !listeners.is_empty() {
                                format!("{:.0} kbit/s", self.rate_kbps)
                            } else if live {
                                "Waiting for a listener".to_string()
                            } else {
                                "Sound level".to_string()
                            })),
                    ),
            )
            .when_some(note, |d, n| {
                d.child(div().text_xs().text_color(c(WARN)).child(n))
            });

        let source = card()
            .child(card_title("Sound to send", "Pick a microphone or a monitor of your speakers"))
            .child(device_list(
                "source-list",
                &inputs,
                default_in.as_ref(),
                self.settings.source_device.as_ref(),
                loaded,
                cx,
                |this, v| this.settings.source_device = v,
            ))
            .child(label(if running {
                "Changes apply next time you press Start."
            } else {
                "Tip: choose a \"Monitor\" source to send what you hear."
            }));

        let mut rows: Vec<AnyElement> = listeners
            .iter()
            .enumerate()
            .map(|(i, l)| {
                div()
                    .id(("listener", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .bg(c(BG_MAIN))
                    .child(dot(ACCENT))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(c(TEXT))
                            .child(l.label.clone()),
                    )
                    .child(label(format!("for {}", ago(l.since))))
                    .into_any_element()
            })
            .collect();
        if rows.is_empty() {
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .h(px(150.))
                    .child(div().text_sm().text_color(c(MUTED)).child("Nobody is listening yet"))
                    .child(label("Open Airwave on another device and choose Listen."))
                    .into_any_element(),
            );
        }
        let who = card()
            .child(card_title("Listeners", "Devices playing your sound right now"))
            .child(
                div()
                    .id("listener-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .h(px(196.))
                    .overflow_y_scroll()
                    .children(rows),
            );

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.page_header("Send", "Share the sound of this computer with devices on the same network."))
            .child(hero)
            .child(pair(stack, source, who))
    }

    fn listen_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stack = self.layout != Layout::Wide;
        let st = self.engine.status.lock().unwrap();
        let state = st.player.clone();
        let format = st.player_format.clone();
        let mut found = st.found.clone();
        let outputs = st.devices.outputs.clone();
        let default_out = st.devices.default_output.clone();
        let loaded = st.devices.loaded;
        drop(st);
        found.sort_by(|a, b| a.name.cmp(&b.name));

        let buffer_ms = self.engine.meters.player_buffer_ms.load(Ordering::Relaxed);
        let muted = self.engine.meters.muted.load(Ordering::Relaxed);
        let playing = matches!(state, PlayerState::Playing(_));
        let busy = matches!(state, PlayerState::Playing(_) | PlayerState::Connecting(_));
        let (status_color, status_text, status_sub) = match &state {
            PlayerState::Idle => (FAINT, "Not playing".to_string(), "Pick a server below to start listening.".to_string()),
            PlayerState::Connecting(a) => (WARN, "Connecting…".to_string(), format!("Reaching {a}")),
            PlayerState::Playing(a) => (ACCENT, format!("Playing from {a}"), format!("{format} · {buffer_ms} ms waiting")),
            PlayerState::Error(e) => (DANGER, "Stopped".to_string(), e.clone()),
        };

        let volume = self.settings.volume;
        let lit = (volume * 10.0).round() as usize;
        let volume_bar = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .child(label("Volume"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_end()
                    .gap(px(4.))
                    .children((0..10).map(|i| {
                        let on = i < lit && !muted;
                        div()
                            .id(("vol", i))
                            .w(px(14.))
                            .h(px(8. + 1.8 * i as f32))
                            .rounded(px(3.))
                            .cursor_pointer()
                            .bg(c(if on { ACCENT } else { 0x2a303c }))
                            .hover(|s| s.opacity(0.8))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_volume((i + 1) as f32 / 10.0);
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div()
                    .w(px(40.))
                    .text_sm()
                    .text_color(c(TEXT))
                    .child(if muted { "Muted".to_string() } else { format!("{}%", lit * 10) }),
            )
            .child(button(
                "mute",
                if muted { "Unmute" } else { "Mute" },
                Look::Quiet,
                cx.listener(|this, _, _, cx| {
                    let m = &this.engine.meters.muted;
                    m.store(!m.load(Ordering::Relaxed), Ordering::Relaxed);
                    cx.notify();
                }),
            ));

        let hero = card()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .child(dot(status_color))
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(c(TEXT))
                                            .child(status_text),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(c(if matches!(state, PlayerState::Error(_)) { DANGER } else { MUTED }))
                                    .child(status_sub),
                            ),
                    )
                    .when(busy, |d| {
                        d.child(button("disconnect", "Disconnect", Look::Danger, cx.listener(|this, _, _, cx| {
                            this.engine.disconnect();
                            cx.notify();
                        })))
                    }),
            )
            .child(
                div()
                    .flex()
                    .when(stack, |d| d.flex_col().items_start())
                    .when(!stack, |d| d.flex_row().items_center().justify_between())
                    .gap_4()
                    .p_4()
                    .rounded(px(10.))
                    .bg(c(BG_MAIN))
                    .border_1()
                    .border_color(c(BORDER))
                    .child(volume_bar)
                    .child(meter("player-meter", self.player_meter, playing)),
            );

        // Servers found on the network.
        let mut rows: Vec<AnyElement> = found
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let addr = f.addr.to_string();
                let is_current = matches!(&state, PlayerState::Playing(a) | PlayerState::Connecting(a) if *a == addr);
                div()
                    .id(("found", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .bg(c(BG_MAIN))
                    .child(
                        div()
                            .flex_none()
                            .size(px(30.))
                            .rounded(px(8.))
                            .bg(c(ACCENT_DARK))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(c(ACCENT))
                            .child(f.name.chars().next().unwrap_or('?').to_uppercase().to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(c(TEXT))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(f.name.clone()),
                            )
                            .child(label(addr.clone())),
                    )
                    .child(if is_current {
                        div().text_xs().text_color(c(ACCENT)).child("Connected").into_any_element()
                    } else {
                        button(("connect", i).into_shared(), "Listen", Look::Primary, cx.listener(move |this, _, _, cx| {
                            this.connect_to(addr.clone());
                            cx.notify();
                        }))
                        .into_any_element()
                    })
                    .into_any_element()
            })
            .collect();
        if rows.is_empty() {
            rows.push(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .h(px(150.))
                    .child(div().text_sm().text_color(c(MUTED)).child("Searching for servers…"))
                    .child(label("Start sending on another computer running Airwave."))
                    .into_any_element(),
            );
        }
        let nearby = card()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .justify_between()
                    .child(card_title("Servers nearby", "Found on your network"))
                    .child(button("rescan", "Search again", Look::Quiet, cx.listener(|this, _, _, cx| {
                        this.engine.scan_now.store(true, Ordering::Relaxed);
                        cx.notify();
                    }))),
            )
            .child(
                div()
                    .id("found-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .h(px(150.))
                    .overflow_y_scroll()
                    .children(rows),
            );

        // Manual address entry.
        let focused = self.address_focus.is_focused(window);
        let input = div()
            .id("address-input")
            .track_focus(&self.address_focus)
            .flex()
            .flex_row()
            .items_center()
            .flex_1()
            .min_w_0()
            .h(px(36.))
            .px_3()
            .rounded(px(9.))
            .bg(c(BG_MAIN))
            .border_1()
            .border_color(c(if focused { ACCENT } else { BORDER }))
            .text_sm()
            .cursor_text()
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.address_focus);
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                this.on_address_key(ev, cx);
            }))
            .child(if self.address.is_empty() && !focused {
                div().text_color(c(FAINT)).child("e.g. 192.168.1.20").into_any_element()
            } else {
                div().text_color(c(TEXT)).child(self.address.clone()).into_any_element()
            })
            .when(focused, |d| {
                d.child(div().w(px(2.)).h(px(16.)).ml(px(1.)).bg(c(ACCENT)))
            });
        let manual = card()
            .child(card_title("Connect by address", "If a server isn't found automatically, type its address"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(input)
                    .child(button("connect-manual", "Connect", Look::Primary, cx.listener(move |this, _, _, cx| {
                        let a = this.address.clone();
                        this.connect_to(a);
                        cx.notify();
                    }))),
            );

        let output = card()
            .child(card_title("Play through", "Tick one or more speakers to play on all of them at once"))
            .child(multi_device_list(
                "output-list",
                &outputs,
                default_out.as_ref(),
                &self.settings.output_devices,
                loaded,
                cx,
            ));

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.page_header("Listen", "Play sound sent from another computer on your network."))
            .child(hero)
            .child(pair(
                stack,
                div().flex().flex_col().gap_5().child(nearby).child(manual),
                output,
            ))
    }

    fn usb_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let stack = self.layout != Layout::Wide;
        let port = self.settings.port;
        let st = self.usb.state.lock().unwrap();
        let adb_path = st.adb_path.clone();
        let adb_devices = st.adb_devices.clone();
        let linked = st.adb_linked.clone();
        let adb_note = st.adb_note.clone();
        let phones = st.phones.clone();
        let aoa_running = st.aoa_running.clone();
        let aoa_note = st.aoa_note.clone();
        let scanned = st.scanned;
        drop(st);
        let server_on = self.engine.server_running();

        let note_el = |n: Option<crate::usb::Note>| {
            n.map(|n| {
                div()
                    .text_xs()
                    .text_color(c(if n.error { DANGER } else { ACCENT }))
                    .child(n.text)
            })
        };
        let empty = |title: &str, hint: &str| {
            div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px_4()
                .h(px(110.))
                .rounded(px(8.))
                .bg(c(BG_MAIN))
                .child(div().text_sm().text_color(c(MUTED)).child(title.to_string()))
                .child(label(hint.to_string()))
                .into_any_element()
        };

        // ---- adb
        let mut adb_rows: Vec<AnyElement> = Vec::new();
        for (i, d) in adb_devices.iter().enumerate() {
            let is_linked = linked.contains(&d.serial);
            let ready = d.state == "device";
            let serial = d.serial.clone();
            let serial2 = d.serial.clone();
            adb_rows.push(
                div()
                    .id(("adb", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .bg(c(BG_MAIN))
                    .child(dot(if is_linked { ACCENT } else if ready { MUTED } else { WARN }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().text_color(c(TEXT)).child(d.model.clone()))
                            .child(label(if ready {
                                if is_linked {
                                    format!("Linked · phone uses 127.0.0.1:{port}")
                                } else {
                                    d.serial.clone()
                                }
                            } else {
                                format!("{} – allow USB debugging on the phone", d.state)
                            })),
                    )
                    .when(ready && !is_linked, |row| {
                        row.child(button(("adb-link", i).into_shared(), "Link", Look::Primary, cx.listener(move |this, _, _, cx| {
                            this.usb.adb_link(serial.clone(), this.settings.port);
                            cx.notify();
                        })))
                    })
                    .when(is_linked, |row| {
                        row.child(button(("adb-listen", i).into_shared(), "Listen to phone", Look::Quiet, cx.listener(|this, _, _, cx| {
                            let addr = format!("127.0.0.1:{}", this.settings.port + FORWARD_OFFSET);
                            this.engine.connect(addr, this.settings.output_devices.clone());
                            this.page = Page::Listen;
                            cx.notify();
                        })))
                        .child(button(("adb-unlink", i).into_shared(), "Unlink", Look::Danger, cx.listener(move |this, _, _, cx| {
                            this.usb.adb_unlink(serial2.clone(), this.settings.port);
                            cx.notify();
                        })))
                    })
                    .into_any_element(),
            );
        }
        if adb_rows.is_empty() {
            adb_rows.push(match &adb_path {
                None if scanned => empty("adb was not found", "Install Android platform-tools and put adb on your PATH."),
                None => empty("Looking for adb…", ""),
                Some(_) => empty("No phone found", "Plug in a phone with USB debugging turned on."),
            });
        }
        let adb_card = card()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(card_title("USB debugging (adb)", "Links phone and computer with adb forward / reverse"))
                    .child(button("usb-refresh", "Look again", Look::Quiet, cx.listener(|this, _, _, cx| {
                        this.usb.refresh();
                        this.usb_refreshed = Instant::now();
                        cx.notify();
                    }))),
            )
            .child(div().flex().flex_col().gap_1().children(adb_rows))
            .child(label(format!(
                "Computer to phone: start sending, the phone connects to 127.0.0.1:{port}. Phone to computer: the phone sends, this computer listens at 127.0.0.1:{}.",
                port + FORWARD_OFFSET
            )))
            .children(note_el(adb_note));

        // ---- accessory mode
        let mut aoa_rows: Vec<AnyElement> = Vec::new();
        for (i, p) in phones.iter().enumerate() {
            let (bus, addr) = (p.bus, p.address);
            let sending_here = aoa_running.contains(&(bus, addr));
            aoa_rows.push(
                div()
                    .id(("phone", i))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded(px(8.))
                    .bg(c(BG_MAIN))
                    .child(dot(if p.accessory { ACCENT } else { MUTED }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().text_color(c(TEXT)).child(p.name.clone()))
                            .child(label(if p.accessory {
                                "In accessory mode".to_string()
                            } else {
                                format!("{:04x}:{:04x} · bus {} device {}", p.vid, p.pid, p.bus, p.address)
                            })),
                    )
                    .when(!p.accessory, |row| {
                        row.child(button(("aoa-switch", i).into_shared(), "Use accessory mode", Look::Quiet, cx.listener(move |this, _, _, cx| {
                            this.usb.switch_to_accessory(bus, addr);
                            cx.notify();
                        })))
                    })
                    .when(p.accessory && !sending_here, |row| {
                        row.child(button(("aoa-send", i).into_shared(), "Send sound", Look::Primary, cx.listener(move |this, _, _, cx| {
                            if !this.engine.server_running() {
                                this.toggle_server();
                            }
                            let taps = this.engine.taps.clone();
                            this.usb.start_accessory_stream(bus, addr, taps);
                            cx.notify();
                        })))
                    })
                    .when(p.accessory && sending_here, |row| {
                        row.child(button(("aoa-stop", i).into_shared(), "Stop", Look::Danger, cx.listener(move |this, _, _, cx| {
                            this.usb.stop_accessory_stream(bus, addr);
                            cx.notify();
                        })))
                    })
                    .into_any_element(),
            );
        }
        if aoa_rows.is_empty() {
            aoa_rows.push(if scanned {
                empty("No phone found on USB", "Plug in an Android phone. USB debugging is not needed.")
            } else {
                empty("Looking for phones…", "")
            });
        }
        let aoa_card = card()
            .child(card_title("Accessory mode", "Sends this computer's sound straight down the cable"))
            .child(div().flex().flex_col().gap_1().children(aoa_rows))
            .child(label(if server_on {
                "Sending is on: the phone gets the same sound as network listeners."
            } else {
                "Sending starts by itself when you press Send sound."
            }))
            .children(note_el(aoa_note));

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.page_header("USB", "Use a cable instead of Wi-Fi: steadier sound and less delay."))
            .child(pair(stack, adb_card, aoa_card))
    }

    fn settings_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let stack = self.layout == Layout::Narrow;
        let row = |title: &str, hint: &str, control: AnyElement| {
            div()
                .flex()
                .when(stack, |d| d.flex_col().items_start())
                .when(!stack, |d| d.flex_row().items_center().justify_between())
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_sm().text_color(c(TEXT)).child(title.to_string()))
                        .child(label(hint.to_string())),
                )
                .child(control)
        };
        let sep = || div().h(px(1.)).bg(c(BORDER));

        let sending = card()
            .gap_4()
            .child(card_title("Sending", ""))
            .child(row(
                "Sound quality",
                "Mono uses half the network data",
                segmented(
                    "quality",
                    &[(Quality::Stereo, "Stereo"), (Quality::Mono, "Mono")],
                    self.settings.quality,
                    cx,
                    |this, v| this.settings.quality = v,
                )
                .into_any_element(),
            ))
            .child(sep())
            .child(row(
                "Compression",
                "Light halves the data, Strong uses a quarter. Helps on weak Wi-Fi.",
                segmented(
                    "codec",
                    &[
                        (Codec::Pcm, Codec::Pcm.label()),
                        (Codec::MuLaw, Codec::MuLaw.label()),
                        (Codec::Adpcm, Codec::Adpcm.label()),
                    ],
                    self.settings.codec,
                    cx,
                    |this, v| this.settings.codec = v,
                )
                .into_any_element(),
            ))
            .child(sep())
            .child(row(
                "Network port",
                "Other devices use this number with your address",
                div()
                    .px_3()
                    .py_1()
                    .rounded(px(8.))
                    .bg(c(BG_MAIN))
                    .border_1()
                    .border_color(c(BORDER))
                    .text_sm()
                    .text_color(c(TEXT))
                    .child(self.settings.port.to_string())
                    .into_any_element(),
            ))
            .child(sep())
            .child(toggle(
                "auto-start",
                self.settings.auto_start_server,
                "Start sending when the app opens",
                "Share your sound right away every time",
                cx,
                |this| this.settings.auto_start_server = !this.settings.auto_start_server,
            ));

        let listening = card()
            .gap_4()
            .child(card_title("Listening", ""))
            .child(row(
                "Buffer",
                "How much sound waits before playing. Lower = less delay, higher = fewer dropouts.",
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(segmented(
                        "buffer-preset",
                        &[(40u32, "Low"), (100u32, "Balanced"), (250u32, "Stable")],
                        self.settings.buffer_ms,
                        cx,
                        |this, v| this.set_buffer(v),
                    ))
                    .child(button("buffer-less", "−", Look::Quiet, cx.listener(|this, _, _, cx| {
                        let step = if this.settings.buffer_ms > 200 { 50 } else { 10 };
                        this.set_buffer(this.settings.buffer_ms.saturating_sub(step));
                        cx.notify();
                    })))
                    .child(
                        div()
                            .w(px(64.))
                            .flex()
                            .justify_center()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(c(TEXT))
                            .child(format!("{} ms", self.settings.buffer_ms)),
                    )
                    .child(button("buffer-more", "+", Look::Quiet, cx.listener(|this, _, _, cx| {
                        let step = if this.settings.buffer_ms >= 200 { 50 } else { 10 };
                        this.set_buffer(this.settings.buffer_ms + step);
                        cx.notify();
                    })))
                    .into_any_element(),
            ))
            .child(sep())
            .child(toggle(
                "auto-scan",
                self.settings.auto_scan,
                "Look for servers automatically",
                "Keeps the Servers nearby list up to date",
                cx,
                |this| {
                    this.settings.auto_scan = !this.settings.auto_scan;
                    this.engine
                        .auto_scan
                        .store(this.settings.auto_scan, Ordering::Relaxed);
                },
            ));

        let devices = card()
            .gap_4()
            .child(row(
                "Sound devices",
                "Plugged in a headset or microphone? Look again.",
                button("refresh-devices", "Refresh list", Look::Quiet, cx.listener(|this, _, _, cx| {
                    this.engine.status.lock().unwrap().devices.loaded = false;
                    this.engine.refresh_devices();
                    cx.notify();
                }))
                .into_any_element(),
            ))
            .child(sep())
            .child(row(
                "About",
                "Airwave sends sound between computers that run Airwave on the same network.",
                div().into_any_element(),
            ));

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_5()
            .child(self.page_header("Settings", "Choices are saved automatically."))
            .child(sending)
            .child(listening)
            .child(devices)
    }
}

trait IntoShared {
    fn into_shared(self) -> SharedString;
}
impl IntoShared for (&'static str, usize) {
    fn into_shared(self) -> SharedString {
        format!("{}-{}", self.0, self.1).into()
    }
}

fn brand(with_tagline: bool) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .px_1()
        .child(logo())
        .child(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .text_color(c(TEXT))
                        .child("Airwave"),
                )
                .when(with_tagline, |d| d.child(label("Sound across your network"))),
        )
}

fn logo() -> impl IntoElement {
    let heights = [10., 18., 26., 16., 22., 12.];
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .gap(px(2.))
        .size(px(40.))
        .rounded(px(11.))
        .bg(c(ACCENT_DARK))
        .border_1()
        .border_color(c(0x24605a))
        .children(
            heights
                .into_iter()
                .map(|h| div().w(px(3.)).h(px(h)).rounded_full().bg(c(ACCENT))),
        )
}

impl Render for AirwaveApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(shot) = &mut self.shot {
            shot.frames += 1;
            window.request_animation_frame();
            match shot.result.load(Ordering::SeqCst) {
                SHOT_OK => {
                    use std::io::Write as _;
                    println!("saved {}", shot.path.display());
                    let _ = std::io::stdout().flush();
                    // Leave at once: some graphics/sound drivers crash while
                    // tearing down, and the picture is already saved.
                    #[cfg(unix)]
                    unsafe {
                        libc::_exit(0)
                    };
                    #[cfg(not(unix))]
                    cx.quit();
                }
                SHOT_FAILED => {
                    eprintln!("screenshot failed");
                    std::process::exit(1);
                }
                _ => {}
            }
            if shot.frames >= 120 && !shot.started {
                shot.started = true;
                let path = shot.path.clone();
                let title = shot.title.clone();
                let result = shot.result.clone();
                std::thread::spawn(move || {
                    let mut last_err = None;
                    for _ in 0..40 {
                        match crate::shot::capture_window(&title, &path) {
                            Ok(()) => {
                                result.store(SHOT_OK, Ordering::SeqCst);
                                return;
                            }
                            Err(e) => last_err = Some(e),
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    eprintln!("{:#}", last_err.unwrap());
                    result.store(SHOT_FAILED, Ordering::SeqCst);
                });
            }
        }

        let width = window.viewport_size().width;
        self.layout = if width < px(720.) {
            Layout::Narrow
        } else if width < px(1000.) {
            Layout::Compact
        } else {
            Layout::Wide
        };

        let page: AnyElement = match self.page {
            Page::Send => self.send_page(cx).into_any_element(),
            Page::Listen => self.listen_page(window, cx).into_any_element(),
            Page::Usb => self.usb_page(cx).into_any_element(),
            Page::Settings => self.settings_page(cx).into_any_element(),
        };

        let narrow = self.layout == Layout::Narrow;
        let content = div()
            .id("page-scroll")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .when(narrow, |d| d.w_full())
            .when(!narrow, |d| d.h_full())
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .when(narrow, |d| d.p_5())
                    .when(!narrow, |d| d.p_8())
                    .child(page),
            );

        if narrow {
            div()
                .flex()
                .flex_col()
                .size_full()
                .bg(c(BG_MAIN))
                .text_color(c(TEXT))
                .child(self.topbar(cx))
                .child(content)
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_row()
                .size_full()
                .bg(c(BG_MAIN))
                .text_color(c(TEXT))
                .child(self.sidebar(cx))
                .child(content)
                .into_any_element()
        }
    }
}
