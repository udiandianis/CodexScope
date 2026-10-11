mod account_usage;
mod model;
mod parser;
mod pricing;
mod store;

use model::{Dashboard, PeriodReport};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(not(target_os = "macos"))]
use tauri::WindowEvent;
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
#[cfg(not(target_os = "macos"))]
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri_plugin_autostart::ManagerExt;
// Positioner is only used for the non-macOS fallback; macOS positions the
// NSPanel manually (see position_panel).
#[cfg(not(target_os = "macos"))]
use tauri_plugin_positioner::{Position, WindowExt};
// NSPanel: lets the popover float over apps in native fullscreen (a plain
// NSWindow from a background/Accessory app cannot overlay another app's
// fullscreen Space). `get_webview_panel` / `to_panel` come from these traits.
#[cfg(target_os = "macos")]
use tauri_nspanel::{ManagerExt as _, WebviewWindowExt as _};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn tray_title_for_platform(label: &str) -> Option<String> {
    // Notched MacBook menu bars have very little room to the right of the
    // notch. A titled status item is often hidden by macOS when the user has
    // many menu extras, so keep the macOS item icon-only and put the count in
    // the tooltip/panel.
    #[cfg(target_os = "macos")]
    {
        let _ = label;
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(label.to_string())
    }
}

/// Rebuild the dashboard (incremental), update the tray's token count, and push
/// the fresh data to the UI so an open popover updates live.
fn refresh(app: &tauri::AppHandle) {
    let dash = parser::build_dashboard();
    if let Some(tray) = app.tray_by_id("main") {
        let label = fmt_tokens_m(dash.today_tokens);
        let _ = tray.set_title(tray_title_for_platform(&label));
        let _ = tray.set_tooltip(Some(format!("CodexScope · 今日 {}", label)));
    }
    check_milestones(app, &dash);
    let _ = app.emit("dashboard-updated", &dash);
}

/// Persisted 100M-token milestone snapshot. Stored in the app *data* dir so it
/// survives app restarts, reboots, and updates (which only replace the .app
/// bundle, never the data dir). The per-period ids let us tell a real crossing
/// from a period reset.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct MilestoneState {
    week_id: String,
    week_floor: i64,
    month_id: String,
    month_floor: i64,
}

/// 100M-token celebration tracking. `state` is the last persisted snapshot
/// (`None` only before the very first observation ever, so the first run
/// baselines without celebrating pre-existing usage). `active` guards against
/// overlapping celebrations.
struct Celebration {
    state: std::sync::Mutex<Option<MilestoneState>>,
    active: AtomicBool,
}

/// User-controlled pin state. The popover is already rendered above normal
/// windows; pinning means it stays visible when focus or desktop context changes.
struct PinnedState(AtomicBool);

/// `~/Library/Application Support/codexscope/milestones.json` (platform
/// equivalent elsewhere). Deliberately the data dir, not the Caches dir the
/// event store uses — Caches can be purged by the OS, milestones must not be.
fn milestones_path() -> Option<std::path::PathBuf> {
    let dir = dirs::data_dir()?.join("codexscope");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("milestones.json"))
}

fn load_milestones() -> Option<MilestoneState> {
    let t = std::fs::read_to_string(milestones_path()?).ok()?;
    serde_json::from_str(&t).ok()
}

fn save_milestones(m: &MilestoneState) {
    if let Some(p) = milestones_path() {
        if let Ok(t) = serde_json::to_string(m) {
            let _ = std::fs::write(p, t);
        }
    }
}

// ── Launch-at-login preference ──────────────────────────────────────
// Persisted in the data dir (survives restarts/updates, like milestones). The
// on/off toggle lives in the tray's right-click menu; on startup we reconcile
// the OS registration to this preference rather than force-enabling every
// launch (which silently undid a user who had turned autostart off).
fn autostart_pref_path() -> Option<std::path::PathBuf> {
    let dir = dirs::data_dir()?.join("codexscope");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("autostart.json"))
}

fn load_autostart_pref() -> Option<bool> {
    let t = std::fs::read_to_string(autostart_pref_path()?).ok()?;
    serde_json::from_str(&t).ok()
}

fn save_autostart_pref(on: bool) {
    if let Some(p) = autostart_pref_path() {
        if let Ok(t) = serde_json::to_string(&on) {
            let _ = std::fs::write(p, t);
        }
    }
}

fn pinned_pref_path() -> Option<std::path::PathBuf> {
    let dir = dirs::data_dir()?.join("codexscope");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("pinned.json"))
}

fn load_pinned_pref() -> bool {
    std::fs::read_to_string(pinned_pref_path().unwrap_or_default())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(false)
}

fn save_pinned_pref(on: bool) {
    if let Some(p) = pinned_pref_path() {
        if let Ok(t) = serde_json::to_string(&on) {
            let _ = std::fs::write(p, t);
        }
    }
}

/// Bring the OS launch-at-login registration in line with the saved preference,
/// returning the effective preference (used to seed the menu checkbox). First
/// run (no saved pref) defaults to on and records it; thereafter we honor the
/// user's choice and only touch the registration when it actually differs.
fn reconcile_autostart(app: &tauri::AppHandle) -> bool {
    let pref = match load_autostart_pref() {
        Some(p) => p,
        None => {
            save_autostart_pref(true);
            true
        }
    };
    let mgr = app.autolaunch();
    let cur = mgr.is_enabled().unwrap_or(false);
    if pref && !cur {
        let _ = mgr.enable();
    } else if !pref && cur {
        let _ = mgr.disable();
    }
    pref
}

/// Current calendar-week and calendar-month identifiers, matching parser.rs's
/// period definitions (Monday-based week, calendar month), so a stored floor is
/// only ever compared within the same period.
fn period_ids() -> (String, String) {
    use chrono::Datelike;
    let d = chrono::Local::now().date_naive();
    let iso = d.iso_week();
    (
        format!("{}-W{:02}", iso.year(), iso.week()),
        format!("{}-{:02}", d.year(), d.month()),
    )
}

