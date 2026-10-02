// Drag the Navi character across the desktop and read the window it is dropped
// on, mirroring the macOS "drag-attach" (see docs/SPEC.md §8).
//
// The drag itself is driven from `island.rs`'s 60 Hz cursor poll — the island is
// click-through, so the DOM would stop receiving mouse events the moment the
// cursor leaves it. The poll only starts a drag when a press begins on the bot
// (the front end pushes the bot's rectangle via `set_bot_rect`); a press that is
// released without moving is forwarded back as `bot-tap` so the island can keep
// its click/slap behaviour.
//
// While a drag is live a dedicated transparent, click-through overlay window
// follows the cursor: it draws the floating character and a highlight around the
// window underneath. Because the overlay is WS_EX_TRANSPARENT, `WindowFromPoint`
// ignores it, so it can never detect itself.

use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, POINT, RECT};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, WindowFromPoint, GA_ROOT,
};

use crate::island::{self, PollGate, WINDOW_LABEL};

/// Window label of the drag overlay (declared in tauri.conf.json).
pub const DRAG_LABEL: &str = "drag";

/// How far (physical px) the press must move before it becomes a drag.
const DRAG_THRESHOLD: f64 = 7.0;

/// The window the character was dropped on, as the island needs it.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DraggedWindow {
    pub app: String,
    pub title: String,
}

/// `drag-end` payload: the attached window, or `None` when dropped on nothing.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DragEnd {
    pub context: Option<DraggedWindow>,
}

/// `drag-hover` payload, in overlay-local logical pixels.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DragHover {
    x: f64,
    y: f64,
    app: String,
    title: String,
    /// [x, y, w, h] of the highlighted window, or null when over nothing.
    rect: Option<[f64; 4]>,
}

struct WindowInfo {
    app: String,
    title: String,
    /// Screen rect in physical pixels: (left, top, width, height).
    rect: (i32, i32, i32, i32),
}

impl WindowInfo {
    fn describe(&self) -> DraggedWindow {
        DraggedWindow {
            app: self.app.clone(),
            title: self.title.clone(),
        }
    }
}

/// One poll tick's pointer snapshot. `wx`/`wy` are window-logical (for the bot
/// hit box), `cx`/`cy` physical screen (for the overlay and hover).
pub struct Pointer {
    pub wx: f64,
    pub wy: f64,
    pub cx: f64,
    pub cy: f64,
    pub down: bool,
    pub was_down: bool,
}

pub fn tick(app: &AppHandle, gate: &PollGate, p: Pointer) {
    let Pointer {
        wx,
        wy,
        cx,
        cy,
        down,
        was_down,
    } = p;

    // A fresh press only arms a drag if it landed on the bot.
    if down && !was_down {
        let bot = *gate.bot_rect.lock().unwrap();
        let over_bot = bot.w > 0.0
            && wx >= bot.x
            && wx <= bot.x + bot.w
            && wy >= bot.y
            && wy <= bot.y + bot.h;
        *gate.press.lock().unwrap() = over_bot.then_some((cx, cy));
    }

    let dragging = gate.dragging.load(Ordering::Relaxed);

    // Escape, or the island's cancel command, aborts without attaching anything.
    if dragging && (gate.cancel_drag.swap(false, Ordering::Relaxed) || escape_down()) {
        finish(app, gate, None);
        return;
    }
    gate.cancel_drag.store(false, Ordering::Relaxed);

    // Promote a press to a drag once it moves past the threshold.
    if down && !dragging {
        if let Some((sx, sy)) = *gate.press.lock().unwrap() {
            if ((cx - sx).powi(2) + (cy - sy).powi(2)).sqrt() > DRAG_THRESHOLD {
                gate.dragging.store(true, Ordering::Relaxed);
                show_overlay(app);
                let _ = app.emit_to(WINDOW_LABEL, "drag-start", ());
            }
        }
    }

    if gate.dragging.load(Ordering::Relaxed) {
        update_overlay(app, cx, cy);
    }

    if !down && was_down {
        let was_bot_press = gate.press.lock().unwrap().is_some();
        *gate.press.lock().unwrap() = None;
        if gate.dragging.swap(false, Ordering::Relaxed) {
            let context = window_at_point(cx, cy).map(|w| w.describe());
            hide_overlay(app);
            let _ = app.emit_to(WINDOW_LABEL, "drag-end", DragEnd { context });
        } else if was_bot_press {
            // A press and release on the bot with no movement: let the island
            // decide (slap when expanded, open when compact).
            let _ = app.emit_to(WINDOW_LABEL, "bot-tap", ());
        }
    }
}

