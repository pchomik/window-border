#![windows_subsystem = "windows"]

use dirs;
use regex::Regex;
use std::fs;
use toml::Value;
use windows::core::w;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// Configuration
// #8dbcff -> R=141, G=188, B=255
const BORDER_COLOR: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 141.0 / 255.0,
    g: 188.0 / 255.0,
    b: 1.0,
    a: 1.0,
};

// Global state
static mut OVERLAY_HWND: HWND = HWND(std::ptr::null_mut());
static mut BORDER_WIDTH_PX: i32 = 3;
static mut CORNER_RADIUS_PX: i32 = 0;
static mut IGNORED_REGEXES: Vec<Regex> = Vec::new();
static mut D2D_FACTORY: Option<ID2D1Factory> = None;

// Hardcoded list of window classes to ALWAYS skip (System UI)
const SYSTEM_IGNORE_CLASSES: &[&str] = &[
    "Windows.UI.Core.CoreWindow", // Start Menu, Search, Action Center
    "Shell_TrayWnd",              // Taskbar
    "Shell_SecondaryTrayWnd",     // Secondary Taskbar
    "Progman",                    // Program Manager (Desktop)
    "WorkerW",                    // Desktop background helper
];

fn load_config() {
    let mut width = 3;
    let mut radius = 0;
    let mut regexes = Vec::new();

    if let Some(config_dir) = dirs::config_dir() {
        let path = config_dir.join("Glint").join("config.toml");

        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(value) = content.parse::<Value>() {
                    if let Some(w) = value
                        .get("window_border_width")
                        .and_then(|v| v.as_integer())
                    {
                        width = w as i32;
                    }
                    if let Some(r) = value
                        .get("window_border_radius")
                        .and_then(|v| v.as_integer())
                    {
                        radius = r as i32;
                    }
                    // Compile regex patterns
                    if let Some(patterns) = value.get("ignored_windows").and_then(|v| v.as_array())
                    {
                        for pattern in patterns {
                            if let Some(pattern_str) = pattern.as_str() {
                                // Attempt to compile regex, ignore invalid ones
                                if let Ok(re) = Regex::new(pattern_str) {
                                    regexes.push(re);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    unsafe {
        BORDER_WIDTH_PX = width;
        CORNER_RADIUS_PX = radius;
        IGNORED_REGEXES = regexes;
    }
}

fn init_d2d() -> windows::core::Result<()> {
    unsafe {
        let options = D2D1_FACTORY_OPTIONS {
            debugLevel: D2D1_DEBUG_LEVEL_NONE,
        };
        let factory: ID2D1Factory =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&options))?;
        D2D_FACTORY = Some(factory);
    }
    Ok(())
}

fn main() -> windows::core::Result<()> {
    load_config();
    init_d2d()?;

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(None)?;
        let window_class = w!("glint_overlay");

        let wc = WNDCLASSW {
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hInstance: instance.into(),
            lpszClassName: window_class,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(def_window_proc),
            ..Default::default()
        };

        RegisterClassW(&wc);

        // WS_EX_LAYERED is required for UpdateLayeredWindow
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_TOOLWINDOW,
            window_class,
            w!(""),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        OVERLAY_HWND = hwnd;

        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );

        SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );

        update_overlay();

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
    if event == EVENT_OBJECT_LOCATIONCHANGE {
        let active = GetForegroundWindow();
        if hwnd != active || hwnd == OVERLAY_HWND {
            return;
        }
    }
    update_overlay();
}

// Helper to get window class name
unsafe fn get_class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let len = GetClassNameW(hwnd, &mut buffer);
    if len == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buffer[..len as usize])
}

// Helper to get window title
unsafe fn get_window_title(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd) + 1;
    if len <= 1 {
        return String::new();
    }
    let mut buffer = vec![0u16; len as usize];
    GetWindowTextW(hwnd, &mut buffer);
    String::from_utf16_lossy(&buffer[..buffer.len() - 1])
}