/// Decide whether to celebrate: fire if either period advanced to a higher
/// 100M floor *within the same period*. `None` (first ever observation) never
/// fires. A period-id mismatch means that period reset, so it re-baselines
/// silently rather than comparing floors. Returns a single bool, so a jump
/// across several boundaries — or week and month advancing together — is one
/// celebration.
fn milestone_fire(prev: Option<&MilestoneState>, cur: &MilestoneState) -> bool {
    match prev {
        None => false,
        Some(p) => {
            (p.week_id == cur.week_id && cur.week_floor > p.week_floor)
                || (p.month_id == cur.month_id && cur.month_floor > p.month_floor)
        }
    }
}

/// Observe the latest totals, persist the snapshot, and celebrate on a new
/// 100M-token milestone. We watch week ∪ month, not day: today is always within
/// both the current week and month, so a day crossing is already implied by the
/// month — but a calendar week can straddle a month boundary, so early in a
/// month the week total can lead the (freshly reset) month, hence both. Because
/// the snapshot is persisted, a crossing that happened while the app wasn't
/// running (it reads the logs Codex writes regardless) still catches up on the
/// next observation.
fn check_milestones(app: &tauri::AppHandle, dash: &Dashboard) {
    let Some(state) = app.try_state::<Celebration>() else {
        return;
    };
    // total_tokens is already in millions, so a 100M milestone is total / 100.
    let (week_id, month_id) = period_ids();
    let cur = MilestoneState {
        week_id,
        week_floor: (dash.week.metrics.total_tokens / 100.0).floor() as i64,
        month_id,
        month_floor: (dash.month.metrics.total_tokens / 100.0).floor() as i64,
    };

    let mut g = state.state.lock().unwrap();
    let fire = milestone_fire(g.as_ref(), &cur);
    // Keep the persisted floors monotonic within a period: a later observation
    // with a lower total (a transient/partial read, or two observers racing)
    // must not regress the stored floor and re-fire the celebration on restart.
    let mut next = cur.clone();
    if let Some(prev) = g.as_ref() {
        if prev.week_id == next.week_id && prev.week_floor > next.week_floor {
            next.week_floor = prev.week_floor;
        }
        if prev.month_id == next.month_id && prev.month_floor > next.month_floor {
            next.month_floor = prev.month_floor;
        }
    }
    *g = Some(next.clone());
    // Persist while still holding the lock so two observers can't interleave and
    // write a stale snapshot over a newer one.
    save_milestones(&next);
    drop(g);
    if fire {
        celebrate(app);
    }
}

/// Trigger the celebration overlay. Window/panel work must run on the main
/// thread (refresh() runs on a background thread), so hop there.
fn celebrate(app: &tauri::AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || show_celebration(&handle));
}

/// Show (or reuse) a full-screen, click-through, non-activating overlay on the
/// primary monitor and run the confetti animation, then hide it after it plays.
/// Must be called on the main thread.
fn show_celebration(app: &tauri::AppHandle) {
    let Some(state) = app.try_state::<Celebration>() else {
        return;
    };
    // Skip if a celebration is already playing.
    if state.active.swap(true, Ordering::SeqCst) {
        return;
    }

    let (pos, size) = match app.primary_monitor() {
        Ok(Some(m)) => (*m.position(), *m.size()),
        _ => {
            state.active.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Whether the confetti window was reused or freshly built — only used on
    // macOS to decide whether to (re-)apply the NSPanel attributes.
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let existed = app.get_webview_window("confetti").is_some();
    let win = match app.get_webview_window("confetti") {
        Some(w) => w,
        None => {
            match tauri::WebviewWindowBuilder::new(
                app,
                "confetti",
                tauri::WebviewUrl::App("confetti.html".into()),
            )
            .title("CodexScope Celebration")
            .inner_size(size.width as f64, size.height as f64)
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .resizable(false)
            .visible(false)
            .build()
            {
                Ok(w) => w,
                Err(_) => {
                    state.active.store(false, Ordering::SeqCst);
                    return;
                }
            }
        }
    };

    // Cover the whole primary monitor and let clicks pass through to the apps
    // beneath — the celebration must never interrupt what the user is doing.
    let _ = win.set_position(pos);
    let _ = win.set_size(size);
    let _ = win.set_ignore_cursor_events(true);

    #[cfg(target_os = "macos")]
    {
        use tauri_nspanel::cocoa::appkit::NSWindowCollectionBehavior;
        #[allow(non_upper_case_globals)]
        const NS_NONACTIVATING_PANEL: i32 = 1 << 7;

        // Convert to a non-activating panel once, so it can float over apps in
        // native fullscreen without stealing focus (same approach as the main
        // popover). On reuse the window is already a panel.
        if !existed {
            if let Ok(panel) = win.to_panel() {
                panel.set_level(25); // NSMainMenuWindowLevel (24) + 1
                panel.set_style_mask(NS_NONACTIVATING_PANEL);
                panel.set_collection_behaviour(
                    NSWindowCollectionBehavior::NSWindowCollectionBehaviorMoveToActiveSpace
                        | NSWindowCollectionBehavior::NSWindowCollectionBehaviorFullScreenAuxiliary,
                );
            }
        }
        let _ = win.eval("window.__burst&&window.__burst()");
        if let Ok(panel) = app.get_webview_panel("confetti") {
            panel.show();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = win.eval("window.__burst&&window.__burst()");
        let _ = win.show();
    }

    // Hide once the animation has played out (emission ~2.3s + fall/fade).
    let app2 = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(4200));
        let app3 = app2.clone();
        let _ = app2.run_on_main_thread(move || {
            #[cfg(target_os = "macos")]
            if let Ok(panel) = app3.get_webview_panel("confetti") {
                panel.order_out(None);
            }
            #[cfg(not(target_os = "macos"))]
            if let Some(w) = app3.get_webview_window("confetti") {
                let _ = w.hide();
            }
            if let Some(st) = app3.try_state::<Celebration>() {
                st.active.store(false, Ordering::SeqCst);
            }
        });
    });
}

/// Last tray-icon rectangle (physical px: x, y, width, height), captured on tray
/// click. Used to anchor the panel like tauri-plugin-positioner's
/// TrayBottomCenter — but we can't use the positioner itself on a swizzled
/// NSPanel: its calculate_position calls current_monitor().unwrap(), which fails
/// for a hidden/panel window, so positioning silently no-ops (panel stays
/// top-left). We also must add the icon height ourselves (see position_panel).
///
/// On Windows the cached tray rect is used only to pick which monitor the
/// popover opens on (see position_popover_windows); the popover itself is then
/// pinned to that monitor's top-right work-area corner with a small margin.
#[derive(Clone, Copy)]
struct TrayAnchorInfo {
    rect_x: f64,
    rect_y: f64,
    rect_w: f64,
    rect_h: f64,
    click_x: f64,
    click_y: f64,
}

