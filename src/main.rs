//! Airwave: send this computer's sound to other devices on the network,
//! or play sound that another computer sends. Built with gpui.

mod codec;
mod engine;
mod settings;
mod shot;
mod ui;
mod usb;

use std::path::PathBuf;

use gpui::{
    px, size, App, AppContext as _, Application, Bounds, TitlebarOptions, WindowBounds,
    WindowOptions,
};

fn main() {
    let mut screenshot: Option<PathBuf> = None;
    let mut start_page: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--screenshot" {
            screenshot = args.next().map(PathBuf::from);
            if screenshot.is_none() {
                eprintln!("--screenshot needs a file name, e.g. --screenshot shot.png");
                std::process::exit(2);
            }
        } else if a == "--page" {
            start_page = args.next();
        } else if let Some(p) = a.strip_prefix("--screenshot=") {
            screenshot = Some(PathBuf::from(p));
        }
    }

    if screenshot.is_some() {
        shot::prepare_platform();
        // Safety net: never hang around if the picture can't be taken.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(90));
            eprintln!("screenshot timed out");
            std::process::exit(1);
        });
    }

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1040.), px(720.)), cx);
        let title = match &screenshot {
            Some(_) => shot::unique_title(),
            None => "Airwave".to_string(),
        };
        let shot_state = screenshot.clone().map(|path| ui::ShotState {
            result: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(ui::SHOT_PENDING)),
            path,
            title: title.clone(),
            frames: 0,
            started: false,
        });
        let focus = shot_state.is_none();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..Default::default()
                }),
                focus,
                window_min_size: Some(size(px(860.), px(600.))),
                app_id: Some("airwave".into()),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| ui::AirwaveApp::new(shot_state, start_page, cx)),
        )
        .expect("could not open the window");
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if focus {
            cx.activate(true);
        }
    });
}
