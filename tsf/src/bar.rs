//! 浮条：宿主进程内的四格状态条（品牌标 | 中/英 | 上下叠放的 。和， | ☰）。
//!
//! 为什么在 DLL 里画、而不是独立进程（用户拍板 m09409「走A」，见对话与 ADR）：
//! - 搜狗、微软拼音都把状态条画在**宿主进程里**（每个宿主各一份 `SoPY_Status` 窗口），
//!   没有"另一个程序常驻"。切到输入法 `Activate` 就出现，切走 `Deactivate` 就消失。
//! - 独立进程（方案 B）的问题：用的时候进程照样在、冷启动 100–300ms 延迟、
//!   空闲退出时机难定、多应用并发抢拉起会堆出多个实例、托盘反复加删像故障。
//!
//! ## 护栏（这个窗口直接跑在别人进程里，必须比独立 exe 更小心）
//!
//! - 纯 GDI 自绘，不起线程、不碰词库、不调 COM、不在 wndproc 里做 IPC；
//! - `tsf/Cargo.toml` 的 `[profile.release] panic = "abort"` 让 `catch_unwind` 失效，
//!   所以 wndproc 里**绝不能 panic**：不 unwrap/expect、不越界索引、只用常量尺寸；
//! - 点击先写注册表、再靠消息循环重绘；状态每次 `paint` 现读 `state::snapshot()`，
//!   不缓存 —— 用 Shift 在应用里切了中英，浮条下次重绘就是最新的；
//! - 命中测试 / 尺寸是纯函数，可单测。
//!
//! ## 一个进程里可能有好几条（2026-10-30 修）
//!
//! TSF 是**按线程**激活的：一个进程里几条线程各自 `Activate` 一次，就各自建一个窗口。
//! 之前窗口句柄存在进程级全局里，只记得最后一个 → 别的线程的窗口没人 `Deactivate`，
//! 于是屏幕上叠出好几条浮条。现在句柄放 `thread_local`，谁建谁关。
//!
//! 另外 `Deactivate` 只在"这条线程不用这个输入法了"时才来，用户 Alt+Tab 切到别的程序
//! 并不会触发 —— 所以还有一个 600ms 的 `WM_TIMER` 看门狗：本进程不是前台就自动收起，
//! 回到前台再自动放出来（只在"自动收起过"时才放，用户手动关掉输入法不会被它复活）。

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, AlphaBlend, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush,
    DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW,
    EndPaint, FillRect, GdiFlush, HBITMAP, HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DI_NORMAL, DefWindowProcW, DrawIconEx,
    EnumWindows, GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindowRect,
    GetWindowThreadProcessId, HICON, IDC_ARROW, IDI_APPLICATION, IMAGE_ICON, IsWindowVisible,
    KillTimer, LR_LOADFROMFILE, LoadCursorW, LoadIconW, LoadImageW, MA_NOACTIVATE, MoveWindow,
    PostMessageW, RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE, SetTimer,
    ShowWindow, SystemParametersInfoW, ULW_ALPHA, UpdateLayeredWindow, WM_APP, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS_NAME: &str = "inklink-bar";
/// 弹菜单走这条自投递消息，**不在鼠标消息里直接弹**：菜单要自己跑一个嵌套消息循环，
/// 直接在 `WM_LBUTTONUP` 里进会重入 wndproc，历史教训是崩在 user32。投到队列尾部再弹就干净。
const WM_OPEN_MENU: u32 = WM_APP + 20;
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

/// 看门狗的定时器 ID（`WM_TIMER` 的 wParam）。
const TIMER_WATCH: usize = 1;
/// 看门狗周期：600ms。再快没必要，再慢切走程序后能明显看到浮条赖着不走。
const WATCH_MS: u32 = 600;

// 本线程的浮条窗口。**必须是 thread_local**：TSF 按线程激活，一个进程里可能有好几条
// 线程各建一个窗口；放进程级全局只记得最后一个，其余窗口就永远没人关（叠出好几条）。
thread_local! {
    static HWND_CELL: Cell<isize> = const { Cell::new(0) };
}
static REGISTERED: AtomicBool = AtomicBool::new(false);
static HOVER: AtomicI32 = AtomicI32::new(-1);
static DRAG_ARMED: AtomicBool = AtomicBool::new(false);
static DRAGGING: AtomicBool = AtomicBool::new(false);
/// 是不是被看门狗**自动**收起来的。用户自己关掉输入法（`Deactivate`）不会被复活。
static AUTO_HIDDEN: AtomicBool = AtomicBool::new(false);
/// 这条线程的输入法是不是"开着"（`Activate` 置真、`Deactivate` 置假）。
/// 看门狗只复活 `WANTED` 的浮条 —— 用户手动关掉输入法后不该被它弹回来。
static WANTED: AtomicBool = AtomicBool::new(false);
/// `EnumWindows` 回调的临时结果：进程里还有没有别的可见浮条。
static ANY_BAR: AtomicBool = AtomicBool::new(false);
static FONT: AtomicIsize = AtomicIsize::new(0);

// 上一次画出来的状态。看门狗拿它跟注册表比，不一样就重画 ——
// 状态是全局的，别的程序里点了中/英或标点，这条浮条也得跟着变。
static PAINTED_VALID: AtomicBool = AtomicBool::new(false);
static PAINTED_CHINESE: AtomicBool = AtomicBool::new(true);
static PAINTED_PUNCT: AtomicBool = AtomicBool::new(true);

// 96 DPI 下的设计尺寸，实际按 DPI 缩放。
// 整体节奏照搜狗那条浮条：四格等距，不做逐格色块。
// 高度 36（原来 44，用户嫌上下太宽）：格高 28 刚好放下 24px 的品牌标，
// 标点格里「。」「，」上下叠两行也还有 2~3px 的呼吸。
const PAD: i32 = 4;
const ITEM_H: i32 = 28;
const RADIUS: i32 = 8;
const GAP: i32 = 4;
/// 第一格是品牌标（深蓝圆角方块 + 白色 i），点它等同点 ☰。
const W_BRAND: i32 = 30;
/// 品牌标和状态之间那道竖线（和搜狗一样），两边各留 6。
const SEP_GAP: i32 = 6;
const SEP_W: i32 = 1;
const W_MODE: i32 = 34;
const W_PUNCT: i32 = 34;
const W_MENU: i32 = 30;