struct TrayAnchor(std::sync::Mutex<Option<TrayAnchorInfo>>);

/// Timestamp (ms) of the last drag start. The popover hides on focus loss; on
/// Windows `start_dragging` enters the OS move loop which briefly blurs the
/// window, so we ignore the hide for a short window after a drag.
#[cfg(not(target_os = "macos"))]
struct DragGuard(AtomicI64);

/// Start dragging the borderless popover (Windows/Linux). Done via a command
/// (not the JS drag-region) so we can record the drag start and suppress the
/// imminent hide-on-blur. The frontend only calls this once a real drag begins.
#[cfg(not(target_os = "macos"))]
#[tauri::command]
fn begin_drag(window: tauri::Window) -> Result<(), String> {
    if let Some(g) = window.try_state::<DragGuard>() {
        g.0.store(now_ms(), Ordering::Relaxed);
    }
    window.start_dragging().map_err(|e| e.to_string())
}

/// macOS uses a menu-bar NSPanel that isn't user-draggable, so begin_drag is a
/// no-op there. It's also never invoked (the frontend gates it out) — this just
/// keeps the shared invoke_handler list valid and guarantees zero macOS effect.
#[cfg(target_os = "macos")]
#[tauri::command]
fn begin_drag(_window: tauri::Window) -> Result<(), String> {
    Ok(())
}

/// Anchor the panel under the tray icon, safely below the menu bar/notch area:
///   x = tray_x + tray_width/2 − window_width/2
///   y = max(tray_y + tray_height, monitor_top + notch_safe_top)
/// The tray rect's height is not always the full menu-bar height on notched
/// MacBooks, and a floating NSPanel is not auto-constrained by macOS like a
/// normal window. Clamp it to a conservative notch-safe top edge and screen
/// bounds so the panel doesn't tuck under the camera housing.
#[cfg(target_os = "macos")]
fn position_panel(app: &tauri::AppHandle) {
    const NOTCH_SAFE_TOP_LOGICAL: f64 = 42.0;
    const EDGE_MARGIN_LOGICAL: f64 = 8.0;

    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    let Ok(size) = w.outer_size() else {
        return;
    };
    let win_w = size.width as f64;
    let win_h = size.height as f64;

    if let Some(state) = app.try_state::<TrayAnchor>() {
        if let Some(anchor) = *state.0.lock().unwrap() {
            let rect_center_x = anchor.rect_x + anchor.rect_w / 2.0;
            let anchor_x =
                if (rect_center_x - anchor.click_x).abs() <= anchor.rect_w.max(48.0) * 2.0 {
                    rect_center_x
                } else {
                    anchor.click_x
                };
            let rect_bottom_y = anchor.rect_y + anchor.rect_h;
            let anchor_y =
                if (rect_bottom_y - anchor.click_y).abs() <= anchor.rect_h.max(24.0) * 2.0 {
                    rect_bottom_y
                } else {
                    anchor.click_y
                };
            let mut x = anchor_x - win_w / 2.0;
            let mut y = anchor_y;
            // tray-icon's macOS backend flips Y coordinates with the main
            // display height, which can make monitor_from_point pick the wrong
            // screen on multi-display setups. The menu-bar item's X range is the
            // stable signal, so prefer the monitor that horizontally contains it.
            let monitor = w
                .available_monitors()
                .ok()
                .and_then(|monitors| {
                    let margin = anchor.rect_w.max(48.0);
                    monitors
                        .iter()
                        .find(|m| {
                            let mp = m.position();
                            let ms = m.size();
                            let min_x = mp.x as f64 - margin;
                            let max_x = mp.x as f64 + ms.width as f64 + margin;
                            anchor_x >= min_x && anchor_x <= max_x
                        })
                        .cloned()
                })
                .or_else(|| {
                    w.monitor_from_point(anchor.click_x, anchor.click_y)
                        .ok()
                        .flatten()
                });
            if let Some(monitor) = monitor {
                let mp = monitor.position();
                let ms = monitor.size();
                let scale = monitor.scale_factor();
                let margin = EDGE_MARGIN_LOGICAL * scale;
                let min_x = mp.x as f64 + margin;
                let max_x = mp.x as f64 + ms.width as f64 - win_w - margin;
                if max_x >= min_x {
                    x = x.clamp(min_x, max_x);
                }

                let safe_top = mp.y as f64 + NOTCH_SAFE_TOP_LOGICAL * scale;
                let max_y = mp.y as f64 + ms.height as f64 - win_h - margin;
                let max_menu_anchor_y = safe_top + 24.0 * scale;
                if y < mp.y as f64 || y > max_menu_anchor_y {
                    y = safe_top;
                }
                y = y.max(safe_top);
                if max_y >= safe_top {
                    y = y.min(max_y);
                }
            }
            let _ = w.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
            return;
        }
    }

    // Fallback (e.g. opened from the menu before any tray click): centre near
    // the top of the current monitor.
    if let Ok(Some(monitor)) = w.current_monitor() {
        let mp = monitor.position();
        let ms = monitor.size();
        let scale = monitor.scale_factor();
        let margin = EDGE_MARGIN_LOGICAL * scale;
        let x = mp.x as f64 + (ms.width as f64 - win_w) / 2.0;
        let safe_top = mp.y as f64 + NOTCH_SAFE_TOP_LOGICAL * scale;
        let max_y = mp.y as f64 + ms.height as f64 - win_h - margin;
        let y = if max_y >= safe_top {
            safe_top.min(max_y)
        } else {
            safe_top
        };
        let _ = w.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    }
}

// ── Popover position memory (Windows/Linux) ─────────────────────────
// The borderless popover can be dragged (a header drag region calls
// startDragging in the frontend); we remember where the user left it and reopen
// there next time, falling back to the default top-right when there's no saved
// position on a connected monitor. macOS uses a menu-bar-anchored NSPanel and
// does not persist a position.
#[cfg(not(target_os = "macos"))]
fn popover_pos_path() -> Option<std::path::PathBuf> {
    let dir = dirs::data_dir()?.join("codexscope");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.join("popover_pos.json"))
}