unsafe fn update_overlay() {
    let overlay_hwnd = OVERLAY_HWND;
    let active_hwnd = GetForegroundWindow();

    // Always hide if invalid or not visible
    if active_hwnd.0 == std::ptr::null_mut() || !IsWindowVisible(active_hwnd).as_bool() {
        hide_overlay(overlay_hwnd);
        return;
    }

    // --- Ignore Logic ---
    let class = get_class_name(active_hwnd);
    for sys_class in SYSTEM_IGNORE_CLASSES {
        if class == *sys_class {
            hide_overlay(overlay_hwnd);
            return;
        }
    }

    let title = get_window_title(active_hwnd);
    let window_info = format!("{} {}", title, class);

    for re in unsafe { &*std::ptr::addr_of!(IGNORED_REGEXES) } {
        if re.is_match(&window_info) {
            hide_overlay(overlay_hwnd);
            return;
        }
    }

    // --- Rendering Logic ---

    let is_maximized = IsZoomed(active_hwnd).as_bool();

    let mut rect = RECT::default();
    let result = DwmGetWindowAttribute(
        active_hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut rect as *mut _ as *mut _,
        std::mem::size_of::<RECT>() as u32,
    );

    if result.is_err() {
        let _ = GetWindowRect(active_hwnd, &mut rect);
    }

    // Check if window matches monitor or work area
    let monitor = MonitorFromWindow(active_hwnd, MONITOR_DEFAULTTONEAREST);
    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };

    if GetMonitorInfoW(monitor, &mut monitor_info).as_bool() {
        let m = monitor_info.rcMonitor;
        let w = monitor_info.rcWork;

        // Check if rect matches monitor (fullscreen) or work area (maximized)
        if (rect.left == m.left
            && rect.top == m.top
            && rect.right == m.right
            && rect.bottom == m.bottom)
            || (rect.left == w.left
                && rect.top == w.top
                && rect.right == w.right
                && rect.bottom == w.bottom)
        {
            hide_overlay(overlay_hwnd);
            return;
        }
    }

    let dpi = GetDpiForWindow(active_hwnd);
    let scale_factor = dpi as f32 / USER_DEFAULT_SCREEN_DPI as f32;
    let border_width = (BORDER_WIDTH_PX as f32 * scale_factor).ceil() as i32;
    let radius_px = if is_maximized {
        0
    } else {
        (CORNER_RADIUS_PX as f32 * scale_factor).ceil() as i32
    };

    // Inflate rect by border width so we draw OUTSIDE the window content (if possible)
    // Actually, usually we want it centered or inside?
    // User asked for "better border rendering".
    // Usually borders are drawn centered on the line.
    // Let's keep the previous logic: Draw around the window.

    // We need a canvas that covers the window + border.
    // Let's inflate by border_width.
    let mut overlay_rect = rect;
    overlay_rect.left -= border_width;
    overlay_rect.top -= border_width;
    overlay_rect.right += border_width;
    overlay_rect.bottom += border_width;

    let width = overlay_rect.right - overlay_rect.left;
    let height = overlay_rect.bottom - overlay_rect.top;

    draw_d2d_border(
        overlay_hwnd,
        width,
        height,
        border_width as f32,
        radius_px as f32,
        &overlay_rect,
    );
}

unsafe fn draw_d2d_border(
    hwnd: HWND,
    width: i32,
    height: i32,
    border_width: f32,
    radius: f32,
    screen_rect: &RECT,
) {
    // 1. Create a memory DC compatible with the screen
    let screen_dc = GetDC(None);
    let mem_dc = CreateCompatibleDC(Some(screen_dc));

    // 2. Create a 32-bit bitmap for alpha blending
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // Top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let bitmap = CreateDIBSection(Some(screen_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0);

    if bitmap.is_err() {
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
        return;
    }
    let bitmap = bitmap.unwrap();

    let old_bitmap = SelectObject(mem_dc, HGDIOBJ(bitmap.0));

    // 3. Initialize D2D Render Target tied to this DC
    if let Some(factory) = &*std::ptr::addr_of!(D2D_FACTORY) {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 0.0,
            dpiY: 0.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };

        let rt_result = factory.CreateDCRenderTarget(&props);

        if let Ok(rt) = rt_result {
            let rect = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            if rt.BindDC(mem_dc, &rect).is_ok() {
                rt.BeginDraw();
                rt.Clear(Some(&D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.0,
                }));

                if let Ok(brush) = rt.CreateSolidColorBrush(&BORDER_COLOR, None) {
                    // Draw Rounded Rectangle
                    // The stroke is centered on the path.
                    // We want the border to be exactly `border_width` thick.
                    // The rect should be inset by half the stroke width to stay within bounds.

                    let half_width = border_width / 2.0;
                    let draw_rect = D2D_RECT_F {
                        left: half_width,
                        top: half_width,
                        right: width as f32 - half_width,
                        bottom: height as f32 - half_width,
                    };

                    // Adjust radius for the inset
                    let draw_radius = radius;

                    let rounded_rect = D2D1_ROUNDED_RECT {
                        rect: draw_rect,
                        radiusX: draw_radius,
                        radiusY: draw_radius,
                    };

                    rt.DrawRoundedRectangle(&rounded_rect, &brush, border_width, None);
                }

                let _ = rt.EndDraw(None, None);
            }
        }
    }

    // 4. Update Layered Window
    let pt_src = POINT { x: 0, y: 0 };
    let pt_dst = POINT {
        x: screen_rect.left,
        y: screen_rect.top,
    };
    let size = SIZE {
        cx: width,
        cy: height,
    };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };

    let result = UpdateLayeredWindow(
        hwnd,
        Some(screen_dc),
        Some(&pt_dst),
        Some(&size),
        Some(mem_dc),
        Some(&pt_src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );
    if let Err(e) = result {
        println!("UpdateLayeredWindow failed: {:?}", e);
    }

    // Ensure visibility and z-order
    let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOPMOST),
        screen_rect.left,
        screen_rect.top,
        width,
        height,
        SWP_SHOWWINDOW | SWP_NOACTIVATE,
    );

    // Cleanup
    SelectObject(mem_dc, old_bitmap);
    let _ = DeleteObject(HGDIOBJ(bitmap.0));
    let _ = DeleteDC(mem_dc);
    ReleaseDC(None, screen_dc);
}

unsafe fn hide_overlay(hwnd: HWND) {
    let _ = SetWindowPos(
        hwnd,
        None,
        0,
        0,
        0,
        0,
        SWP_HIDEWINDOW | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
    );
}

unsafe extern "system" fn def_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