// COLORREF 是 0x00BBGGRR（低字节是红）
/// 整条是一块白底，不再给每格画独立的灰方块 —— 那样四坨色块很笨重。
const C_BG: u32 = 0x00_FF_FF_FF;
/// 描边 #E2E2E8：白底浮条放到白色窗口上，没有一条清楚的边界就"化"进去了。
const C_BORDER: u32 = 0x00_E8_E2_E2;
/// 悬停垫底 #F2F2F6（比原来更淡，只做"鼠标在这儿"的暗示，不抢视觉）。
const C_HOVER: u32 = 0x00_F6_F2_F2;
/// 常态文字/图形 #3A3A3E。
const C_TEXT: u32 = 0x00_3E_3A_3A;
/// 关闭/未激活 #A2A2A8（比常态浅，一眼能分出开和关）。
const C_DIM: u32 = 0x00_A8_A2_A2;
/// 激活色 = 品牌深蓝 #062657（原来是搜狗那种橙，2026-10-29 按用户要求换掉）。
const C_ACCENT: u32 = 0x00_57_26_06;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(v: i32) -> i32 {
    let dpi = unsafe { GetDpiForSystem() };
    let dpi = if dpi == 0 { 96 } else { dpi as i32 };
    (v * dpi + 48) / 96
}

/// 四个可点的地方：品牌标、中文/英文、中文标点/半角、菜单。纯函数，可单测。
///
/// 品牌标和第一格状态之间多留一道竖线（`SEP_W` 宽，两边各 `SEP_GAP`），
/// 和搜狗浮条一样给"品牌 | 状态"一个视觉分隔。竖线本身不是可点区域。
fn item_rects() -> [RECT; 4] {
    let pad = scale(PAD);
    let gap = scale(GAP);
    let h = scale(ITEM_H);
    let sep_total = scale(SEP_GAP) * 2 + scale(SEP_W);
    let widths = [scale(W_BRAND), scale(W_MODE), scale(W_PUNCT), scale(W_MENU)];
    let mut rects = [RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }; 4];
    let mut x = pad;
    for (i, w) in widths.iter().enumerate() {
        rects[i] = RECT {
            left: x,
            top: pad,
            right: x + w,
            bottom: pad + h,
        };
        x += w + gap;
        if i == 0 {
            x += sep_total;
        }
    }
    rects
}

fn window_size() -> (i32, i32) {
    let last = item_rects()[3];
    let pad = scale(PAD);
    (last.right + pad, last.bottom + pad)
}

fn hit_test(x: i32, y: i32) -> i32 {
    for (i, r) in item_rects().iter().enumerate() {
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return i as i32;
        }
    }
    -1
}

/// 重画整条并交给系统合成（分层窗口没有 `WM_PAINT` 那一套，改了状态就直接调这个）。
/// 返回 `UpdateLayeredWindow` 是否成功；失败时窗口会是空的，记一行日志方便排查。
fn redraw(hwnd: HWND) -> bool {
    let ok = unsafe { present(hwnd) };
    if !ok {
        crate::log_line("bar: UpdateLayeredWindow failed");
    }
    ok
}

fn our_hwnd() -> HWND {
    HWND_CELL.with(|c| c.get()) as HWND
}

fn set_our_hwnd(hwnd: HWND) {
    HWND_CELL.with(|c| c.set(hwnd as isize));
}

/// 前台窗口是不是我们自己这个进程的窗口。
fn foreground_is_self() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if (fg as isize) == 0 {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, &mut pid);
        pid == GetCurrentProcessId()
    }
}

/// 这个窗口的类名是不是我们的浮条。
fn is_our_class(hwnd: HWND) -> bool {
    unsafe {
        let mut buf = [0u16; 48];
        let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if n <= 0 {
            return false;
        }
        buf[..n as usize]
            .iter()
            .copied()
            .eq(CLASS_NAME.encode_utf16())
    }
}

unsafe extern "system" fn on_find_bar(hwnd: HWND, lparam: LPARAM) -> i32 {
    let me = lparam as HWND;
    if hwnd == me || !is_our_class(hwnd) {
        return 1;
    }
    if IsWindowVisible(hwnd) != 0 {
        ANY_BAR.store(true, Ordering::Relaxed);
        return 0;
    }
    1
}

unsafe extern "system" fn on_hide_bar(hwnd: HWND, lparam: LPARAM) -> i32 {
    let me = lparam as HWND;
    if hwnd != me && is_our_class(hwnd) && IsWindowVisible(hwnd) != 0 {
        ShowWindow(hwnd, SW_HIDE);
    }
    1
}

/// 进程里是否已经有别的浮条在显示。同一个程序可能有好几条 TSF 线程、各建一个窗口，
/// 屏幕上只能留一条 —— 后建的让位给已经在显示的，别叠成一堆。
fn any_other_bar_visible(me: HWND) -> bool {
    ANY_BAR.store(false, Ordering::Relaxed);
    unsafe { EnumWindows(Some(on_find_bar), me as LPARAM) };
    ANY_BAR.load(Ordering::Relaxed)
}

/// 先把自己以外的浮条收起来（同一个程序里只留一条）。
fn hide_other_bars(me: HWND) {
    unsafe { EnumWindows(Some(on_hide_bar), me as LPARAM) };
}

/// 浮条出现（`Activate` 时由 lib.rs 调，在宿主 UI 线程上，直接 ShowWindow 即可）。
pub fn show() {
    unsafe {
        ensure_window();
        let hwnd = our_hwnd();
        if (hwnd as isize) == 0 {
            return;
        }
        WANTED.store(true, Ordering::SeqCst);
        AUTO_HIDDEN.store(false, Ordering::SeqCst);
        hide_other_bars(hwnd);
        if IsWindowVisible(hwnd) == 0 {
            // 分层窗口要先有内容再显示，否则会先闪一下空白（或者干脆看不见）。
            redraw(hwnd);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            crate::log_line("bar show");
        }
        // 看门狗只在窗口存在期间跑；进程切到后台就靠它把浮条收走。
        SetTimer(hwnd, TIMER_WATCH, WATCH_MS, None);
    }
}

/// 浮条消失（`Deactivate` 时调，也就是用户真的把这个输入法关了）。
pub fn hide() {
    unsafe {
        let hwnd = our_hwnd();
        if (hwnd as isize) == 0 {
            return;
        }
        WANTED.store(false, Ordering::SeqCst);
        AUTO_HIDDEN.store(false, Ordering::SeqCst);
        if IsWindowVisible(hwnd) != 0 {
            ShowWindow(hwnd, SW_HIDE);
            crate::log_line("bar hide");
        }
        KillTimer(hwnd, TIMER_WATCH);
    }
}

/// 重绘（Shift 在应用里切了中英之后，lib.rs 调这个让浮条跟一下）。
pub fn refresh() {
    unsafe {
        let hwnd = our_hwnd();
        if (hwnd as isize) == 0 {
            return;
        }
        if IsWindowVisible(hwnd) != 0 {
            redraw(hwnd);
        }
    }
}

