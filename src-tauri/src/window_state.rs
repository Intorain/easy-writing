//! 主窗口状态：尺寸与位置由应用自己按"点"（逻辑像素）记在 window-geometry.json 里；
//! 最大化状态仍交给官方插件。官方插件记的是物理像素，显示器换缩放档（Retina 默认 ↔ 更多空间）后
//! 同一个数字对应的窗口会大一倍或小一半，这就是"每次启动都很大"的来源。
//! 第一次运行（没有记录）按屏幕可用区域的 85% 开窗，上限 1440×960。

use serde::{Deserialize, Serialize};
use tauri::{plugin::TauriPlugin, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Window, Wry};
use tauri_plugin_window_state::{AppHandleExt, StateFlags, WindowExt};

use crate::{DesktopWebviewWindow, MAIN_WINDOW_LABEL};

pub const MIN_WIDTH: f64 = 1100.0;
pub const MIN_HEIGHT: f64 = 720.0;
/// 第一次运行的默认尺寸上限（点）
pub const DEFAULT_WIDTH: f64 = 1440.0;
pub const DEFAULT_HEIGHT: f64 = 960.0;
/// 默认窗口占屏幕可用区域的比例
const DEFAULT_FRACTION: f64 = 0.85;
const GEOMETRY_FILE: &str = "window-geometry.json";
/// 插件只管最大化；尺寸与位置见下文
const SAVED_STATE: StateFlags = StateFlags::MAXIMIZED;

/// 普通（非最大化）窗口的位置与尺寸，单位是点，与显示器缩放无关
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
}

pub fn plugin() -> TauriPlugin<Wry> {
    tauri_plugin_window_state::Builder::default()
        .with_state_flags(SAVED_STATE)
        .with_filter(|label| label == MAIN_WINDOW_LABEL)
        // 窗口始终可见，且不能用旧状态覆盖平台各自的标题栏样式。
        // 先恢复普通窗口的边界，再最大化，最后显示。
        .skip_initial_state(MAIN_WINDOW_LABEL)
        .build()
}

fn geometry_path(window: &DesktopWebviewWindow) -> Option<std::path::PathBuf> {
    window.app_handle().path().app_config_dir().ok().map(|dir| dir.join(GEOMETRY_FILE))
}

pub fn load_geometry(window: &DesktopWebviewWindow) -> Option<Geometry> {
    let text = std::fs::read_to_string(geometry_path(window)?).ok()?;
    let geometry: Geometry = serde_json::from_str(&text).ok()?;
    (geometry.width.is_finite() && geometry.height.is_finite() && geometry.width > 0.0 && geometry.height > 0.0).then_some(geometry)
}