#[cfg(not(target_os = "macos"))]
fn load_popover_pos() -> Option<(i32, i32)> {
    let t = std::fs::read_to_string(popover_pos_path()?).ok()?;
    serde_json::from_str(&t).ok()
}

#[cfg(not(target_os = "macos"))]
fn save_popover_pos(x: i32, y: i32) {
    if let Some(p) = popover_pos_path() {
        if let Ok(t) = serde_json::to_string(&(x, y)) {
            let _ = std::fs::write(p, t);
        }
    }
}

/// Position AND right-size the popover for the monitor it opens on. Reopens at
/// the user's last-dragged spot if it's still on a connected monitor, else pins
/// to the top-right of the tray monitor's work area (margin from the edges).
///
/// Everything is derived from the *intended* logical size × the target monitor's
/// scale — never the window's current physical size — and the size is re-asserted
/// on every open. A borderless window can otherwise get stuck at the previous
/// monitor's physical size after a DPI/monitor change (e.g. unplugging a 175%
/// display drops back to 100% but the window stays oversized until restart);
/// forcing the size here makes it recover on the next open. The monitor is
/// resolved from the cached tray rect -> current -> primary; work_area excludes
/// the taskbar so the margin is clean wherever the taskbar sits.
#[cfg(not(target_os = "macos"))]
fn position_popover_windows(app: &tauri::AppHandle) {
    // Logical size — must match app.windows[0] width/height in tauri.conf.json.
    const POPOVER_W: f64 = 400.0;
    const POPOVER_H: f64 = 900.0;
    const MARGIN: f64 = 12.0; // logical px gap from the screen edges

    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    // Force the intended size at the target monitor's DPI (recovers a stuck size).
    let fit = |monitor: &tauri::Monitor| {
        let scale = monitor.scale_factor();
        let max_h = (monitor.work_area().size.height as f64 / scale - MARGIN * 2.0).max(400.0);
        let height = POPOVER_H.min(max_h);
        let _ = w.set_size(tauri::PhysicalSize::new(
            (POPOVER_W * scale).round() as u32,
            (height * scale).round() as u32,
        ));
    };

    // 1. Reopen at the last position if a point just inside it is still on a
    //    connected monitor (a disconnected/shrunk monitor falls through to the
    //    default rather than opening off-screen).
    if let Some((sx, sy)) = load_popover_pos() {
        if let Ok(Some(m)) = w.monitor_from_point(sx as f64 + 20.0, sy as f64 + 20.0) {
            let _ = w.set_position(tauri::PhysicalPosition::new(sx, sy));
            fit(&m);
            return;
        }
    }

    // 2. Default: top-right of the tray monitor's work area.
    //    Prefer the monitor under the tray icon; fall back to current, then primary.
    let anchor = app
        .try_state::<TrayAnchor>()
        .and_then(|s| *s.0.lock().unwrap());
    let monitor = anchor
        .and_then(|anchor| {
            w.monitor_from_point(anchor.click_x, anchor.click_y)
                .ok()
                .flatten()
        })
        .or_else(|| w.current_monitor().ok().flatten())
        .or_else(|| app.primary_monitor().ok().flatten());

    if let Some(m) = monitor {
        let area = m.work_area(); // excludes the taskbar
        let scale = m.scale_factor();
        let margin = MARGIN * scale; // keep the visual gap DPI-consistent
        let win_w = POPOVER_W * scale; // intended physical width on this monitor
        let right = area.position.x as f64 + area.size.width as f64;
        let x = right - win_w - margin;
        let y = area.position.y as f64 + margin;
        let _ = w.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
        fit(&m);
    } else {
        // Couldn't resolve a monitor (rare) → let the positioner place it.
        let _ = w.move_window(Position::TopRight);
    }
}

/// True if our (Accessory) app is currently the frontmost application.
#[cfg(target_os = "macos")]
fn app_is_frontmost() -> bool {
    use tauri_nspanel::cocoa::base::id;
    use tauri_nspanel::objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let proc_info: id = msg_send![class!(NSProcessInfo), processInfo];
        let our_pid: i32 = msg_send![proc_info, processIdentifier];
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let front: id = msg_send![workspace, frontmostApplication];
        if front.is_null() {
            return false;
        }
        let front_pid: i32 = msg_send![front, processIdentifier];
        front_pid == our_pid
    }
}

/// Hide the panel when the user switches Space or activates another app, so it
/// doesn't linger over the new (e.g. fullscreen) Space until the next click.
/// resign-key alone misses pure Space switches because the panel joins all
/// Spaces and can stay key across the transition.
#[cfg(target_os = "macos")]
fn hide_panel_on_context_switch(app: &tauri::AppHandle) {
    if app
        .try_state::<PinnedState>()
        .map(|p| p.0.load(Ordering::Relaxed))
        .unwrap_or(false)
    {
        return;
    }
    if app_is_frontmost() {
        return;
    }
    if let Ok(panel) = app.get_webview_panel("main") {
        if panel.is_visible() {
            panel.order_out(None);
        }
    }
}

/// Register NSWorkspace observers that auto-hide the panel on Space change / app
/// activation (mirrors tauri-nspanel's menu-bar example). The observers live for
/// the whole app lifetime, so the returned tokens are intentionally dropped.
#[cfg(target_os = "macos")]
fn register_panel_autohide(app: &tauri::AppHandle) {
    use std::ffi::CString;
    use tauri_nspanel::block::ConcreteBlock;
    use tauri_nspanel::cocoa::base::{id, nil};
    use tauri_nspanel::objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let center: id = msg_send![workspace, notificationCenter];
        for name in [
            "NSWorkspaceActiveSpaceDidChangeNotification",
            "NSWorkspaceDidActivateApplicationNotification",
        ] {
            let app = app.clone();
            let block = ConcreteBlock::new(move |_notif: id| {
                hide_panel_on_context_switch(&app);
            });
            let block = block.copy();
            let ns_name: id = msg_send![
                class!(NSString),
                stringWithUTF8String: CString::new(name).unwrap().as_ptr()
            ];
            let _: id = msg_send![
                center,
                addObserverForName: ns_name object: nil queue: nil usingBlock: block
            ];
        }
    }
}