/// 注册窗口类 + 创建窗口（幂等，按线程）。位置只取一次：拖动之后窗口一直在，下次 show 不重读。
fn ensure_window() {
    unsafe {
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let name = wide(CLASS_NAME);
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: std::ptr::null_mut(),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: name.as_ptr(),
            };
            let _ = RegisterClassW(&wc);
        }

        if (our_hwnd() as isize) == 0 {
            let name = wide(CLASS_NAME);
            // WS_EX_LAYERED：整条按像素带 alpha 交给系统合成（`UpdateLayeredWindow`），
            // 圆角边缘才能抗锯齿。原来用 `SetWindowRgn` 裁圆角，区域只有"在/不在"两种，
            // 四个角是一圈台阶 —— 用户说的"圆角这里锯齿很粗糙"。
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                name.as_ptr(),
                name.as_ptr(),
                WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                10,
                10,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if hwnd.is_null() {
                crate::log_line("bar: CreateWindowExW failed");
                return;
            }
            set_our_hwnd(hwnd);
            let (w, h) = window_size();
            let (x, y) = crate::state::read_pos().unwrap_or_else(default_position);
            MoveWindow(hwnd, x, y, w, h, 0);
            crate::log_line(&format!("bar: created {w}x{h} at {x},{y}"));
        }
    }
}

fn default_position() -> (i32, i32) {
    let (w, h) = window_size();
    let mut area: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
    }
    // 右下角往上多让 44px：搜狗/微软的浮条也在右下角（本机实测默认位置会被
    // 搜狗的 `SoPY_Status` 完全盖住）。用户拖走一次就记住位置。
    (area.right - w - 24, area.bottom - h - 24 - 44)
}

/// 第一格里的品牌标。图标只加载一次（`LoadImageW` 会读盘），之后一直复用。
static BADGE: AtomicIsize = AtomicIsize::new(0);

/// `installer\inklink.ico` 里有的帧（`tools\draw-logo.py` 每档按自己的尺寸原生渲染）。
const BADGE_FRAMES: [i32; 7] = [16, 20, 24, 32, 40, 48, 64];

/// 品牌标画多大：取 ICO 里**现成的**、不超过设计尺寸的最大一帧。
/// 100% → 24，125% → 24，150% → 32，200% → 48。宁可小一两像素，也不让系统重采样。
fn badge_side() -> i32 {
    pick_frame(scale(W_BRAND - 6))
}

/// `badge_side()` 的纯函数部分（不读 DPI，方便单测）：
/// 在 `BADGE_FRAMES` 里挑一个不超过 `want` 的最大帧，一个都不满足就用最小帧。
fn pick_frame(want: i32) -> i32 {
    BADGE_FRAMES
        .iter()
        .copied()
        .filter(|&s| s <= want)
        .max()
        .unwrap_or(BADGE_FRAMES[0])
}

fn load_badge(side: i32) -> HICON {
    // 用 DLL 同目录下的 `inklink.ico`（安装器一定拷过去），没有就退回系统默认图标。
    let dll = crate::registry::dll_path();
    let dir = std::path::Path::new(&dll)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = wide(&format!("{dir}\\inklink.ico"));
    // **必须按要画的尺寸要帧**：原来传 `0, 0`，`LoadImageW` 拿的是文件里第一帧（16×16），
    // 再由 `DrawIconEx` 拉到 24 —— 浮条上的品牌标糊成一团就是这么来的。
    let h = unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            path.as_ptr(),
            IMAGE_ICON,
            side,
            side,
            LR_LOADFROMFILE,
        )
    };
    if !h.is_null() {
        return h as HICON;
    }
    unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) as HICON }
}

unsafe fn draw_badge(hdc: HDC, r: &RECT) {
    // 图标比格子小一圈，留出边距；尺寸正好是 ICO 里的某一帧，1:1 贴上去，不经过缩放。
    let side = badge_side();
    let mut h = BADGE.load(Ordering::Relaxed);
    if h == 0 {
        let icon = load_badge(side);
        if icon.is_null() {
            return;
        }
        h = icon as isize;
        BADGE.store(h, Ordering::Relaxed);
    }
    let x = r.left + (r.right - r.left - side) / 2;
    let y = r.top + (r.bottom - r.top - side) / 2;
    DrawIconEx(
        hdc,
        x,
        y,
        h as HICON,
        side,
        side,
        0,
        std::ptr::null_mut(),
        DI_NORMAL,
    );
}

fn inset(r: &RECT, d: i32) -> RECT {
    RECT {
        left: r.left + d,
        top: r.top + d,
        right: r.right - d,
        bottom: r.bottom - d,
    }
}

// ── 整条的画布：32 位 DIB + 按像素 alpha ────────────────────────────────────
//
// GDI 的 `RoundRect`/`CreateRoundRectRgn` 都没有抗锯齿，圆角是一圈台阶；1px 的描边
// 在角上只剩几个孤立像素，看着"锯齿很粗糙"。现在整条先画进一张 32 位 DIB，
// 外形（圆角 + 描边）和悬停垫底按圆角矩形的距离场算覆盖率，再用 `UpdateLayeredWindow`
// 带 alpha 交给系统合成 —— 角上的过渡像素是半透明的，叠在什么桌面上都是平滑的。

/// 一张自上而下的 32 位 DIB，选进内存 DC。`Drop` 时归还 GDI 对象（不 panic）。
struct Canvas {
    dc: HDC,
    dib: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Canvas {
    unsafe fn new(w: i32, h: i32) -> Option<Canvas> {
        if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
            return None;
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // 自上而下
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..std::mem::zeroed()
            },
            bmiColors: [std::mem::zeroed()],
        };
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            return None;
        }
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if dib.is_null() || bits.is_null() {
            DeleteDC(dc);
            return None;
        }
        let old = SelectObject(dc, dib as HGDIOBJ);
        Some(Canvas {
            dc,
            dib,
            old,
            bits: bits as *mut u32,
            w,
            h,
        })
    }

    /// 直接改像素（`0xAARRGGBB`）。先 `GdiFlush`：GDI 的绘制可能还攒在批里没落到位图上。
    unsafe fn pixels(&mut self) -> &mut [u32] {
        GdiFlush();
        std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize)
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.dib as HGDIOBJ);
            DeleteDC(self.dc);
        }
    }
}