fn finish(app: &AppHandle, gate: &PollGate, context: Option<DraggedWindow>) {
    gate.dragging.store(false, Ordering::Relaxed);
    *gate.press.lock().unwrap() = None;
    hide_overlay(app);
    let _ = app.emit_to(WINDOW_LABEL, "drag-end", DragEnd { context });
}

/// Called by the `cancel_drag` command.
pub fn request_cancel(gate: &PollGate) {
    gate.cancel_drag.store(true, Ordering::Relaxed);
}

fn overlay(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(DRAG_LABEL)
}

fn show_overlay(app: &AppHandle) {
    let Some(win) = overlay(app) else { return };
    if let Some(m) = island::monitor_under_cursor(app) {
        let p = m.position();
        let s = m.size();
        let _ = win.set_position(PhysicalPosition::new(p.x, p.y));
        let _ = win.set_size(PhysicalSize::new(s.width, s.height));
    }
    let _ = win.set_always_on_top(true);
    let _ = win.show();
    // Start the overlay's animation loop only while it is on screen.
    let _ = win.emit("drag-show", ());
}

fn hide_overlay(app: &AppHandle) {
    if let Some(win) = overlay(app) {
        let _ = win.emit("drag-hide", ());
        let _ = win.hide();
    }
}

fn update_overlay(app: &AppHandle, cx: f64, cy: f64) {
    let Some(win) = overlay(app) else { return };
    let Some(m) = island::monitor_under_cursor(app) else {
        return;
    };
    let p = m.position();
    let s = m.size();
    let scale = m.scale_factor();

    // Follow the cursor when it crosses to another display.
    let moved_monitor = win
        .outer_position()
        .map(|o| o.x != p.x || o.y != p.y)
        .unwrap_or(true);
    if moved_monitor {
        let _ = win.set_position(PhysicalPosition::new(p.x, p.y));
        let _ = win.set_size(PhysicalSize::new(s.width, s.height));
    }

    let info = window_at_point(cx, cy);
    let rect = info.as_ref().map(|w| {
        [
            (w.rect.0 - p.x) as f64 / scale,
            (w.rect.1 - p.y) as f64 / scale,
            w.rect.2 as f64 / scale,
            w.rect.3 as f64 / scale,
        ]
    });
    let payload = DragHover {
        x: (cx - p.x as f64) / scale,
        y: (cy - p.y as f64) / scale,
        app: info.as_ref().map(|w| w.app.clone()).unwrap_or_default(),
        title: info.as_ref().map(|w| w.title.clone()).unwrap_or_default(),
        rect,
    };
    let _ = win.emit("drag-hover", payload);
}

fn escape_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000) != 0 }
}

/// The top-level window under a physical screen point, described for the prompt.
/// Skips Navi's own windows, and windows with no title (the desktop, the shell).
fn window_at_point(x: f64, y: f64) -> Option<WindowInfo> {
    unsafe {
        let pt = POINT {
            x: x as i32,
            y: y as i32,
        };
        let mut hwnd = WindowFromPoint(pt);
        if hwnd.0.is_null() {
            return None;
        }
        // WindowFromPoint can return a child control; the app window is the root.
        let root = GetAncestor(hwnd, GA_ROOT);
        if !root.0.is_null() {
            hwnd = root;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return None;
        }

        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len <= 0 {
            return None;
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        if title.trim().is_empty() {
            return None;
        }

        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return None;
        }
        let w = r.right - r.left;
        let h = r.bottom - r.top;
        if w <= 0 || h <= 0 {
            return None;
        }

        Some(WindowInfo {
            app: process_name(pid).unwrap_or_default(),
            title,
            rect: (r.left, r.top, w, h),
        })
    }
}

/// Friendly app name from a process id, via its executable name.
fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok || len == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let stem = std::path::Path::new(&path)
            .file_stem()?
            .to_string_lossy()
            .to_string();
        Some(pretty_app(&stem))
    }
}

/// "chrome" → "Chrome", "notepad" → "Notepad".
fn pretty_app(stem: &str) -> String {
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_app_capitalises() {
        assert_eq!(pretty_app("chrome"), "Chrome");
        assert_eq!(pretty_app("notepad"), "Notepad");
        assert_eq!(pretty_app(""), "");
    }
}
