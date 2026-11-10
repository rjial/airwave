//! `--screenshot <file.png>`: save a picture of our own window and exit.
//!
//! gpui has no "read back the frame" call, so on Linux the app runs on X11
//! (XWayland on Wayland desktops) and asks the X server for the pixels of
//! its own window, found by its unique title.

use std::path::Path;

use anyhow::{bail, Result};

pub fn unique_title() -> String {
    format!("Airwave [capture {}]", std::process::id())
}

/// Call before gpui starts: makes gpui pick X11 so the window can be read.
pub fn prepare_platform() {
    #[cfg(target_os = "linux")]
    {
        let has_x = std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty());
        if has_x {
            // SAFETY: called at the very start of main, before any threads exist.
            unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
        }
    }
}

#[cfg(target_os = "linux")]
pub fn capture_window(title: &str, out: &Path) -> Result<()> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, ImageFormat, Window};

    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;
    let net_wm_name = conn.intern_atom(false, b"_NET_WM_NAME")?.reply()?.atom;
    let utf8 = conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom;

    let title_of = |w: Window| -> Option<String> {
        let r = conn
            .get_property(false, w, net_wm_name, utf8, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        if !r.value.is_empty() {
            return Some(String::from_utf8_lossy(&r.value).into_owned());
        }
        let r = conn
            .get_property(false, w, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        if r.value.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&r.value).into_owned())
        }
    };

    // Walk the window tree looking for our title.
    let mut stack = vec![root];
    let mut found = None;
    while let Some(w) = stack.pop() {
        if title_of(w).as_deref() == Some(title) {
            found = Some(w);
            break;
        }
        if let Ok(c) = conn.query_tree(w) {
            if let Ok(r) = c.reply() {
                stack.extend(r.children);
            }
        }
    }
    let Some(win) = found else {
        bail!("could not find the app window");
    };

    let geo = conn.get_geometry(win)?.reply()?;
    let (w, h) = (geo.width, geo.height);
    let img = conn
        .get_image(ImageFormat::Z_PIXMAP, win, 0, 0, w, h, !0)?
        .reply()?;
    let depth = img.depth;
    let data = img.data;
    let px = (w as usize) * (h as usize);
    if depth < 24 || data.len() < px * 4 {
        bail!(
            "unsupported window format (depth {depth}, {} bytes)",
            data.len()
        );
    }
    let stride = data.len() / h as usize;
    let mut rgba = Vec::with_capacity(px * 4);
    for y in 0..h as usize {
        let row = &data[y * stride..];
        for x in 0..w as usize {
            let p = &row[x * 4..x * 4 + 4];
            // X11 ZPixmap on little-endian: B, G, R, X
            rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    if rgba.chunks(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0) {
        bail!("window picture is still blank");
    }
    if let Some(dir) = out.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    image::save_buffer(out, &rgba, w as u32, h as u32, image::ColorType::Rgba8)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn capture_window(_title: &str, _out: &Path) -> Result<()> {
    bail!("--screenshot is only supported on Linux in this version")
}