/// 圆角矩形的有向距离（像素）：< 0 在里面。`(x0,y0)-(x1,y1)` 是外框，`r` 是圆角半径。
/// 纯函数，可单测。
fn round_rect_sdf(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (hx, hy) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let r = r.min(hx).min(hy).max(0.0);
    let qx = (px - cx).abs() - (hx - r);
    let qy = (py - cy).abs() - (hy - r);
    let outside = (qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// 距离 → 覆盖率：一个像素宽的线性过渡。
fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// COLORREF（0x00BBGGRR）拆成 (r, g, b)。
fn rgb_of(c: u32) -> (f32, f32, f32) {
    (
        (c & 0xFF) as f32,
        ((c >> 8) & 0xFF) as f32,
        ((c >> 16) & 0xFF) as f32,
    )
}

/// 在不透明的画布上叠一块抗锯齿圆角（悬停垫底）。
fn fill_round_aa(px: &mut [u32], w: i32, h: i32, r: &RECT, radius: f32, color: u32) {
    let (cr, cg, cb) = rgb_of(color);
    let (x0, y0, x1, y1) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
    for y in r.top.max(0)..r.bottom.min(h) {
        for x in r.left.max(0)..r.right.min(w) {
            let a = coverage(round_rect_sdf(
                x as f32 + 0.5,
                y as f32 + 0.5,
                x0,
                y0,
                x1,
                y1,
                radius,
            ));
            if a <= 0.0 {
                continue;
            }
            let i = (y * w + x) as usize;
            let p = px[i];
            let (pr, pg, pb) = (
                ((p >> 16) & 0xFF) as f32,
                ((p >> 8) & 0xFF) as f32,
                (p & 0xFF) as f32,
            );
            let mix = |o: f32, n: f32| (o + (n - o) * a + 0.5) as u32;
            px[i] = (mix(pr, cr) << 16) | (mix(pg, cg) << 8) | mix(pb, cb);
        }
    }
}

/// 最后一步：按外形给整张图上 alpha，并把描边混进去。输出是预乘 alpha（`AlphaBlend`/
/// `UpdateLayeredWindow` 要的格式）。
///
/// 每个像素：`outer` = 在整条圆角里的覆盖率（= alpha），`inner` = 在往里缩一圈描边宽度的
/// 圆角里的覆盖率；内容色占 `inner`，描边色占 `outer - inner`。两条边都是抗锯齿的。
fn apply_frame(px: &mut [u32], w: i32, h: i32, radius: f32, border_w: f32, border: u32) {
    let (br, bg, bb) = rgb_of(border);
    let (fw, fh) = (w as f32, h as f32);
    for y in 0..h {
        for x in 0..w {
            let d = round_rect_sdf(x as f32 + 0.5, y as f32 + 0.5, 0.0, 0.0, fw, fh, radius);
            let outer = coverage(d);
            let inner = coverage(d + border_w);
            let i = (y * w + x) as usize;
            let p = px[i];
            let (cr, cg, cb) = (
                ((p >> 16) & 0xFF) as f32,
                ((p >> 8) & 0xFF) as f32,
                (p & 0xFF) as f32,
            );
            let ring = outer - inner;
            let ch = |c: f32, b: f32| ((c * inner + b * ring) + 0.5).min(255.0) as u32;
            let a = (outer * 255.0 + 0.5) as u32;
            px[i] = (a << 24) | (ch(cr, br) << 16) | (ch(cg, bg) << 8) | ch(cb, bb);
        }
    }
}

/// 直角实心块（分隔竖线、☰ 的三道横线）。
unsafe fn fill_rect(hdc: HDC, r: &RECT, color: u32) {
    let brush: HBRUSH = CreateSolidBrush(color);
    FillRect(hdc, r, brush);
    DeleteObject(brush as HGDIOBJ);
}

unsafe fn draw_text_centered(hdc: HDC, r: &RECT, text: &str, fg: u32, font: HGDIOBJ) {
    let old_font = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, fg);
    // **必须补 NUL 再传 `len-1`**：空字符串时 `encode_utf16().collect()` 出来的是零长
    // `Vec`，`as_ptr()` 是悬垂指针（值恰好是 2 = align_of::<u16>()）。以前这里直接传
    // `buf.as_ptr()` + `len`，`DrawTextW` 即使 `cchText == 0` 也会去读 0x2，
    // 宿主进程就地 AV：`c0000005`，`ExceptionInformation[1] = 0x2`，
    // 报在 `user32.dll+0x24702`，从窗口回调逃出去就成了 `c000041d`。
    // 见 `C:\Users\Administrator\AppData\Local\CrashDumps\inklink-ui.exe.2004.dmp`
    // 与 `HANDOFF.md`「踩过的坑」。
    let buf: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut tr = *r;
    DrawTextW(
        hdc,
        buf.as_ptr(),
        (buf.len() - 1) as i32,
        &mut tr,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    SelectObject(hdc, old_font);
}

/// 菜单格的三道横线。不用 `☰` 字形：字体缺字会画成豆腐块，手画的永远一样清楚。
unsafe fn draw_menu_glyph(hdc: HDC, r: &RECT) {
    let w = scale(12);
    let th = scale(2).max(1);
    let gap = scale(4);
    let cx = (r.left + r.right) / 2;
    let total = th * 3 + gap * 2;
    let y0 = r.top + (r.bottom - r.top - total) / 2;
    for i in 0..3 {
        let y = y0 + i * (th + gap);
        let bar = RECT {
            left: cx - w / 2,
            top: y,
            right: cx - w / 2 + w,
            bottom: y + th,
        };
        fill_rect(hdc, &bar, C_TEXT);
    }
}

fn get_font() -> HGDIOBJ {
    make_font(&FONT, 17)
}

fn make_font(slot: &AtomicIsize, px: i32) -> HGDIOBJ {
    unsafe {
        let existing = slot.load(Ordering::SeqCst);
        if existing != 0 {
            return existing as HGDIOBJ;
        }
        let face = wide("Microsoft YaHei UI");
        let f = CreateFontW(
            -scale(px),
            0,
            0,
            0,
            600,
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            // 4 = ANTIALIASED_QUALITY（灰度抗锯齿）。
            // 不用 5 = CLEARTYPE：小字号 + 彩色文字时 ClearType 会在笔画边缘拉出
            // 橙/蓝色边（用户看到的"字发花、脏"就是这个），灰度 AA 在浮条这种小控件上更干净。
            4,
            0,
            face.as_ptr(),
        );
        slot.store(f as isize, Ordering::SeqCst);
        f as HGDIOBJ
    }
}

// ── 标点格的符号：抗锯齿自绘 ─────────────────────────────────────────────────
//
// GDI 的 `Ellipse`/`Polygon` 没有抗锯齿：句号圈只有 1px 的硬线、逗号尾巴一圈台阶，
// 用户的原话是"太单薄、一条线、边缘很粗糙"。字体也不行（全角标点在 14px 下只剩一粒灰点）。
// 所以这里按**有向距离场**自己算每个像素的覆盖率：距离 < -0.5px 全涂、> 0.5px 不涂，
// 中间线性过渡 —— 就是一个像素宽的抗锯齿边。算好后放进 32 位预乘 alpha 的 DIB，
// `AlphaBlend` 叠到底色上（白底或悬停底都对）。纯计算 + GDI，不起线程、不碰 COM。

/// 句号/逗号的形状，坐标单位是**像素**（已经按 DPI 缩放）。
#[derive(Clone, Copy)]
enum Glyph {
    /// 空心圆环：圆心、环中线半径、半个线宽。
    Ring { cx: f32, cy: f32, r: f32, hw: f32 },
    /// 实心圆点。
    Dot { cx: f32, cy: f32, r: f32 },
    /// 逗号：圆头 + 一条往左下收尖的弯尾。`(cx, cy)` 是圆头中心，`k` 是 DPI 缩放。
    Comma { cx: f32, cy: f32, k: f32 },
}

fn dist(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    ((ax - bx) * (ax - bx) + (ay - by) * (ay - by)).sqrt()
}

/// 从 a（半径 ra）到 b（半径 rb）、半径线性变化的一段笔画的近似距离场。
fn taper(px: f32, py: f32, a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    dist(px, py, a.0 + dx * t, a.1 + dy * t) - (a.2 + (b.2 - a.2) * t)
}

/// 有向距离（像素）：< 0 在形状里面。纯函数，可单测。
fn glyph_sdf(g: Glyph, px: f32, py: f32) -> f32 {
    match g {
        Glyph::Ring { cx, cy, r, hw } => (dist(px, py, cx, cy) - r).abs() - hw,
        Glyph::Dot { cx, cy, r } => dist(px, py, cx, cy) - r,
        Glyph::Comma { cx, cy, k } => {
            // 96 DPI 下的设计：圆头 r=2.1（和句号环的外径一个量级，两个符号才不打架）；
            // 尾巴从圆头右下起笔、垂直落下再往左下甩出去，三段半径 1.5 → 1.0 → 0.4，
            // 收成尖但不是"钉子"。总高约 7.9，比原来 9.2 收了一档（用户说句号太大、
            // 头重脚轻，句号收小之后逗号不跟着收就变成下重上轻）。
            let p = |x: f32, y: f32, r: f32| (cx + x * k, cy + y * k, r * k);
            let head = dist(px, py, cx, cy) - 2.1 * k;
            let a = p(0.6, 0.5, 1.5);
            let b = p(0.8, 3.0, 1.0);
            let c = p(-1.4, 5.4, 0.4);
            head.min(taper(px, py, a, b)).min(taper(px, py, b, c))
        }
    }
}

/// 把若干形状按覆盖率画进 `area`（预乘 alpha + `AlphaBlend`），颜色是 COLORREF。
unsafe fn draw_glyphs_aa(hdc: HDC, area: &RECT, glyphs: &[Glyph], color: u32) {
    let w = area.right - area.left;
    let h = area.bottom - area.top;
    if w <= 0 || h <= 0 || w > 512 || h > 512 {
        return;
    }
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // 自上而下
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        },
        bmiColors: [std::mem::zeroed()],
    };
    let mem = CreateCompatibleDC(hdc);
    if mem.is_null() {
        return;
    }
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let dib = CreateDIBSection(
        mem,
        &bmi,
        DIB_RGB_COLORS,
        &mut bits,
        std::ptr::null_mut(),
        0,
    );
    if dib.is_null() || bits.is_null() {
        DeleteDC(mem);
        return;
    }
    let old = SelectObject(mem, dib as HGDIOBJ);

    let (r, g, b) = (color & 0xFF, (color >> 8) & 0xFF, (color >> 16) & 0xFF);
    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            // 像素中心，换回 hdc 坐标
            let fx = (area.left + x) as f32 + 0.5;
            let fy = (area.top + y) as f32 + 0.5;
            let d = glyphs
                .iter()
                .map(|&gl| glyph_sdf(gl, fx, fy))
                .fold(f32::INFINITY, f32::min);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            let a = (cov * 255.0 + 0.5) as u32;
            // 预乘：颜色分量 × alpha / 255
            let pr = (r * a + 127) / 255;
            let pg = (g * a + 127) / 255;
            let pb = (b * a + 127) / 255;
            px[(y * w + x) as usize] = (a << 24) | (pr << 16) | (pg << 8) | pb;
        }
    }

    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    AlphaBlend(hdc, area.left, area.top, w, h, mem, 0, 0, w, h, blend);
    SelectObject(mem, old);
    DeleteObject(dib as HGDIOBJ);
    DeleteDC(mem);
}