fn save_geometry(window: &DesktopWebviewWindow) -> Result<(), String> {
    if window.is_maximized().map_err(|e| e.to_string())? || window.is_minimized().map_err(|e| e.to_string())? || window.is_fullscreen().map_err(|e| e.to_string())? {
        return Ok(()); // 最大化、最小化、全屏时的尺寸不是作者想要的普通窗口尺寸
    }
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let size = window.inner_size().map_err(|e| e.to_string())?.to_logical::<f64>(scale);
    let position = window.outer_position().map_err(|e| e.to_string())?.to_logical::<f64>(scale);
    let geometry = Geometry { width: size.width, height: size.height, x: position.x, y: position.y };
    let path = geometry_path(window).ok_or("没有配置目录")?;
    std::fs::write(&path, serde_json::to_string_pretty(&geometry).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// 第一次运行的窗口尺寸：屏幕可用区域的 85%，不超过 1440×960，不低于最小尺寸（但最小尺寸也不能超出屏幕）
pub fn default_size(work_width: f64, work_height: f64) -> (f64, f64) {
    let width = (work_width * DEFAULT_FRACTION).min(DEFAULT_WIDTH).max(MIN_WIDTH.min(work_width));
    let height = (work_height * DEFAULT_FRACTION).min(DEFAULT_HEIGHT).max(MIN_HEIGHT.min(work_height));
    (width.floor(), height.floor())
}

pub fn restore(window: &DesktopWebviewWindow) {
    match load_geometry(window) {
        Some(geometry) => {
            if let Err(error) = window
                .set_size(LogicalSize::new(geometry.width, geometry.height))
                .and_then(|()| window.set_position(LogicalPosition::new(geometry.x, geometry.y)))
            {
                eprintln!("恢复主窗口位置和尺寸失败，使用初始窗口：{error}");
            }
        }
        None => {
            // 没有记录：按屏幕大小开窗，居中
            if let Ok(Some(monitor)) = window.current_monitor().or_else(|_| window.primary_monitor()) {
                let scale = monitor.scale_factor();
                let area = monitor.work_area().size.to_logical::<f64>(scale);
                let (width, height) = default_size(area.width, area.height);
                if let Err(error) = window.set_size(LogicalSize::new(width, height)).and_then(|()| window.center()) {
                    eprintln!("设置主窗口默认尺寸失败：{error}");
                }
            }
        }
    }
    let window = window.clone();
    after_native_changes(move || {
        if let Err(error) = fit_to_work_area(&window) {
            eprintln!("调整主窗口可见区域失败：{error}");
            let _ = window.center();
        }
        after_native_changes(move || {
            if let Err(error) = window.restore_state(StateFlags::MAXIMIZED) {
                eprintln!("恢复主窗口最大化状态失败：{error}");
            }
            after_native_changes(move || {
                if let Err(error) = window.show().and_then(|()| window.set_focus()) {
                    eprintln!("显示主窗口失败：{error}");
                }
            });
        });
    });
}

fn after_native_changes(action: impl FnOnce() + Send + 'static) {
    // macOS 的 set_size/set_position 本身异步派发到 GCD 主队列，
    // 继续排入同一队列，保证读取边界、最大化、显示依次发生，无需固定延时。
    #[cfg(target_os = "macos")]
    dispatch2::DispatchQueue::main().exec_async(action);
    #[cfg(not(target_os = "macos"))]
    action();
}

pub fn save_before_close(window: &Window) {
    if window.label() != MAIN_WINDOW_LABEL {
        return;
    }
    // 前端备份后直接 destroy 窗口，必须在窗口仍存活时读取最终状态。
    if let Some(webview) = window.app_handle().get_webview_window(MAIN_WINDOW_LABEL) {
        if let Err(error) = save_geometry(&webview) {
            eprintln!("保存主窗口位置和尺寸失败：{error}");
        }
    }
    // 插件的退出保存同时覆盖系统退出、应用重启等路径。
    if let Err(error) = window.app_handle().save_window_state(SAVED_STATE) {
        eprintln!("保存主窗口状态失败：{error}");
    }
}

fn fit_to_work_area(window: &DesktopWebviewWindow) -> tauri::Result<()> {
    let monitor = window.current_monitor()?.or(window.primary_monitor()?);
    let Some(monitor) = monitor else {
        return Ok(());
    };
    let area = monitor.work_area();
    let inner = window.inner_size()?;
    let outer = window.outer_size()?;
    let chrome_width = outer.width.saturating_sub(inner.width);
    let chrome_height = outer.height.saturating_sub(inner.height);
    let scale = monitor.scale_factor();
    // 普通窗口留出少量屏幕边距；完全填满工作区在 macOS 会被识别为最大化，
    // 导致插件跳过普通尺寸更新。真正的最大化在下一阶段单独恢复。
    let margin = (16.0 * scale).ceil() as u32;
    let max_width = area
        .size
        .width
        .saturating_sub(chrome_width)
        .saturating_sub(margin)
        .max(1);
    let max_height = area
        .size
        .height
        .saturating_sub(chrome_height)
        .saturating_sub(margin)
        .max(1);

    // 高缩放或较小的屏幕上，最低尺寸不能大于当前显示器的可用区域。
    let min_width = ((MIN_WIDTH * scale).ceil() as u32).min(max_width);
    let min_height = ((MIN_HEIGHT * scale).ceil() as u32).min(max_height);
    window.set_min_size(Some(PhysicalSize::new(min_width, min_height)))?;
    let width = inner.width.clamp(min_width, max_width);
    let height = inner.height.clamp(min_height, max_height);
    if width != inner.width || height != inner.height {
        window.set_size(PhysicalSize::new(width, height))?;
    }

    let position = window.outer_position()?;
    let x = clamp_axis(
        position.x,
        area.position.x,
        area.size.width,
        width + chrome_width,
    );
    let y = clamp_axis(
        position.y,
        area.position.y,
        area.size.height,
        height + chrome_height,
    );
    if x != position.x || y != position.y {
        window.set_position(PhysicalPosition::new(x, y))?;
    }
    Ok(())
}

fn clamp_axis(position: i32, origin: i32, available: u32, extent: u32) -> i32 {
    let start = i64::from(origin);
    let end = start + i64::from(available.saturating_sub(extent));
    i64::from(position)
        .clamp(start, end)
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_size_fits_small_screens_and_caps_large_ones() {
        // 13 寸默认缩放（1280×832 点，可用高约 800）：开到可用区域的 85%，但不低于最小尺寸
        assert_eq!(default_size(1280.0, 800.0), (1100.0, 720.0));
        // 更多空间档（2560×1664 点）：85% 超过上限，按上限 1440×960
        assert_eq!(default_size(2560.0, 1630.0), (1440.0, 960.0));
        // 1680×1050 外接屏：按 85%
        assert_eq!(default_size(1680.0, 1020.0), (1428.0, 867.0));
        // 比最小尺寸还小的屏幕：最小尺寸让位给屏幕
        assert_eq!(default_size(1000.0, 600.0), (1000.0, 600.0));
    }

    #[test]
    fn geometry_round_trips_as_json() {
        let geometry = Geometry { width: 1280.5, height: 800.0, x: 40.0, y: 60.0 };
        let text = serde_json::to_string(&geometry).unwrap();
        assert_eq!(serde_json::from_str::<Geometry>(&text).unwrap(), geometry);
    }
}