/// Read the user's GLOBAL macOS appearance preference: true when dark mode is on.
/// We read `AppleInterfaceStyle` from NSUserDefaults (present and "Dark" => dark,
/// absent => light) rather than the app's NSApp.effectiveAppearance — an
/// Accessory (menu-bar) app never becomes frontmost, so its effective appearance
/// (and thus the webview's `prefers-color-scheme`) can lag the real system value.
/// The user default reflects the system setting directly, regardless of focus.
#[cfg(target_os = "macos")]
fn system_is_dark() -> bool {
    use std::ffi::CStr;
    use tauri_nspanel::cocoa::base::{id, nil};
    use tauri_nspanel::objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let defaults: id = msg_send![class!(NSUserDefaults), standardUserDefaults];
        let key: id = msg_send![
            class!(NSString),
            stringWithUTF8String: b"AppleInterfaceStyle\0".as_ptr() as *const std::os::raw::c_char
        ];
        let val: id = msg_send![defaults, stringForKey: key];
        if val == nil {
            return false;
        }
        let raw: *const std::os::raw::c_char = msg_send![val, UTF8String];
        if raw.is_null() {
            return false;
        }
        CStr::from_ptr(raw)
            .to_string_lossy()
            .eq_ignore_ascii_case("dark")
    }
}

/// Watch for live system dark/light-mode changes and push them to the frontend.
/// `AppleInterfaceThemeChangedNotification` is posted on the DISTRIBUTED
/// notification center the instant the user flips Appearance, and is delivered
/// to every registered app regardless of activation policy or frontmost status —
/// so it works for our hidden, non-activating menu-bar panel where the webview's
/// own `prefers-color-scheme` `change` event does not reliably fire. The observer
/// lives for the whole app lifetime, so the returned token is intentionally
/// dropped (same as register_panel_autohide).
#[cfg(target_os = "macos")]
fn watch_system_theme(app: &tauri::AppHandle) {
    use std::ffi::CString;
    use tauri_nspanel::block::ConcreteBlock;
    use tauri_nspanel::cocoa::base::{id, nil};
    use tauri_nspanel::objc::{class, msg_send, sel, sel_impl};

    unsafe {
        let center: id = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        let app = app.clone();
        let block = ConcreteBlock::new(move |_notif: id| {
            let _ = app.emit("system-theme", system_is_dark());
        });
        let block = block.copy();
        let ns_name: id = msg_send![
            class!(NSString),
            stringWithUTF8String: CString::new("AppleInterfaceThemeChangedNotification").unwrap().as_ptr()
        ];
        let _: id = msg_send![
            center,
            addObserverForName: ns_name object: nil queue: nil usingBlock: block
        ];
    }
}

/// Show the panel as a popover anchored under the tray icon, and focus it.
/// Always reset the scroll to the top so it doesn't reopen mid-scroll.
fn show_popover(app: &tauri::AppHandle) {
    // On macOS the window is an NSPanel — position it manually, then show()
    // (makes it key and orders it front, incl. over fullscreen Spaces).
    #[cfg(target_os = "macos")]
    {
        position_panel(app);
        if let Ok(panel) = app.get_webview_panel("main") {
            panel.show();
        }
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(w) = app.get_webview_window("main") {
        // Pin the popover to the monitor's top-right corner (see
        // position_popover_windows).
        position_popover_windows(app);
        let _ = w.show();
        let _ = w.set_focus();
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.eval(
            "(function(){var e=document.querySelector('.om-scroll');if(e){e.scrollTop=0;}else{window.scrollTo(0,0);}})()",
        );
    }
}

#[tauri::command]
async fn get_dashboard(app: tauri::AppHandle) -> Dashboard {
    // Opening the popover should never leave an old quota visible for another
    // full polling interval. Return the cached dashboard immediately, while a
    // stale (>2 min) official quota refresh runs in the background and pushes
    // `dashboard-updated` when it completes.
    if account_usage::rate_limits_cache_is_stale(2 * 60 * 1000) {
        let handle = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let _ = account_usage::refresh_rate_limits_cache();
            refresh(&handle);
        });
    }
    // build_dashboard does blocking IO (reads/writes the cache, parses logs) and
    // holds BUILD_LOCK — running it inline would block the command on the async
    // runtime and, with a large cache, stall the UI. Hop to a blocking worker
    // (the 30s refresh thread already runs the same work off the main thread).
    let dash = tauri::async_runtime::spawn_blocking(parser::build_dashboard)
        .await
        .unwrap_or_else(|_| parser::build_dashboard());
    // Sync the tray count to this freshly-fetched value. The panel refetches the
    // instant it opens, while the tray otherwise only refreshes every 30s — so
    // without this the two could disagree for up to 30s during heavy usage.
    if let Some(tray) = app.tray_by_id("main") {
        let label = fmt_tokens_m(dash.today_tokens);
        let _ = tray.set_title(tray_title_for_platform(&label));
        // Mirror refresh(): keep the tooltip in sync for Windows, where the
        // title isn't shown next to the icon.
        let _ = tray.set_tooltip(Some(format!("CodexScope · 今日 {}", label)));
    }
    check_milestones(&app, &dash);
    dash
}

#[tauri::command]
async fn refresh_dashboard(app: tauri::AppHandle) -> Dashboard {
    // Explicit refresh waits for a fresh official quota snapshot, then returns
    // a dashboard rebuilt from both the new quota and latest local logs.
    let _ = tauri::async_runtime::spawn_blocking(|| {
        account_usage::force_refresh_rate_limits_cache()
    })
    .await;
    get_dashboard(app).await
}

#[tauri::command]
async fn get_period_report(period: String, offset: i32) -> PeriodReport {
    let fallback_period = period.clone();
    tauri::async_runtime::spawn_blocking(move || parser::build_period_report(&period, offset))
        .await
        .unwrap_or_else(|_| parser::build_period_report(&fallback_period, offset))
}

#[tauri::command]
async fn get_range_report(start_ms: i64, end_ms: i64) -> PeriodReport {
    tauri::async_runtime::spawn_blocking(move || parser::build_range_report(start_ms, end_ms))
        .await
        .unwrap_or_else(|_| parser::build_range_report(start_ms, end_ms))
}