/// 标点格的两个符号，上下叠放（上句号、下逗号，和搜狗那条一样）。
///
/// 中文标点：空心圈「。」，线宽 1.9px（原来 1px 硬线，看着单薄）；
/// 半角：实心小点「.」—— 一眼能分出是哪种。逗号两种状态一样。
fn punct_glyphs(cell: &RECT, chinese_punct: bool) -> [Glyph; 2] {
    let k = dpi_k();
    let cx = (cell.left + cell.right) as f32 / 2.0;
    let mid = (cell.top + cell.bottom) as f32 / 2.0;
    // 句号外径 6.4px，和逗号圆头（直径 4.6px）只差一档 —— 原来外径 8.7px，
    // 比逗号大出一倍，看着头重脚轻。逗号尾巴往下多伸 ~7px，所以两者整体往上提，
    // 让"圈 + 逗号头"这两团墨落在格子正中，细尾巴不算重量。
    let upper = mid - 4.6 * k;
    let lower = mid + 2.6 * k;
    let top = if chinese_punct {
        Glyph::Ring {
            cx,
            cy: upper,
            r: 2.4 * k,
            hw: 0.8 * k,
        }
    } else {
        Glyph::Dot {
            cx,
            cy: upper,
            r: 1.8 * k,
        }
    };
    [top, Glyph::Comma { cx, cy: lower, k }]
}

/// DPI 缩放系数（96 DPI = 1.0），给浮点几何用。
fn dpi_k() -> f32 {
    scale(96) as f32 / 96.0
}

