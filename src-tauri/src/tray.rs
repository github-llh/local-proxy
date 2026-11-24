//! 托盘图标与菜单、popover / 设置窗口管理。

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WebviewWindow, WebviewWindowBuilder,
};

/// popover 窗口尺寸。
const POPOVER_SIZE: (f64, f64) = (360.0, 520.0);
/// 设置窗口尺寸。
const SETTINGS_SIZE: (f64, f64) = (760.0, 600.0);

/// 初始化托盘与全部窗口。
pub fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    create_windows(app)?;

    let open_popover = MenuItem::with_id(app, "open-popover", "打开面板", true, None::<&str>)?;
    let open_settings = MenuItem::with_id(app, "open-settings", "设置…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 LocalProxy", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_popover, &open_settings, &quit])?;

    let _tray = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().unwrap())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("LocalProxy")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open-popover" => {
                let _ = show_popover(app);
            }
            "open-settings" => {
                let _ = show_settings(app);
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                let _ = toggle_popover(app);
            }
        })
        .build(app)?;

    Ok(())
}

/// 创建隐藏的 popover / settings 窗口。
fn create_windows(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // popover：无边框、置顶、不在任务栏显示
    let popover = WebviewWindowBuilder::new(
        app,
        "popover",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("LocalProxy")
    .inner_size(POPOVER_SIZE.0, POPOVER_SIZE.1)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    .visible(false)
    .build()?;
    // 失焦自动隐藏（菜单栏面板惯例）
    let popover_clone = popover.clone();
    popover.on_window_event(move |event| {
        if let tauri::WindowEvent::Focused(false) = event {
            let _ = popover_clone.hide();
        }
    });

    // 设置窗口：常规装饰、居中
    WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("index.html".into()),
    )
    .title("LocalProxy 设置")
    .inner_size(SETTINGS_SIZE.0, SETTINGS_SIZE.1)
    .center()
    .visible(false)
    .build()?;

    Ok(())
}

/// 显示 popover 并定位到托盘下方。
pub fn show_popover(app: &AppHandle) -> tauri::Result<()> {
    let window = popover_window(app)?;
    position_near_tray(app, &window)?;
    window.show()?;
    window.set_focus()?;
    Ok(())
}

/// 切换 popover 可见性。
pub fn toggle_popover(app: &AppHandle) -> tauri::Result<()> {
    let window = popover_window(app)?;
    if window.is_visible()? {
        window.hide()?;
    } else {
        position_near_tray(app, &window)?;
        window.show()?;
        window.set_focus()?;
    }
    Ok(())
}

/// 显示设置窗口。
pub fn show_settings(app: &AppHandle) -> tauri::Result<()> {
    let window = app
        .get_webview_window("settings")
        .ok_or_else(|| tauri::Error::WindowNotFound)?;
    window.show()?;
    window.set_focus()?;
    Ok(())
}

fn popover_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    app.get_webview_window("popover")
        .ok_or_else(|| tauri::Error::WindowNotFound)
}

/// 将窗口定位到托盘图标下方（macOS 顶部菜单栏 / Windows 任务栏右侧）。
fn position_near_tray(app: &AppHandle, window: &WebviewWindow) -> tauri::Result<()> {
    let win_size = window.outer_size()?;
    let scale = window.scale_factor().unwrap_or(1.0);

    // 优先用托盘矩形（Tauri v2: Rect { position: Position, size: Size }，均为 enum）
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(Some(rect)) = tray.rect() {
            let (rx, ry) = match rect.position {
                tauri::Position::Physical(p) => (p.x, p.y),
                tauri::Position::Logical(l) => ((l.x * scale) as i32, (l.y * scale) as i32),
            };
            let (rw, rh) = match rect.size {
                tauri::Size::Physical(s) => (s.width as i32, s.height as i32),
                tauri::Size::Logical(s) => ((s.width * scale) as i32, (s.height * scale) as i32),
            };
            let x = rx + rw - win_size.width as i32;
            let y = ry + rh + 4;
            window.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
                x: x.max(0),
                y: y.max(0),
            }))?;
            return Ok(());
        }
    }

    // 兜底：主显示器右上角
    if let Ok(Some(monitor)) = window.primary_monitor() {
        let x = monitor.size().width as i32 - win_size.width as i32 - 8;
        window.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
            x: x.max(0),
            y: 28,
        }))?;
    }
    Ok(())
}

/// 向前端广播窗口标签（前端据此渲染 popover / settings 视图）。
pub fn announce_window_label(app: &AppHandle) {
    for label in ["popover", "settings"] {
        if let Some(window) = app.get_webview_window(label) {
            let _ = window.emit("window-label", label);
        }
    }
}