/// Save a full-panel screenshot (a `data:image/png;base64,...` URL captured in
/// the webview) to the user's Desktop as `CodexScope <date> at <time>.png`.
/// DOM rasterization sidesteps macOS Screen Recording permission entirely.
/// Returns the written file path on success.
#[tauri::command]
fn save_screenshot(data_url: String) -> Result<String, String> {
    use base64::Engine;
    let body = data_url
        .strip_prefix("data:image/png;base64,")
        .ok_or_else(|| "expected a data:image/png;base64,... URL".to_string())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .map_err(|e| format!("invalid base64: {e}"))?;

    let dir =
        dirs::desktop_dir().ok_or_else(|| "could not resolve the Desktop directory".to_string())?;
    let stamp = chrono::Local::now().format("CodexScope %Y-%m-%d at %H.%M.%S.png");
    let path = dir.join(stamp.to_string());

    std::fs::write(&path, &bytes).map_err(|e| format!("failed to write file: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
fn get_autostart_enabled(app: tauri::AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart_enabled(app: tauri::AppHandle, enabled: bool) -> bool {
    let mgr = app.autolaunch();
    let _ = if enabled { mgr.enable() } else { mgr.disable() };
    let now_on = mgr.is_enabled().unwrap_or(enabled);
    save_autostart_pref(now_on);
    now_on
}

#[tauri::command]
fn get_pinned(app: tauri::AppHandle) -> bool {
    app.try_state::<PinnedState>()
        .map(|p| p.0.load(Ordering::Relaxed))
        .unwrap_or(false)
}

#[tauri::command]
fn set_pinned(app: tauri::AppHandle, enabled: bool) -> bool {
    if let Some(pinned) = app.try_state::<PinnedState>() {
        pinned.0.store(enabled, Ordering::Relaxed);
    }
    save_pinned_pref(enabled);
    let _ = app.emit("pinned-changed", enabled);
    enabled
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// For CLI/example validation against real logs.
pub fn dashboard_json() -> String {
    serde_json::to_string_pretty(&parser::build_dashboard()).unwrap_or_default()
}

fn fmt_tokens_m(m: f64) -> String {
    if m >= 1000.0 {
        format!("{:.2}B", m / 1000.0)
    } else if m >= 100.0 {
        format!("{:.0}M", m)
    } else if m >= 10.0 {
        format!("{:.1}M", m)
    } else if m >= 1.0 {
        format!("{:.2}M", m)
    } else {
        let k = (m * 1000.0).round() as i64;
        // no usage yet (e.g. just past midnight) — "0K" reads like "OK", so
        // show a clearer idle label instead.
        if k <= 0 {
            "就绪".to_string()
        } else {
            format!("{k}K")
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Tracks when the popover was last hidden, so a click on the tray icon
    // while it's open (which first blurs/hides it) doesn't immediately reopen.
    let last_hidden = Arc::new(AtomicI64::new(0));

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        // Must be the FIRST plugin: a second launch (e.g. reinstall/relaunch)
        // hands off to the already-running instance and exits, so the menu bar
        // never shows two icons.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_popover(app);
        }))
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ));
    // Registers the WebviewPanelManager state used by `to_panel`/`get_webview_panel`.
    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(tauri_nspanel::init());
    }

    builder
        .invoke_handler(tauri::generate_handler![
            get_dashboard,
            refresh_dashboard,
            get_period_report,
            get_range_report,
            save_screenshot,
            get_autostart_enabled,
            set_autostart_enabled,
            get_pinned,
            set_pinned,
            quit_app,
            begin_drag
        ])
        .setup(move |app| {
            // Menu-bar–only app: no Dock icon, runs in the background.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Holds the latest tray-icon rect so show_popover can anchor the panel.
            // Captured in the tray click handler on every platform — see
            // position_panel (macOS, below the icon) and position_popover_windows
            // (Windows/Linux, above the icon).
            app.manage(TrayAnchor(std::sync::Mutex::new(None)));
            app.manage(PinnedState(AtomicBool::new(load_pinned_pref())));
            // Drag-start timestamp so a drag doesn't hide the popover (non-macOS).
            #[cfg(not(target_os = "macos"))]
            app.manage(DragGuard(AtomicI64::new(0)));

            // 100M-token celebration tracking. Load the persisted snapshot so
            // milestones survive restarts/reboots/updates; the first run ever
            // (no file) baselines on first observation without celebrating.
            app.manage(Celebration {
                state: std::sync::Mutex::new(load_milestones()),
                active: AtomicBool::new(false),
            });

            // Reconcile launch-at-login with the user's saved preference. The
            // on/off toggle lives in the tray's right-click menu (built below);
            // we do NOT force-enable on every start, which would undo a manual
            // opt-out. `autostart_on` seeds the menu checkbox.
            let autostart_on = reconcile_autostart(app.handle());
            #[cfg(target_os = "macos")]
            let _ = autostart_on;

            // Popover behaviour. On macOS, convert the window to a non-activating
            // NSPanel so it can float over apps in native fullscreen, and hide it
            // on resign-key (clicking outside / switching apps) like a popover.
            #[cfg(target_os = "macos")]
            if let Some(window) = app.get_webview_window("main") {
                use tauri_nspanel::cocoa::appkit::NSWindowCollectionBehavior;
                // NSWindowStyleMaskNonActivatingPanel — receive events without
                // activating (stealing focus from) the frontmost app.
                #[allow(non_upper_case_globals)]
                const NS_NONACTIVATING_PANEL: i32 = 1 << 7;

                let lh = last_hidden.clone();
                let handle = app.handle().clone();
                let delegate = tauri_nspanel::panel_delegate!(CodexScopePanelDelegate {
                    window_did_resign_key
                });
                delegate.set_listener(Box::new(move |name: String| {
                    if name == "window_did_resign_key" {
                        if handle
                            .try_state::<PinnedState>()
                            .map(|p| p.0.load(Ordering::Relaxed))
                            .unwrap_or(false)
                        {
                            return;
                        }
                        lh.store(now_ms(), Ordering::Relaxed);
                        if let Ok(panel) = handle.get_webview_panel("main") {
                            panel.order_out(None);
                        }
                    }
                }));

                if let Ok(panel) = window.to_panel() {
                    panel.set_level(25); // NSMainMenuWindowLevel (24) + 1
                    panel.set_style_mask(NS_NONACTIVATING_PANEL);
                    // MoveToActiveSpace: the panel relocates onto whatever Space
                    // is active *when shown* — so it appears over a fullscreen app
                    // if you open it there, but it does NOT live on every Space.
                    // (CanJoinAllSpaces + Stationary made it omnipresent and kept
                    // it painted through transitions, so it lingered/ghosted over
                    // a fullscreen Space even after order_out.) FullScreenAuxiliary
                    // is what actually permits coexisting with a fullscreen window.
                    panel.set_collection_behaviour(
                        NSWindowCollectionBehavior::NSWindowCollectionBehaviorMoveToActiveSpace
                            | NSWindowCollectionBehavior::NSWindowCollectionBehaviorFullScreenAuxiliary,
                    );
                    panel.set_delegate(delegate);
                }

                // Also hide on Space change / app activation, not just resign-key.
                register_panel_autohide(app.handle());

                // Follow the system appearance natively (the webview's
                // prefers-color-scheme is unreliable for a hidden, non-activating
                // menu-bar panel). Watch for live changes, and emit the current
                // value once now so the frontend's System mode starts correct even
                // if the webview reported a stale appearance at launch.
                watch_system_theme(app.handle());
                let _ = app.emit("system-theme", system_is_dark());
            }

            // Non-macOS: keep the plain window, hide on focus loss.
            #[cfg(not(target_os = "macos"))]
            if let Some(win) = app.get_webview_window("main") {
                let w = win.clone();
                let lh = last_hidden.clone();
                let handle = app.handle().clone();
                win.on_window_event(move |e| match e {
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        lh.store(now_ms(), Ordering::Relaxed);
                        if let Ok(p) = w.outer_position() {
                            save_popover_pos(p.x, p.y);
                        }
                        let _ = w.hide();
                    }
                    WindowEvent::Focused(false) => {
                        if handle
                            .try_state::<PinnedState>()
                            .map(|p| p.0.load(Ordering::Relaxed))
                            .unwrap_or(false)
                        {
                            return;
                        }
                        // A hidden window's "blur" (e.g. the one Windows fires at
                        // startup) carries a meaningless default position — only a
                        // VISIBLE popover the user clicks away from should be saved
                        // and hidden. Without this, startup persisted the OS's
                        // default placement and every open snapped there.
                        if !w.is_visible().unwrap_or(false) {
                            return;
                        }
                        // A title-bar drag momentarily blurs the window (the OS
                        // move loop); don't treat that as a click-away dismiss.
                        let dragging = w
                            .try_state::<DragGuard>()
                            .map(|g| now_ms() - g.0.load(Ordering::Relaxed) < 700)
                            .unwrap_or(false);
                        if dragging {
                            return;
                        }
                        lh.store(now_ms(), Ordering::Relaxed);
                        // Remember where the user left it (dragged or default) so
                        // the next open reuses this spot.
                        if let Ok(p) = w.outer_position() {
                            save_popover_pos(p.x, p.y);
                        }
                        let _ = w.hide();
                    }
                    _ => {}
                });
            }

            // Build the menu-bar tray: app glyph (template icon) + today's tokens.
            let dash = parser::build_dashboard();
            let label = fmt_tokens_m(dash.today_tokens);
            let tray_title = tray_title_for_platform(&label);

            let lh_tray = last_hidden.clone();
            let mut tray_builder = TrayIconBuilder::with_id("main")
                .icon(tauri::include_image!("icons/tray-icon.png"))
                .icon_as_template(false)
                .tooltip(format!("CodexScope · 今日 {}", label))
                .show_menu_on_left_click(false) // left = toggle panel; non-macOS right = native menu
                .on_tray_icon_event(move |tray, event| {
                    let app = tray.app_handle();
                    tauri_plugin_positioner::on_tray_event(app, &event);
                    // Cache the tray-icon rect (physical px) for panel positioning.
                    // macOS aligns the panel under the menu-bar icon; Windows/Linux
                    // uses it to pick the monitor and pins the popover to that
                    // monitor's top-right — see position_panel / position_popover_windows.
                    if let TrayIconEvent::Click { position, rect, .. } = &event {
                        if let Some(anchor) = app.try_state::<TrayAnchor>() {
                            let p = rect.position.to_physical::<f64>(1.0);
                            let s = rect.size.to_physical::<f64>(1.0);
                            *anchor.0.lock().unwrap() = Some(TrayAnchorInfo {
                                rect_x: p.x,
                                rect_y: p.y,
                                rect_w: s.width,
                                rect_h: s.height,
                                click_x: position.x,
                                click_y: position.y,
                            });
                        }
                    }
                    // macOS does not attach an NSStatusItem menu, so mouseUp is
                    // reliable and avoids focus churn from showing the panel
                    // while the status-button press is still active.
                    let should_toggle_popover = matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    );
                    if should_toggle_popover {
                        // if it was just hidden by the blur from this same click, leave it closed
                        let just_hidden = now_ms() - lh_tray.load(Ordering::Relaxed) < 250;
                        #[cfg(target_os = "macos")]
                        {
                            let visible = app
                                .get_webview_panel("main")
                                .map(|p| p.is_visible())
                                .unwrap_or(false);
                            if visible {
                                if let Ok(p) = app.get_webview_panel("main") {
                                    p.order_out(None);
                                }
                            } else if !just_hidden {
                                show_popover(app);
                            }
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            let visible = app
                                .get_webview_window("main")
                                .and_then(|w| w.is_visible().ok())
                                .unwrap_or(false);
                            if visible {
                                if let Some(w) = app.get_webview_window("main") {
                                    let _ = w.hide();
                                }
                            } else if !just_hidden {
                                show_popover(app);
                            }
                        }
                    }
                })
                ;

            #[cfg(not(target_os = "macos"))]
            {
                let open_i = MenuItem::with_id(app, "open", "打开 CodexScope", true, None::<&str>)?;
                let refresh_i = MenuItem::with_id(app, "refresh", "刷新", true, None::<&str>)?;
                // Launch-at-login toggle (a checkbox item). Seeded from the reconciled
                // preference; clicking it flips the OS registration and persists.
                let autostart_i = CheckMenuItem::with_id(
                    app,
                    "autostart",
                    "开机自动启动",
                    true,
                    autostart_on,
                    None::<&str>,
                )?;
                let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let menu = Menu::with_items(
                    app,
                    &[
                        &open_i,
                        &refresh_i,
                        &PredefinedMenuItem::separator(app)?,
                        &autostart_i,
                        &PredefinedMenuItem::separator(app)?,
                        &quit_i,
                    ],
                )?;

                tray_builder = tray_builder.menu(&menu).on_menu_event(move |app, event| match event.id.as_ref() {
                    "open" => show_popover(app),
                    "refresh" => refresh(app),
                    "autostart" => {
                        // Flip the OS registration, re-read the real state, mirror
                        // it into the checkbox, and persist the user's choice.
                        let mgr = app.autolaunch();
                        let enabled = mgr.is_enabled().unwrap_or(false);
                        let _ = if enabled { mgr.disable() } else { mgr.enable() };
                        let now_on = mgr.is_enabled().unwrap_or(!enabled);
                        let _ = autostart_i.set_checked(now_on);
                        save_autostart_pref(now_on);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                });
            }

            if let Some(title) = tray_title.as_deref() {
                tray_builder = tray_builder.title(title);
            }

            let tray = tray_builder.build(app)?;
            // Re-apply after creation so left click is always reserved for the
            // popover. On macOS we intentionally do not attach an NSStatusItem
            // menu because AppKit treats that menu as the left-click action.
            let _ = tray.set_show_menu_on_left_click(false);

            // Load prices off the main thread (the fetch can block ~20s on a
            // cold/stale cache) and refresh once a day. build_dashboard reads the
            // memoized copy, so neither JSON parsing nor the network ever runs
            // while BUILD_LOCK is held.
            std::thread::spawn(|| {
                pricing::Pricing::reload_shared();
                loop {
                    std::thread::sleep(Duration::from_secs(24 * 60 * 60));
                    pricing::Pricing::reload_shared();
                }
            });

            // Account-level usage is the source for lifetime stats. It can take
            // tens of seconds, so keep its 15-minute loop separate from the
            // faster official quota loop below.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                let _ = account_usage::refresh_cache();
                refresh(&handle);
                std::thread::sleep(Duration::from_secs(15 * 60));
            });

            // Official remaining quota updates every two minutes. Failed
            // requests retain the last valid cache, so the UI never falls back
            // to an unrelated per-model limit.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                let _ = account_usage::refresh_rate_limits_cache();
                refresh(&handle);
                std::thread::sleep(Duration::from_secs(2 * 60));
            });

            // Background refresh: keep the tray's token count current and push
            // live updates to an open popover. Cheap thanks to incremental ingest.
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(30));
                refresh(&handle);
            });

            // Filesystem watcher: reflect Codex rollout writes within ~1s instead
            // of waiting up to the 30s poll. Our cache lives elsewhere, so this
            // never self-triggers. Debounced so a burst of writes coalesces into
            // one rebuild; the 30s poll above stays as a fallback.
            if let Some(codex_home) = std::env::var_os("CODEX_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| dirs::home_dir().map(|h| h.join(".codex")))
            {
                let roots = vec![codex_home.join("sessions"), codex_home.join("archived_sessions")];
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    use notify::{RecursiveMode, Watcher};
                    let (tx, rx) = std::sync::mpsc::channel();
                    let mut watcher = match notify::recommended_watcher(
                        move |res: notify::Result<notify::Event>| {
                            if res.is_ok() {
                                let _ = tx.send(());
                            }
                        },
                    ) {
                        Ok(w) => w,
                        Err(_) => return,
                    };
                    let mut watched = false;
                    for root in roots {
                        let _ = std::fs::create_dir_all(&root);
                        if watcher.watch(&root, RecursiveMode::Recursive).is_ok() {
                            watched = true;
                        }
                    }
                    if !watched {
                        return;
                    }
                    // Block for the first change, then drain the burst until quiet.
                    while rx.recv().is_ok() {
                        while rx.recv_timeout(Duration::from_millis(400)).is_ok() {}
                        refresh(&handle);
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(wk: &str, wf: i64, mo: &str, mf: i64) -> MilestoneState {
        MilestoneState {
            week_id: wk.into(),
            week_floor: wf,
            month_id: mo.into(),
            month_floor: mf,
        }
    }

    #[test]
    fn first_ever_observation_baselines_without_firing() {
        // No prior snapshot → never celebrate pre-existing usage on first run.
        assert!(!milestone_fire(None, &ms("2026-W24", 3, "2026-06", 3)));
    }

    #[test]
    fn no_change_does_not_fire() {
        let prev = ms("2026-W24", 1, "2026-06", 3);
        assert!(!milestone_fire(
            Some(&prev),
            &ms("2026-W24", 1, "2026-06", 3)
        ));
    }

    #[test]
    fn month_crossing_fires() {
        let prev = ms("2026-W24", 1, "2026-06", 3);
        assert!(milestone_fire(
            Some(&prev),
            &ms("2026-W24", 1, "2026-06", 4)
        ));
    }

    #[test]
    fn week_crossing_fires_even_when_month_flat() {
        // Early in a month the week (straddling from the previous month) can lead.
        let prev = ms("2026-W24", 0, "2026-06", 0);
        assert!(milestone_fire(
            Some(&prev),
            &ms("2026-W24", 1, "2026-06", 0)
        ));
    }

    #[test]
    fn multi_boundary_jump_is_a_single_fire() {
        // 3 → 7 is still one celebration (fire is a bool, not a count).
        let prev = ms("2026-W24", 1, "2026-06", 3);
        assert!(milestone_fire(
            Some(&prev),
            &ms("2026-W24", 1, "2026-06", 7)
        ));
    }

    #[test]
    fn new_month_rebaselines_silently() {
        // Period id changed → that period reset; re-baseline, don't compare floors
        // (so a new month opening below last month's floor never fires).
        let prev = ms("2026-W24", 1, "2026-06", 3);
        assert!(!milestone_fire(
            Some(&prev),
            &ms("2026-W27", 0, "2026-07", 0)
        ));
    }

    #[test]
    fn new_week_does_not_fire_on_reset() {
        let prev = ms("2026-W24", 2, "2026-06", 3);
        // New week (id changed), month unchanged and flat → no fire.
        assert!(!milestone_fire(
            Some(&prev),
            &ms("2026-W25", 0, "2026-06", 3)
        ));
    }
}