/// 把整条画进画布（不碰窗口）。测试用同一份代码渲预览图，改配色改布局不用先部署。
///
/// 顺序：整块白底（GDI）→ 悬停垫底（抗锯齿，直接改像素）→ 图标/文字/符号（GDI）→
/// 外形 alpha + 描边（直接改像素）。GDI 往 32 位 DIB 里画东西不管 alpha 字节，
/// 所以 alpha 只在最后一步统一算。
///
/// `hover` 是悬停在哪一格（-1 = 没有）。作为参数传进来而不是在这里读 `HOVER`：
/// 预览图测试要画悬停态，又不能去改全局（别的测试在并行跑）。
unsafe fn paint_bar(c: &mut Canvas, hover: i32) {
    let (w, h) = (c.w, c.h);
    let client = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    fill_rect(c.dc, &client, C_BG);

    let font = get_font();
    let rects = item_rects();
    // 每次现读：Shift 在别的动作里切了中英，这里立刻反映；不缓存。
    let (chinese, punct) = crate::state::snapshot();
    PAINTED_CHINESE.store(chinese, Ordering::Relaxed);
    PAINTED_PUNCT.store(punct, Ordering::Relaxed);
    PAINTED_VALID.store(true, Ordering::Relaxed);

    // 悬停只在当前格后面垫一层浅色圆角，不改文字颜色。
    // 只缩 1px：格高 28、品牌标 24，缩多了垫底会被品牌标整个盖住，悬停没反馈。
    if (0..4).contains(&hover) {
        let r = inset(&rects[hover as usize], scale(1));
        fill_round_aa(c.pixels(), w, h, &r, scale(6) as f32, C_HOVER);
    }

    let hdc = c.dc;
    // 品牌标和状态之间的分隔竖线。
    let sep_x = rects[0].right + scale(SEP_GAP);
    let sep = RECT {
        left: sep_x,
        top: rects[0].top + scale(7),
        right: sep_x + scale(SEP_W),
        bottom: rects[0].bottom - scale(7),
    };
    fill_rect(hdc, &sep, C_BORDER);

    // 第一格：品牌标（浮条里唯一一个"重"的元素，和搜狗那个橙色 S 一个位置）
    draw_badge(hdc, &rects[0]);

    // 第二格：中/英。开=品牌深蓝，关=浅灰。
    let (mode_text, mode_fg) = if chinese {
        ("中", C_ACCENT)
    } else {
        ("英", C_DIM)
    };
    draw_text_centered(hdc, &rects[1], mode_text, mode_fg, font);

    // 第三格：中文标点 / 半角。像搜狗那样**上下叠放**（上句号、下逗号）。
    // 两个符号都用 GDI 画形状而不是打字体 —— 15px 字号下全角标点只剩一粒灰点。
    let cell = rects[2];
    let punct_fg = if punct { C_ACCENT } else { C_DIM };
    draw_glyphs_aa(hdc, &cell, &punct_glyphs(&cell, punct), punct_fg);

    // 第四格：菜单。
    draw_menu_glyph(hdc, &rects[3]);

    // 外形：圆角 + 描边，都抗锯齿。描边 1px（高 DPI 跟着放大）。
    let border_w = scale(1).max(1) as f32;
    apply_frame(c.pixels(), w, h, scale(RADIUS) as f32, border_w, C_BORDER);
}

/// 画一帧并交给系统合成。窗口尺寸取 `window_size()`，位置不动（`pptDst` 传空）。
unsafe fn present(hwnd: HWND) -> bool {
    let (w, h) = window_size();
    let Some(mut c) = Canvas::new(w, h) else {
        return false;
    };
    paint_bar(&mut c, HOVER.load(Ordering::Relaxed));
    let size = SIZE { cx: w, cy: h };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    UpdateLayeredWindow(
        hwnd,
        std::ptr::null_mut(),
        std::ptr::null(),
        &size,
        c.dc,
        &src,
        0,
        &blend,
        ULW_ALPHA,
    ) != 0
}

/// 分层窗口正常不会收到 `WM_PAINT`；万一来了，验证掉区域并重画一帧，免得它反复来。
unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    BeginPaint(hwnd, &mut ps);
    EndPaint(hwnd, &ps);
    present(hwnd);
}

fn toggle_mode(hwnd: HWND) {
    let on = !crate::state::chinese();
    crate::state::set_chinese(on);
    crate::log_line(&format!("bar toggle chinese -> {on}"));
    redraw(hwnd);
}

fn toggle_punct(hwnd: HWND) {
    let on = !crate::state::punctuation();
    crate::state::set_punctuation(on);
    crate::log_line(&format!("bar toggle punct -> {on}"));
    redraw(hwnd);
}

struct DragState {
    cursor: POINT,
    window: POINT,
    pressed: i32,
}

static DRAG: Mutex<Option<DragState>> = Mutex::new(None);

fn handle_menu(hwnd: HWND, cmd: u32) {
    match cmd {
        crate::menu::CMD_TOGGLE_MODE => toggle_mode(hwnd),
        crate::menu::CMD_TOGGLE_PUNCT => toggle_punct(hwnd),
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_MOUSEMOVE => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let hit = hit_test(x, y);
            if hit != HOVER.load(Ordering::Relaxed) {
                HOVER.store(hit, Ordering::Relaxed);
                redraw(hwnd);
            }
            if DRAG_ARMED.load(Ordering::Relaxed) {
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                let (dx, dy) = DRAG
                    .lock()
                    .map(|d| {
                        d.as_ref()
                            .map(|s| (pt.x - s.cursor.x, pt.y - s.cursor.y))
                            .unwrap_or((0, 0))
                    })
                    .unwrap_or((0, 0));
                if !DRAGGING.load(Ordering::Relaxed) && (dx.abs() > 3 || dy.abs() > 3) {
                    DRAGGING.store(true, Ordering::Relaxed);
                    HOVER.store(-1, Ordering::Relaxed);
                }
                if DRAGGING.load(Ordering::Relaxed) {
                    // 尺寸必须**原样传回**：`MoveWindow` 的 nWidth/nHeight 是新尺寸，
                    // 传 0 会把窗口缩成 0x0（浮条"拖一下就不见了"）。
                    let (w, h) = window_size();
                    if let Some(d) = DRAG.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                        MoveWindow(hwnd, d.window.x + dx, d.window.y + dy, w, h, 0);
                    }
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let mut cursor: POINT = std::mem::zeroed();
            let mut wr: RECT = std::mem::zeroed();
            GetCursorPos(&mut cursor);
            GetWindowRect(hwnd, &mut wr);
            *DRAG.lock().unwrap_or_else(|p| p.into_inner()) = Some(DragState {
                cursor,
                window: POINT {
                    x: wr.left,
                    y: wr.top,
                },
                pressed: hit_test(x, y),
            });
            DRAG_ARMED.store(true, Ordering::Relaxed);
            DRAGGING.store(false, Ordering::Relaxed);
            SetCapture(hwnd);
            0
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            DRAG_ARMED.store(false, Ordering::Relaxed);
            let dragging = DRAGGING.load(Ordering::Relaxed);
            DRAGGING.store(false, Ordering::Relaxed);
            let pressed = DRAG
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .map(|d| d.pressed)
                .unwrap_or(-1);
            *DRAG.lock().unwrap_or_else(|p| p.into_inner()) = None;
            if dragging {
                let mut wr: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut wr);
                crate::state::write_pos(wr.left, wr.top);
            } else {
                match pressed {
                    // 品牌标：等同点 ☰，把菜单弹出来（投递到队列，不在这里直接弹）
                    0 | 3 => {
                        PostMessageW(hwnd, WM_OPEN_MENU, 0, 0);
                    }
                    1 => toggle_mode(hwnd),
                    2 => toggle_punct(hwnd),
                    _ => {}
                }
            }
            0
        }
        WM_OPEN_MENU => {
            // 菜单自己跑消息循环（会重入 wndproc），用 MENU_OPEN 挡掉重复弹出
            if !MENU_OPEN.swap(true, Ordering::SeqCst) {
                let mut wr: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut wr);
                let (chinese, punct) = crate::state::snapshot();
                let cmd = crate::menu::popup(hwnd, wr.left, wr.top, chinese, punct);
                MENU_OPEN.store(false, Ordering::SeqCst);
                if cmd != 0 {
                    crate::log_line(&format!("bar: menu choice {cmd}"));
                }
                handle_menu(hwnd, cmd);
            }
            0
        }
        WM_TIMER => {
            // 看门狗：本进程不是前台就把浮条收走（`Deactivate` 在 Alt+Tab 切程序时不会来），
            // 回到前台再放出来 —— 但只复活"被自动收起的"，用户手动关掉输入法不该被它弹回来。
            if wparam == TIMER_WATCH && WANTED.load(Ordering::SeqCst) {
                let self_front = foreground_is_self();
                let visible = IsWindowVisible(hwnd) != 0;
                if !self_front {
                    if visible {
                        ShowWindow(hwnd, SW_HIDE);
                    }
                    AUTO_HIDDEN.store(true, Ordering::SeqCst);
                } else if !visible {
                    // 切回前台要复活；同一个程序里已经有别的浮条在显示时就不用抢了
                    // （几条 TSF 线程各有一条，屏幕上只该有一条）。
                    let revive = AUTO_HIDDEN.load(Ordering::SeqCst) || !any_other_bar_visible(hwnd);
                    if revive {
                        redraw(hwnd);
                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        AUTO_HIDDEN.store(false, Ordering::SeqCst);
                    }
                }
                // 状态是全局的（注册表）：别的程序里点了中/英或标点，这一条也要跟着变，
                // 否则同一时刻不同窗口里的浮条会显示不同的状态。
                let (chinese, punct) = crate::state::snapshot();
                if !PAINTED_VALID.load(Ordering::Relaxed)
                    || chinese != PAINTED_CHINESE.load(Ordering::Relaxed)
                    || punct != PAINTED_PUNCT.load(Ordering::Relaxed)
                {
                    redraw(hwnd);
                }
            }
            0
        }
        // 注意：不要自己写 `WM_CLOSE => ...` 的分支 —— 之前漏了 import，Rust 把
        // 它当成了**新的绑定变量**（irrefutable pattern），于是每条消息都被当成
        // 关闭请求、窗口在 WM_NCCREATE 阶段就被 DestroyWindow 掉。DefWindowProcW
        // 对 WM_CLOSE 的默认处理就是 DestroyWindow，够了。
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 四格从左到右不重叠() {
        let rects = item_rects();
        for i in 1..4 {
            assert!(
                rects[i].left >= rects[i - 1].right,
                "第 {i} 格左边要 >= 上一格右边"
            );
        }
    }

    #[test]
    fn 窗口尺寸等于最后一格右边加内边距() {
        let (w, h) = window_size();
        let last = item_rects()[3];
        let pad = scale(PAD);
        assert_eq!(w, last.right + pad);
        assert_eq!(h, last.bottom + pad);
    }

    #[test]
    fn 命中测试覆盖每一格() {
        let rects = item_rects();
        for (i, r) in rects.iter().enumerate() {
            let cx = r.left + 1;
            let cy = r.top + 1;
            assert_eq!(hit_test(cx, cy), i as i32, "第 {i} 格左上角内侧");
        }
        assert_eq!(hit_test(-5, -5), -1, "负坐标不该命中");
        let last = rects[3];
        assert_eq!(
            hit_test(last.right + 5, last.top + 1),
            -1,
            "越过最后一格不该命中"
        );
    }

    #[test]
    fn 浮条能创建显示并绘制() {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;

        show();
        let raw = HWND_CELL.with(|c| c.get());
        assert_ne!(raw, 0, "show() 之后应该有窗口句柄");
        let hwnd = raw as HWND;

        // 0..=3 每个格子的 hover 状态都画一遍：悬停会走「给该格画高亮底」的分支，
        // 这条分支在宿主里 AV 崩过，必须覆盖。
        //
        // 分层窗口不走 WM_PAINT，`redraw` 是**同步**画完并 `UpdateLayeredWindow` 的；
        // 以前靠 `InvalidateRect + UpdateWindow` 触发绘制，漏过一次"根本没画"的空转。
        for h in -1..=3 {
            HOVER.store(h, std::sync::atomic::Ordering::Relaxed);
            assert!(
                redraw(hwnd),
                "hover={h} 时 UpdateLayeredWindow 失败（窗口会是空的）"
            );
        }
        HOVER.store(-1, std::sync::atomic::Ordering::Relaxed);

        assert_ne!(
            unsafe { IsWindowVisible(hwnd) },
            0,
            "show() 之后窗口应该可见"
        );
        hide();
        assert_eq!(
            unsafe { IsWindowVisible(hwnd) },
            0,
            "hide() 之后窗口应该不可见"
        );
    }

    /// 把预乘 alpha 的像素叠到一块纯色"桌面"上，写成 32 位 BMP（自上而下）。
    fn write_preview(name: &str, px: &[u32], w: i32, h: i32, desk: (u32, u32, u32)) {
        let size = (w as usize) * (h as usize) * 4;
        let mut out: Vec<u8> = Vec::with_capacity(54 + size);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&((54 + size) as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&w.to_le_bytes());
        out.extend_from_slice(&(-h).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for &p in px {
            let a = p >> 24;
            // 预乘过的：结果 = 前景 + 桌面 × (1 - a)
            let over = |c: u32, d: u32| (c + d * (255 - a) / 255).min(255) as u8;
            out.push(over(p & 0xFF, desk.2));
            out.push(over((p >> 8) & 0xFF, desk.1));
            out.push(over((p >> 16) & 0xFF, desk.0));
            out.push(255);
        }
        // 固定写到 crate 的 target 下：`temp_dir()` 在特权/受限环境里未必可写。
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(name);
        std::fs::write(&path, &out).expect("预览图写入失败");
    }

    /// 用和窗口完全相同的 `paint_bar` 渲一帧，叠在白色和深灰两种"桌面"上，
    /// 存成 `tsf/target/bar-preview.bmp` / `bar-preview-dark.bmp`，
    /// 另有一张悬停在标点格上的 `bar-preview-hover.bmp`。
    /// 改配色/布局时先看这两张图（圆角抗锯齿在深色桌面上最容易看出来）。
    #[test]
    fn 渲染预览图() {
        let (w, h) = window_size();
        unsafe {
            let mut c = Canvas::new(w, h).expect("画布创建失败");
            paint_bar(&mut c, -1);
            let px = c.pixels().to_vec();
            write_preview("bar-preview.bmp", &px, w, h, (0xFF, 0xFF, 0xFF));
            write_preview("bar-preview-dark.bmp", &px, w, h, (0x3A, 0x3A, 0x3A));

            // 悬停在标点格上（看垫底的圆角是不是也平滑）
            let mut c = Canvas::new(w, h).expect("画布创建失败");
            paint_bar(&mut c, 2);
            let px = c.pixels().to_vec();
            write_preview("bar-preview-hover.bmp", &px, w, h, (0xFF, 0xFF, 0xFF));
        }
    }

    /// 圆角是不是真的抗锯齿了：角外全透明、中间不透明、角上弧线经过的像素是**半透明**的
    /// （GDI 区域裁出来的角只有 0/255 两种值）。期望值按圆角几何手算：
    /// 100×36、r=8 的框，像素 (2,2) 的中心到圆角圆心 (8,8) 的距离 5.5√2 ≈ 7.78，
    /// 比半径小 0.22 → 覆盖率 0.72。
    #[test]
    fn 外形四角是抗锯齿的() {
        let (w, h) = (100, 36);
        let mut px = vec![0x00FF_FFFFu32; (w * h) as usize];
        apply_frame(&mut px, w, h, 8.0, 1.0, 0x0000_0000);
        let a = |x: i32, y: i32| px[(y * w + x) as usize] >> 24;
        assert_eq!(a(0, 0), 0, "角外透明");
        assert_eq!(a(50, 18), 255, "中间不透明");
        let edge = a(2, 2);
        assert!(
            (170..=200).contains(&edge),
            "角上过渡像素应约 0.72×255，实际 {edge}"
        );
        assert_eq!(a(w - 3, 2), edge, "右上角对称");
        assert_eq!(a(2, h - 3), edge, "左下角对称");
        assert_eq!(a(w - 3, h - 3), edge, "右下角对称");
        // 描边 1px：上边正中最外一行是描边色（黑），往里一行是内容（白）
        assert_eq!(px[50] & 0x00FF_FFFF, 0, "最外一行是描边");
        assert_eq!(
            px[(w + 50) as usize] & 0x00FF_FFFF,
            0x00FF_FFFF,
            "第二行是内容"
        );
    }

    /// 抗锯齿靠的是 `glyph_sdf` 的正负号：< 0 在墨里、> 0 在外面、跨 0 的那一个像素
    /// 才按距离给半透明。符号算反了整格会变成一块实心方块（或者干脆什么都不画），
    /// 而预览图上一眼很难分出来，所以这里把内外点钉死。
    #[test]
    fn 句号圆环的距离场内外分明() {
        // 圆心 (0,0)、环中线半径 4、半线宽 1 → 墨在半径 3~5 之间
        let ring = Glyph::Ring {
            cx: 0.0,
            cy: 0.0,
            r: 4.0,
            hw: 1.0,
        };
        assert!(glyph_sdf(ring, 0.0, 0.0) > 0.0, "圆心是环的洞，该在外面");
        assert!(glyph_sdf(ring, 4.0, 0.0) < 0.0, "环中线上，该在墨里");
        assert!(glyph_sdf(ring, 0.0, -4.0) < 0.0, "上下左右要对匀");
        assert!(glyph_sdf(ring, 2.0, 0.0) > 0.0, "圈里的洞");
        assert!(glyph_sdf(ring, 6.0, 0.0) > 0.0, "圈外的空");
        assert!(glyph_sdf(ring, 3.0, 0.0).abs() < 0.001, "内边缘处距离为 0");
    }

    #[test]
    fn 逗号是有头有尾的不对称形状() {
        let comma = Glyph::Comma {
            cx: 0.0,
            cy: 0.0,
            k: 1.0,
        };
        assert!(glyph_sdf(comma, 0.0, 0.0) < 0.0, "圆头中心在墨里");
        assert!(glyph_sdf(comma, 0.0, 50.0) > 0.0, "远处在外面");
        // 尾巴往**左下**甩：下方几个像素处还在墨里，正上方对称位置就该空了。
        assert!(glyph_sdf(comma, 0.0, 3.0) < 0.0, "圆头下面接着尾巴");
        assert!(glyph_sdf(comma, -1.4, 5.4) < 0.0, "尾尖的中心仍是墨");
        assert!(glyph_sdf(comma, -0.3, 4.2) < 0.0, "尾巴中段在墨里");
        assert!(
            glyph_sdf(comma, 0.0, -3.0) > 0.0,
            "上面不该有东西（逗号不是两端对称的）"
        );
        assert!(
            glyph_sdf(comma, 0.0, 7.5) > 0.0,
            "尾巴到 5.4+0.4 就收尖了，再往下必须是空的"
        );
    }

    #[test]
    fn 品牌标只挑_ico_里真有的帧() {
        assert_eq!(pick_frame(24), 24);
        assert_eq!(pick_frame(30), 24, "30 没有现成帧，只能退到 24");
        assert_eq!(pick_frame(23), 20, "不能超过想要的大小");
        assert_eq!(pick_frame(200), 64, "最大档就是 64");
        assert_eq!(pick_frame(8), 16, "想要得比最小时用最小帧");
        let side = badge_side();
        assert!(
            BADGE_FRAMES.contains(&side),
            "实际取值 {side} 必须是 ico 里有的帧"
        );
    }
}
