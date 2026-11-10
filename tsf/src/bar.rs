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
//! 回到前台再自动放出来（用户手动关掉输入法的那条线程不会被它复活）。
//!
//! ## 同一进程最多一条（2026-11-01 修"偶尔同时冒出两条"）
//!
//! 窗口按线程，"这条线程想不想显示、是不是被看门狗收起的"也必须按线程
//! （`INSTANCE_VISIBILITY`）。以前这两个标记是进程级的：任一线程把"被自动收起"置真，
//! 别的线程的看门狗回到前台都会跳过"已经有一条在显示"的检查，各放一条出来。
//! "先查有没有可见的、再显示"本身也是竞态，所以现在由进程级 `VISIBLE_OWNER` 的一次
//! 比较交换授权：拿到的那一个才能收起别的、绘制、显示、发布；没拿到的不等待、保持隐藏。
//! 收起时先隐藏、确认不可见了才释放所有权。纯逻辑在 `drive_visibility`，有内存模拟宿主的单测。

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, AlphaBlend, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush,
    DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW,
    EndPaint, FillRect, GdiFlush, HBITMAP, HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, GetCurrentProcessId, PROCESS_INFORMATION, STARTUPINFOW,
};
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
/// 状态条变了（AI 处理中/失败），让窗口过程在编辑会话之外重画一次。
const WM_STATUS: u32 = WM_APP + 21;
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

/// 临时盖在浮条上的一行字（AI 的"翻译中… / 失败"）。
///
/// 为什么不另开一个提示窗口：浮条本来就在屏幕上、而且 AI 就是从它上面的菜单点出来的，
/// 用户点完菜单眼睛还在附近；再加一个窗口就多一套生命周期和 DPI 适配。
static STATUS: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// 看门狗的定时器 ID（`WM_TIMER` 的 wParam）。
const TIMER_WATCH: usize = 1;
/// 看门狗周期：600ms。再快没必要，再慢切走程序后能明显看到浮条赖着不走。
const WATCH_MS: u32 = 600;

/// AI 面板开着时浮条外圈那圈转动彩光的动画定时器。
const TIMER_GLOW: usize = 2;
/// 25 帧/秒：转得顺，又不至于让宿主进程一直忙着画。只在面板开着、浮条可见时跑。
const GLOW_MS: u32 = 40;
/// 转一圈多久。
const GLOW_PERIOD_MS: f32 = 2400.0;
/// 浮条四周给彩光留的边（96 DPI 设计值）。**一直留着**：不亮的时候这圈像素全透明，
/// 分层窗口对 alpha 为 0 的像素不做命中测试，鼠标会直接穿过去，跟没有一样。
/// 常留是为了亮/灭时窗口大小不变——否则位置、拖动、命中测试都要跟着来回换算。
const GLOW_PAD: i32 = 7;
/// 彩光开没开（面板开着 = 开）。进程级：面板一个进程只有一个。
static GLOW_ON: AtomicBool = AtomicBool::new(false);

// 本线程的浮条窗口。**必须是 thread_local**：TSF 按线程激活，一个进程里可能有好几条
// 线程各建一个窗口；放进程级全局只记得最后一个，其余窗口就永远没人关（叠出好几条）。
//
// 实例状态（这条线程想不想显示、是不是被看门狗收起的）也必须按线程：以前放进程级，
// 任一线程把"被自动收起"置真，别的线程的看门狗回到前台都会绕过去重，各放一条出来。
thread_local! {
    static HWND_CELL: Cell<isize> = const { Cell::new(0) };
    static INSTANCE_VISIBILITY: Cell<InstanceVisibility> = const {
        Cell::new(InstanceVisibility {
            wanted: false,
            auto_hidden: false,
        })
    };
}
static REGISTERED: AtomicBool = AtomicBool::new(false);
static HOVER: AtomicI32 = AtomicI32::new(-1);
static DRAG_ARMED: AtomicBool = AtomicBool::new(false);
static DRAGGING: AtomicBool = AtomicBool::new(false);
/// 进程级可见所有权：`OWNER_NONE` = 没有，`OWNER_ARBITRATING` = 有一个显示请求正在仲裁，
/// 其他值 = 当前那条可见浮条的句柄。所有把隐藏浮条变可见的路径都要先比较交换它，
/// 拿到的那一个才能显示 —— 这是"同一进程最多一条"的唯一授权点。
static VISIBLE_OWNER: AtomicIsize = AtomicIsize::new(OWNER_NONE);
static FONT: AtomicIsize = AtomicIsize::new(0);

// 上一次画出来的状态。看门狗拿它跟注册表比，不一样就重画 ——
// 状态是全局的，别的程序里点了中/英或标点，这条浮条也得跟着变。
static PAINTED_VALID: AtomicBool = AtomicBool::new(false);
static PAINTED_CHINESE: AtomicBool = AtomicBool::new(true);
/// 上一次绘制时的 Caps Lock 状态（大写时图标显示 A）
static PAINTED_CAPS: AtomicBool = AtomicBool::new(false);
static PAINTED_PUNCT: AtomicBool = AtomicBool::new(true);
/// 上一次用的是不是暗色主题。`config::snapshot().dark` 变了也要重画。
static PAINTED_DARK: AtomicBool = AtomicBool::new(false);

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
/// AI 三格（翻译/改写/续写）：亮暗切换挪进 ☰ 菜单，这里换成 AI 的快捷入口。
const W_AI: i32 = 38;
const W_MENU: i32 = 30;

// COLORREF 是 0x00BBGGRR（低字节是红）
/// 浮条的一套配色。浅色/暗色只差这张表，形状与排版完全一样。
struct BarPalette {
    /// 整条底色
    bg: u32,
    /// 描边 + 分隔竖线
    border: u32,
    /// 悬停垫底
    hover: u32,
    /// 常态文字/图形
    text: u32,
    /// 关闭/未激活
    dim: u32,
    /// 激活色（中/英 处于"中"、标点处于全角时）
    accent: u32,
}

/// 浅色：#FFFFFF 底、#E2E2E8 描边、#F2F2F6 悬停、#3A3A3E 字、#A2A2A8 灰、品牌深蓝 #062657 激活。
const PALETTE_LIGHT: BarPalette = BarPalette {
    bg: 0x00_FF_FF_FF,
    border: 0x00_E8_E2_E2,
    hover: 0x00_F6_F2_F2,
    text: 0x00_3E_3A_3A,
    dim: 0x00_A8_A2_A2,
    accent: 0x00_57_26_06,
};

/// 暗色：底色沿用候选窗的 #1E1E22 系，字用暖橙（用户指定 #FF9E3D）。
///
/// 激活色原来是近白，用户要求换成橙——暗底上橙色比白色柔和，也和候选窗
/// 暗色主题的选中色是同一个色号。关状态用暗一档的橙，仍能一眼分出来。
const PALETTE_DARK: BarPalette = BarPalette {
    bg: 0x00_26_22_22,
    border: 0x00_46_41_41,
    hover: 0x00_38_33_33,
    text: 0x00_3D_9E_FF,   // #ff9e3d 暖橙（COLORREF 是 0x00BBGGRR）
    dim: 0x00_3D_71_A9,    // #a9713d 暗一档的橙 = 关状态
    accent: 0x00_3D_9E_FF, // 激活同样用 #ff9e3d
};

fn bar_palette(dark: bool) -> &'static BarPalette {
    if dark { &PALETTE_DARK } else { &PALETTE_LIGHT }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(v: i32) -> i32 {
    let dpi = unsafe { GetDpiForSystem() };
    let dpi = if dpi == 0 { 96 } else { dpi as i32 };
    (v * dpi + 48) / 96
}

/// 五个可点的地方：品牌标、中文/英文、中文标点、**AI**、菜单。
///
/// 1.1.42：三个 AI 格并成一个「AI」按钮，点开是浮动面板（翻译/改写/续写在面板里）；
/// 亮/暗切换在 ☰ 菜单里。
///
/// 品牌标和第一格状态之间多留一道竖线（`SEP_W` 宽，两边各 `SEP_GAP`），
/// 和搜狗浮条一样给"品牌 | 状态"一个视觉分隔。竖线本身不是可点区域。
fn item_rects() -> [RECT; 5] {
    let pad = scale(PAD);
    let gap = scale(GAP);
    let h = scale(ITEM_H);
    let sep_total = scale(SEP_GAP) * 2 + scale(SEP_W);
    let widths = [
        scale(W_BRAND),
        scale(W_MODE),
        scale(W_PUNCT),
        scale(W_AI),
        scale(W_MENU),
    ];
    let mut rects = [RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }; 5];
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

/// 浮条本体的大小（不含外圈彩光的边）。
fn window_size() -> (i32, i32) {
    let last = item_rects()[4];
    let pad = scale(PAD);
    (last.right + pad, last.bottom + pad)
}

/// 外圈留给彩光的边（像素）。
fn glow_pad() -> i32 {
    scale(GLOW_PAD)
}

/// 真正的窗口大小 = 浮条本体 + 四周彩光边。
fn outer_size() -> (i32, i32) {
    let (w, h) = window_size();
    let g = glow_pad();
    (w + 2 * g, h + 2 * g)
}

/// 窗口客户区坐标 → 浮条本体坐标（`item_rects` / `hit_test` 用的那套）。
fn to_bar_coords(x: i32, y: i32) -> (i32, i32) {
    let g = glow_pad();
    (x - g, y - g)
}

/// 打开/关掉外圈彩光（AI 面板开/关时调，在面板所在的线程上）。
pub fn set_glow(on: bool) {
    GLOW_ON.store(on, Ordering::SeqCst);
    let hwnd = our_hwnd();
    if (hwnd as isize) == 0 {
        return;
    }
    unsafe {
        if on {
            SetTimer(hwnd, TIMER_GLOW, GLOW_MS, None);
        } else {
            KillTimer(hwnd, TIMER_GLOW);
        }
        if IsWindowVisible(hwnd) != 0 {
            redraw(hwnd);
        }
    }
}

/// 现在转到第几圈的哪儿（0..1）。按真实时间算，丢帧也不会忽快忽慢。
fn glow_phase() -> f32 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t0 = START.get_or_init(Instant::now);
    let ms = t0.elapsed().as_millis() as f32;
    (ms % GLOW_PERIOD_MS) / GLOW_PERIOD_MS
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

/// 本线程的浮条窗口句柄（0 = 还没建）。
///
/// AI 用它当"结果好了叫我"的收件地址：引擎会把 `WM_APP + 30` 投到这个窗口
/// （见 `ai.rs` 的 `WM_AI_DONE`）。必须是本线程那条，别的线程的窗口不会把消息
/// 派发到我们的窗口过程。
pub fn hwnd() -> isize {
    our_hwnd() as isize
}

fn set_our_hwnd(hwnd: HWND) {
    HWND_CELL.with(|c| c.set(hwnd as isize));
}

/// 前台窗口是不是我们自己这个进程的窗口（AI 面板也用它决定收起还是露出来）。
pub(crate) fn foreground_is_self() -> bool {
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

/// 这个窗口的类名是不是我们的浮条。`None` = 类名取不到（不确定）。
fn class_is_bar(hwnd: HWND) -> Option<bool> {
    let mut buf = [0u16; 48];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return None;
    }
    let name = buf.get(..n as usize)?;
    Some(name.iter().copied().eq(CLASS_NAME.encode_utf16()))
}

// ── 进程内窗口枚举（保底清理与核验）──────────────────────────────────────────
//
// 枚举结果放在**调用栈上**的 `EnumContext` 里，经 `LPARAM` 传给回调。以前用一个进程级
// `ANY_BAR` 存结果，两条线程同时枚举会互相覆盖。枚举只负责"看清楚、收干净"，
// 不负责选谁显示 —— 选主只靠 `VISIBLE_OWNER` 的那一次比较交换。

/// 这次枚举是"找一条别的可见浮条"还是"把别的可见浮条都收起来"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnumMode {
    Find,
    Hide,
}

/// `EnumContext::observe` 告诉回调接下来做什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnumStep {
    Continue,
    /// 清理模式：把这一条收起来，再用 `hidden` 回报它是否真的收起了。
    HideThis,
    /// 查询模式：已经找到，停止枚举。
    Stop,
}

/// 一次枚举的上下文。字段都是定长值，回调里不分配、不加锁、不 panic。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EnumContext {
    /// 当前宿主进程的 PID：只看自己进程的窗口，别的宿主的浮条一概不碰。
    pid: u32,
    /// 请求者自己的句柄，跳过。
    me: isize,
    mode: EnumMode,
    /// 查询：第一条别的可见浮条；清理：第一条收不起来的。0 = 没有。
    found: isize,
    /// 遇到了本进程里类名取不到的窗口。
    uncertain: bool,
    /// 回调主动返回了 0（找到了），用来和"`EnumWindows` 本身失败"区分开。
    stopped: bool,
}

impl EnumContext {
    fn new(pid: u32, me: isize, mode: EnumMode) -> Self {
        EnumContext {
            pid,
            me,
            mode,
            found: 0,
            uncertain: false,
            stopped: false,
        }
    }

    /// 回调把从 Win32 取到的一个窗口的数据交给它（纯方法，可单测）。
    /// `is_bar` 是类名判断，`None` 表示取不到。
    fn observe(&mut self, hwnd: isize, pid: u32, is_bar: Option<bool>, visible: bool) -> EnumStep {
        if hwnd == self.me || pid != self.pid {
            return EnumStep::Continue;
        }
        let Some(is_bar) = is_bar else {
            self.uncertain = true;
            return EnumStep::Continue;
        };
        if !is_bar || !visible {
            return EnumStep::Continue;
        }
        match self.mode {
            EnumMode::Find => {
                self.found = hwnd;
                self.stopped = true;
                EnumStep::Stop
            }
            EnumMode::Hide => EnumStep::HideThis,
        }
    }

    /// 清理模式：收起一条之后回报它是否仍可见。
    fn hidden(&mut self, hwnd: isize, still_visible: bool) {
        if still_visible && self.found == 0 {
            self.found = hwnd;
        }
    }

    /// 查询结果。`enum_ok` 是 `EnumWindows` 的返回值是否非零。
    fn query_result(&self, enum_ok: bool) -> VisibilityQuery {
        if self.found != 0 {
            VisibilityQuery::Visible(self.found)
        } else if self.uncertain || (!enum_ok && !self.stopped) {
            VisibilityQuery::Unknown
        } else {
            VisibilityQuery::NoneVisible
        }
    }

    /// 清理结果。只有确定全部收起了才是 `AllHidden`。
    fn cleanup_result(&self, enum_ok: bool) -> CleanupResult {
        if self.found != 0 {
            CleanupResult::StillVisible(self.found)
        } else if self.uncertain || !enum_ok {
            CleanupResult::Unknown
        } else {
            CleanupResult::AllHidden
        }
    }
}

unsafe extern "system" fn on_enum_bar(hwnd: HWND, lparam: LPARAM) -> i32 {
    let ctx = lparam as *mut EnumContext;
    if ctx.is_null() {
        return 0;
    }
    // SAFETY: `lparam` 是 `enum_bars` 栈上 `EnumContext` 的指针，`EnumWindows` 同步回调，
    // 它在整个枚举期间有效，而且只有这一个回调在用它。
    let ctx = unsafe { &mut *ctx };
    let h = hwnd as isize;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    // 只给本进程、不是自己的窗口取类名（别的进程的窗口用不着）。
    let is_bar = if pid == ctx.pid && h != ctx.me {
        class_is_bar(hwnd)
    } else {
        Some(false)
    };
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    match ctx.observe(h, pid, is_bar, visible) {
        EnumStep::Continue => 1,
        EnumStep::Stop => 0,
        EnumStep::HideThis => {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
            let still = unsafe { IsWindowVisible(hwnd) } != 0;
            ctx.hidden(h, still);
            1
        }
    }
}

fn enum_bars(me: isize, mode: EnumMode) -> (EnumContext, bool) {
    let mut ctx = EnumContext::new(unsafe { GetCurrentProcessId() }, me, mode);
    let ok = unsafe { EnumWindows(Some(on_enum_bar), &mut ctx as *mut EnumContext as LPARAM) } != 0;
    (ctx, ok)
}

/// 本进程里除 `me` 以外还有没有可见浮条。
fn find_other_visible_bar(me: isize) -> VisibilityQuery {
    let (ctx, ok) = enum_bars(me, EnumMode::Find);
    ctx.query_result(ok)
}

/// 把本进程里除 `me` 以外的可见浮条都收起来（顺带清掉没被原子记录的遗留重复窗口）。
fn hide_other_bars(me: isize) -> CleanupResult {
    let (ctx, ok) = enum_bars(me, EnumMode::Hide);
    ctx.cleanup_result(ok)
}

// ---------------------------------------------------------------------------
// 可见性仲裁（纯逻辑接缝）
//
// 两层状态：每条 TSF 线程只管自己的 `InstanceVisibility`（要不要显示、是不是被看门狗
// 收起的），而"现在谁可以显示"由一个进程级原子所有权决定 —— 所有把隐藏窗口变可见的
// 路径都要先把它比较交换成 `OWNER_ARBITRATING`，拿到的那一个才能隐藏旧窗口、绘制、显示、
// 发布结果。窗口句柄在这一层一律用 `isize`，到 Win32 边界再转换，便于用内存窗口表单测。
//
// 生产路径（`show` / `hide` / `WM_TIMER`）和测试都只经过 `drive_visibility` 这一个入口。
// ---------------------------------------------------------------------------

/// 原子所有权：没有已发布的可见浮条。
const OWNER_NONE: isize = 0;
/// 原子所有权：有一个显示请求正在仲裁。`CreateWindowExW` 不会返回 -1，所以它只当哨兵用，
/// 不会被当成可见窗口。
const OWNER_ARBITRATING: isize = -1;

/// 一条 TSF 线程的浮条实例状态（线程局部，不跨线程共享）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InstanceVisibility {
    /// 这条线程的输入法是不是开着（`Activate` 置真、`Deactivate` 置假）。
    wanted: bool,
    /// 是不是被看门狗因为宿主离开前台而收起的。只说明"为什么隐藏"，不授权显示。
    auto_hidden: bool,
}

/// 喂给 `reduce_visibility` 的事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisibilityEvent {
    /// 本线程收到输入法激活；`foreground` 是宿主此刻是否在前台。
    Activate { foreground: bool },
    /// 本线程收到输入法停用。
    Deactivate,
    /// 看门狗定时器到点；`self_visible` 是本线程的浮条此刻是否可见。
    Watchdog {
        foreground: bool,
        self_visible: bool,
    },
    /// 一次显示请求结束；`shown` 是本线程的浮条是否已确认可见。
    DisplayFinished { shown: bool },
}

/// `reduce_visibility` 要求调用方执行的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisibilityAction {
    None,
    /// 激活引起的显示：取得仲裁权后可以把别的可见浮条收起来、换成自己。
    RequestActivationDisplay,
    /// 看门狗引起的显示：只补空缺，不抢已经可见的浮条。
    RequestWatchdogDisplay,
    /// 隐藏自己，确认不可见后释放所有权。
    HideAndRelease,
}

/// 显示请求从哪来，决定能不能抢占已有的可见浮条。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DisplayCause {
    Activation,
    Watchdog,
}

/// "本进程里除了我还有没有可见浮条"的查询结果。`Unknown` 一律当作"不能显示"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisibilityQuery {
    NoneVisible,
    Visible(isize),
    Unknown,
}

/// "把除我以外的浮条都收起来"的结果。只有 `AllHidden` 才允许继续显示自己。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupResult {
    AllHidden,
    /// 有一条收不起来、仍然可见（带它的句柄）。
    StillVisible(isize),
    Unknown,
}

/// 一次 `drive_visibility` / 显示事务的结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DisplayOutcome {
    /// 本线程的浮条已确认可见，并已发布为所有者。
    Shown,
    /// 本线程的浮条保持（或变为）隐藏：被否决、失败，或本来就不需要显示。
    KeptHidden,
    /// 另一个请求正在仲裁（或比较交换失败），本次不等待、直接返回。
    Busy,
    /// 本线程没有浮条窗口。
    NoWindow,
}

/// `drive_visibility` 的触发来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Trigger {
    Activate,
    Deactivate,
    Tick,
}

/// 仲裁需要的窗口副作用，只这几样。生产实现直接调 Win32，测试实现是内存窗口表。
/// 方法都取 `&self`：测试要在 `redraw` 回调里对同一张窗口表发起嵌套请求，模拟两条线程重叠。
trait DisplayIo {
    /// 宿主进程此刻是不是前台。
    fn is_foreground(&self) -> bool;
    /// 这个窗口此刻是否可见。
    fn is_visible(&self, h: isize) -> bool;
    /// 把本进程里除 `me` 以外的可见浮条都收起来。
    fn hide_others(&self, me: isize) -> CleanupResult;
    /// 本进程里除 `me` 以外还有没有可见浮条。
    fn find_other_visible(&self, me: isize) -> VisibilityQuery;
    /// 给 `me` 画好内容（分层窗口要先有内容再显示）。返回是否成功。
    fn redraw(&self, me: isize) -> bool;
    /// 不抢焦点地显示 `me`。成功与否由调用方再用 `is_visible` 确认。
    fn show(&self, me: isize);
    /// 隐藏 `me`。
    fn hide(&self, me: isize);
}

/// 实例状态归约：只看本线程的状态和事件，给出新状态和要执行的动作。纯函数。
fn reduce_visibility(
    state: InstanceVisibility,
    event: VisibilityEvent,
) -> (InstanceVisibility, VisibilityAction) {
    use VisibilityAction as A;
    use VisibilityEvent as E;
    let active = |auto_hidden| InstanceVisibility {
        wanted: true,
        auto_hidden,
    };
    match event {
        // 前台激活：想显示，交给仲裁（可以接管已有的可见浮条）。
        E::Activate { foreground: true } => (active(false), A::RequestActivationDisplay),
        // 后台激活：想显示但先收着，等回到前台由看门狗复活。
        E::Activate { foreground: false } => (active(true), A::HideAndRelease),
        E::Deactivate => (InstanceVisibility::default(), A::HideAndRelease),
        // 停用（或从未激活）的实例：看门狗什么都不做 —— 停用后不复活。
        E::Watchdog { .. } if !state.wanted => (state, A::None),
        E::Watchdog {
            foreground: false, ..
        } => (active(true), A::HideAndRelease),
        // 前台且自己隐藏：发一个不抢占的复活请求。能不能显示由仲裁决定，
        // `auto_hidden` 只说明为什么隐藏，不授权显示。
        E::Watchdog {
            foreground: true,
            self_visible: false,
        } => (state, A::RequestWatchdogDisplay),
        E::Watchdog {
            foreground: true,
            self_visible: true,
        } => (state, A::None),
        E::DisplayFinished { shown: true } => (
            InstanceVisibility {
                auto_hidden: false,
                ..state
            },
            A::None,
        ),
        // 失败保留状态，下一次看门狗/激活会重试。
        E::DisplayFinished { shown: false } => (state, A::None),
    }
}

/// 显示事务：一次比较交换取得仲裁哨兵，持有它走完"复核 → 清理/查询 → 绘制 → 显示 →
/// 复核 → 发布"，所有返回路径都把原子发布成 `OWNER_NONE` 或一个经确认可见的句柄。
fn run_display_transaction(
    owner: &AtomicIsize,
    cause: DisplayCause,
    me: isize,
    io: &dyn DisplayIo,
) -> DisplayOutcome {
    if me <= 0 {
        return DisplayOutcome::NoWindow;
    }
    let observed = owner.load(Ordering::SeqCst);
    if observed == OWNER_ARBITRATING {
        return DisplayOutcome::Busy;
    }
    // 看门狗只补空缺：原子指向另一条仍可见的浮条就直接否决。这个快速查询只能否决、
    // 不能授权（授权只来自下面那一次比较交换），所以不构成"先查后显示"的竞态。
    if cause == DisplayCause::Watchdog
        && observed != OWNER_NONE
        && observed != me
        && io.is_visible(observed)
    {
        return DisplayOutcome::KeptHidden;
    }
    // 唯一的授权点：只试一次，不等待、不自旋。
    if owner
        .compare_exchange(
            observed,
            OWNER_ARBITRATING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return DisplayOutcome::Busy;
    }

    // ── 以下握着仲裁哨兵：每条返回路径都经 `finish_arbitration` / `abort_display` 发布 ──
    if !io.is_foreground() {
        return abort_display(owner, me, observed, io);
    }
    match cause {
        // 激活：先把别的浮条全部收起（旧的先隐藏、新的后显示），收不干净就不显示自己。
        DisplayCause::Activation => match io.hide_others(me) {
            CleanupResult::AllHidden => {}
            CleanupResult::StillVisible(h) => return abort_display(owner, me, h, io),
            CleanupResult::Unknown => return abort_display(owner, me, observed, io),
        },
        // 看门狗：不抢占，发现别的可见浮条就让它继续当所有者；看不清也不显示。
        DisplayCause::Watchdog => match io.find_other_visible(me) {
            VisibilityQuery::NoneVisible => {}
            VisibilityQuery::Visible(h) => return abort_display(owner, me, h, io),
            VisibilityQuery::Unknown => return abort_display(owner, me, observed, io),
        },
    }
    if !io.redraw(me) {
        return abort_display(owner, me, observed, io);
    }
    io.show(me);
    // 不信 `ShowWindow` 的返回值（它表示的是"之前是否可见"），用可见性和前台再确认一次。
    if !io.is_visible(me) || !io.is_foreground() {
        return abort_display(owner, me, observed, io);
    }
    finish_arbitration(owner, me);
    DisplayOutcome::Shown
}

/// 握着仲裁哨兵之后的出口：把原子发布成 `OWNER_NONE` 或一个经确认可见的句柄。
fn finish_arbitration(owner: &AtomicIsize, publish: isize) {
    owner.store(publish, Ordering::SeqCst);
}

/// 显示没成功时的收尾：确保 `me` 隐藏；`keep` 若是另一条仍可见的浮条，就让它继续当所有者，
/// 否则发布空。万一 `me` 收不起来（仍可见），它就是那条可见的，发布它，免得别人再显示一条。
fn abort_display(
    owner: &AtomicIsize,
    me: isize,
    keep: isize,
    io: &dyn DisplayIo,
) -> DisplayOutcome {
    if io.is_visible(me) {
        io.hide(me);
    }
    let publish = if io.is_visible(me) {
        me
    } else if keep > 0 && keep != me && io.is_visible(keep) {
        keep
    } else {
        OWNER_NONE
    };
    finish_arbitration(owner, publish);
    DisplayOutcome::KeptHidden
}

/// 隐藏自己并释放所有权：先隐藏，复查确认不可见后才把原子从 `me` 换回 `OWNER_NONE`。
/// 返回是否已确认隐藏。
fn release_owner(owner: &AtomicIsize, me: isize, io: &dyn DisplayIo) -> bool {
    if me <= 0 {
        return true;
    }
    if io.is_visible(me) {
        io.hide(me);
    }
    // 收不起来就不释放：窗口还在屏幕上，别人不能再显示一条。
    if io.is_visible(me) {
        return false;
    }
    // 只在原子正指向自己时才换回空。原子是仲裁中（别人正在显示）或别的句柄时不动它。
    let _ = owner.compare_exchange(me, OWNER_NONE, Ordering::SeqCst, Ordering::SeqCst);
    true
}

/// 生产路径和测试共用的唯一编排入口：读前台/自身可见 → 组装 `VisibilityEvent` →
/// `reduce_visibility` → 执行动作 → 用 `DisplayFinished` 回写本实例状态。
fn drive_visibility(
    owner: &AtomicIsize,
    state: &mut InstanceVisibility,
    me: isize,
    trigger: Trigger,
    io: &dyn DisplayIo,
) -> DisplayOutcome {
    let foreground = io.is_foreground();
    let self_visible = me > 0 && io.is_visible(me);
    let event = match trigger {
        Trigger::Activate => VisibilityEvent::Activate { foreground },
        Trigger::Deactivate => VisibilityEvent::Deactivate,
        Trigger::Tick => VisibilityEvent::Watchdog {
            foreground,
            self_visible,
        },
    };
    let (next, action) = reduce_visibility(*state, event);
    *state = next;
    // 没窗口也先记下状态（例如"想显示"），窗口建好后下一次激活/看门狗还能重试。
    if me <= 0 {
        return DisplayOutcome::NoWindow;
    }
    let cause = match action {
        VisibilityAction::None => {
            return if self_visible {
                DisplayOutcome::Shown
            } else {
                DisplayOutcome::KeptHidden
            };
        }
        VisibilityAction::HideAndRelease => {
            release_owner(owner, me, io);
            return DisplayOutcome::KeptHidden;
        }
        VisibilityAction::RequestActivationDisplay => DisplayCause::Activation,
        VisibilityAction::RequestWatchdogDisplay => DisplayCause::Watchdog,
    };
    let out = run_display_transaction(owner, cause, me, io);
    let finished = VisibilityEvent::DisplayFinished {
        shown: out == DisplayOutcome::Shown,
    };
    *state = reduce_visibility(*state, finished).0;
    out
}

/// 仲裁用的真实窗口副作用。`foreground` 可替换只是为了让冒烟测试在非前台的
/// `cargo test` 进程里也能显示；生产路径一律是 `foreground_is_self`。
struct Win32Io {
    foreground: fn() -> bool,
}

impl DisplayIo for Win32Io {
    fn is_foreground(&self) -> bool {
        (self.foreground)()
    }

    fn is_visible(&self, h: isize) -> bool {
        h > 0 && unsafe { IsWindowVisible(h as HWND) } != 0
    }

    fn hide_others(&self, me: isize) -> CleanupResult {
        hide_other_bars(me)
    }

    fn find_other_visible(&self, me: isize) -> VisibilityQuery {
        find_other_visible_bar(me)
    }

    fn redraw(&self, me: isize) -> bool {
        // 直接调 `present`，不经会写日志的 `redraw()`：仲裁期间不碰磁盘。
        unsafe { present(me as HWND) }
    }

    fn show(&self, me: isize) {
        unsafe { ShowWindow(me as HWND, SW_SHOWNOACTIVATE) };
    }

    fn hide(&self, me: isize) {
        unsafe { ShowWindow(me as HWND, SW_HIDE) };
    }
}

/// 用本线程的实例状态跑一次 `drive_visibility`。`hwnd` 必须是本线程建的那条浮条（或 0）。
fn drive_this_thread(hwnd: HWND, trigger: Trigger, foreground: fn() -> bool) -> DisplayOutcome {
    let io = Win32Io { foreground };
    INSTANCE_VISIBILITY.with(|cell| {
        let mut state = cell.get();
        let out = drive_visibility(&VISIBLE_OWNER, &mut state, hwnd as isize, trigger, &io);
        cell.set(state);
        out
    })
}

/// 浮条出现（`Activate` 时由 lib.rs 调，在宿主 UI 线程上）。
pub fn show() {
    show_with_foreground(foreground_is_self);
}

fn show_with_foreground(foreground: fn() -> bool) {
    // 先记下"想显示"：窗口建不出来时，下一次激活还会重新评估。
    INSTANCE_VISIBILITY.with(|cell| {
        let mut state = cell.get();
        state.wanted = true;
        cell.set(state);
    });
    ensure_window();
    let hwnd = our_hwnd();
    if (hwnd as isize) == 0 {
        return;
    }
    keep_on_screen(hwnd);
    // 前台激活：拿到仲裁权就收起别的浮条、显示自己；后台激活：先收着，等看门狗。
    let out = drive_this_thread(hwnd, Trigger::Activate, foreground);
    crate::log_line(&format!("bar show -> {out:?}"));
    // 看门狗只在窗口存在期间跑；后台激活也要留着它，回到前台靠它复活。
    unsafe { SetTimer(hwnd, TIMER_WATCH, WATCH_MS, None) };
}

/// 浮条消失（`Deactivate` 时调，也就是用户真的把这个输入法关了）。
pub fn hide() {
    let hwnd = our_hwnd();
    let was_visible = (hwnd as isize) != 0 && unsafe { IsWindowVisible(hwnd) } != 0;
    // 没窗口也要走一遍：把本线程的"想显示"清掉。先隐藏、确认不可见再释放所有权。
    drive_this_thread(hwnd, Trigger::Deactivate, foreground_is_self);
    if (hwnd as isize) == 0 {
        return;
    }
    if was_visible {
        crate::log_line("bar hide");
    }
    unsafe { KillTimer(hwnd, TIMER_WATCH) };
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
            // 记下来的位置是浮条**本体**的左上角（老版本没有彩光边，存的就是它）；
            // 窗口要往外扩一圈彩光边，所以左上角往左上挪 `glow_pad`。
            let (w, h) = outer_size();
            let g = glow_pad();
            let (x, y) = crate::state::read_pos().unwrap_or_else(default_position);
            let (x, y) = fit_on_screen(x, y);
            MoveWindow(hwnd, x - g, y - g, w, h, 0);
            crate::log_line(&format!("bar: created {w}x{h} at {x},{y}"));
            if GLOW_ON.load(Ordering::SeqCst) {
                SetTimer(hwnd, TIMER_GLOW, GLOW_MS, None);
            }
        }
    }
}

/// 把浮条本体（左上角 `(x, y)`、大小 `w × h`）整个挪进工作区 `work`。纯函数，可单测。
///
/// 只挪本体，四周透明的彩光边出界无所谓（鼠标本来就穿过去）。比工作区还大就贴左/贴顶。
fn clamp_into(work: &RECT, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    let x = x.min(work.right - w).max(work.left);
    let y = y.min(work.bottom - h).max(work.top);
    (x, y)
}

/// 离 `(x, y)` 最近的那块显示器的工作区（已扣任务栏）。取不到退回主显示器。
fn work_area_near(x: i32, y: i32) -> RECT {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if !mon.is_null() && GetMonitorInfoW(mon, &mut mi) != 0 {
            return mi.rcWork;
        }
        let mut area: RECT = std::mem::zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
        area
    }
}

/// 浮条本体左上角挪进它所在（最近）那块屏幕的工作区。
///
/// 2026-11-06 用户报：存的位置是 (1776, 983)，屏幕右边只到 1910，浮条一宽 → AI 格整个
/// 在屏幕外点不到。`state::read_pos` 只挡了 ±10000 的离谱值，不知道窗口多大、屏幕多大，
/// 所以在这里（知道两者的地方）挪。按本体中心找显示器：多屏时留在原来那块屏上。
fn fit_on_screen(x: i32, y: i32) -> (i32, i32) {
    let (w, h) = window_size();
    clamp_into(&work_area_near(x + w / 2, y + h / 2), x, y, w, h)
}

/// 每次显示前检查一遍：分辨率变小、拔掉副屏之后，已经建好的浮条也要挪回来。
fn keep_on_screen(hwnd: HWND) {
    let mut wr: RECT = unsafe { std::mem::zeroed() };
    if unsafe { GetWindowRect(hwnd, &mut wr) } == 0 {
        return;
    }
    let g = glow_pad();
    let (cur_x, cur_y) = (wr.left + g, wr.top + g);
    // 位置是全局记一份的，但浮条窗口是**每个进程一条**（TSF 按线程激活）。
    // 只有建窗口时读一次的话：宿主的浮条窗口早建好，用户在别的程序里把它拖走之后，
    // 这边还待在老地方 —— 同一个浮条在不同的程序里出现在屏幕不同位置
    // （2026-11-06 用户反馈"一会在左边一会在右边"）。所以每次显示都跟一次。
    let want = crate::state::read_pos().unwrap_or((cur_x, cur_y));
    // 出界（换分辨率、拔掉副屏）也要挪回来
    let (x, y) = fit_on_screen(want.0, want.1);
    if (x, y) != (cur_x, cur_y) {
        let (w, h) = outer_size();
        unsafe { MoveWindow(hwnd, x - g, y - g, w, h, 0) };
        if (x, y) != want {
            crate::state::write_pos(x, y);
            crate::log_line(&format!("bar: off-screen {},{} -> {x},{y}", want.0, want.1));
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

unsafe fn draw_badge(hdc: HDC, r: &RECT) -> RECT {
    // 图标比格子小一圈，留出边距；尺寸正好是 ICO 里的某一帧，1:1 贴上去，不经过缩放。
    let side = badge_side();
    let mut h = BADGE.load(Ordering::Relaxed);
    if h == 0 {
        let icon = load_badge(side);
        if icon.is_null() {
            return RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
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
    RECT {
        left: x,
        top: y,
        right: x + side,
        bottom: y + side,
    }
}

/// 暗色主题下把品牌标的深蓝底换成橙底，白色 `i` 原样保留。
///
/// 图标是位图，GDI 没法直接换色，只能在像素上重新映射。识别靠"蓝比红多多少"：
/// 深蓝底是 `#062657`（b − r = 81），白字形和浮条底色都接近中性（b − r ≈ 0~4），
/// 所以 `navy = (b−r)/81` 正好是"这个像素有多少是深蓝底"的占比；
/// 白色字形占比用亮度估。两者取大的当"图标覆盖率"，边缘像素才能和底色融得自然，
/// 图标圆角外面的背景像素（覆盖率≈0）一个都不动。
fn recolor_badge_to_orange(px: &mut [u32], w: i32, r: &RECT) {
    for y in r.top..r.bottom {
        for x in r.left..r.right {
            let i = (y * w + x) as usize;
            if i >= px.len() {
                continue;
            }
            let p = px[i];
            let cr = ((p >> 16) & 0xFF) as f32;
            let cg = ((p >> 8) & 0xFF) as f32;
            let cb = (p & 0xFF) as f32;

            let navy = ((cb - cr) / 81.0).clamp(0.0, 1.0);
            let luma = (cr * 0.299 + cg * 0.587 + cb * 0.114) as f32;
            let glyph = ((luma - 120.0) / 135.0).clamp(0.0, 1.0);
            let cover = navy.max(glyph);
            if cover <= 0.02 {
                continue;
            }

            // 目标色：底色用橙，往上按字形占比插值到白
            let g = (glyph / cover.max(0.02)).clamp(0.0, 1.0);
            let mix = |a: f32, b: f32| a + (b - a) * g;
            let (tr, tg, tb) = (mix(255.0, 255.0), mix(158.0, 255.0), mix(61.0, 255.0));

            // 再按覆盖率跟原像素混合（抗锯齿边缘）
            let nr = cr + (tr - cr) * cover;
            let ng = cg + (tg - cg) * cover;
            let nb = cb + (tb - cb) * cover;
            px[i] = (p & 0xFF00_0000) | ((nr as u32) << 16) | ((ng as u32) << 8) | (nb as u32);
        }
    }
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
pub(crate) struct Canvas {
    pub(crate) dc: HDC,
    dib: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub(crate) w: i32,
    pub(crate) h: i32,
}

impl Canvas {
    pub(crate) unsafe fn new(w: i32, h: i32) -> Option<Canvas> {
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
    pub(crate) unsafe fn pixels(&mut self) -> &mut [u32] {
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
pub(crate) fn round_rect_sdf(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (hx, hy) = ((x1 - x0) / 2.0, (y1 - y0) / 2.0);
    let r = r.min(hx).min(hy).max(0.0);
    let qx = (px - cx).abs() - (hx - r);
    let qy = (py - cy).abs() - (hy - r);
    let outside = (qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// 距离 → 覆盖率：一个像素宽的线性过渡。
pub(crate) fn coverage(d: f32) -> f32 {
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
unsafe fn draw_menu_glyph(hdc: HDC, r: &RECT, color: u32) {
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
        fill_rect(hdc, &bar, color);
    }
}

/// 画主题格：亮色一个太阳（实心圆 + 四道短光），暗色一个月牙（圆 - 一个底色圆）。
///
/// 用 `fill_round_aa` 画圆，和整条的圆角同一套抗锯齿；
/// 直接用 GDI 的 `Ellipse` 在 8px 直径下会变成一块方砧。

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
/// 中文标点：空心圈「。」，线宽 1.6px；半角：实心小点「.」—— 一眼能分出是哪种。
/// 逗号两种状态一样。
///
/// 两个符号不上下对齐：句号左移、逗号右移各 1 个设计像素（`k` 是 DPI 缩放，
/// 96 DPI 时为 1.0），和手写的「。，」一样错开一点（用户给的参考图）。
fn punct_glyphs(cell: &RECT, chinese_punct: bool, k: f32) -> [Glyph; 2] {
    let center = (cell.left + cell.right) as f32 / 2.0;
    let period_cx = center - k;
    let comma_cx = center + k;
    let mid = (cell.top + cell.bottom) as f32 / 2.0;
    // 句号外径 6.4px，和逗号圆头（直径 4.6px）只差一档 —— 原来外径 8.7px，
    // 比逗号大出一倍，看着头重脚轻。逗号尾巴往下多伸 ~7px，所以两者整体往上提，
    // 让"圈 + 逗号头"这两团墨落在格子正中，细尾巴不算重量。
    let upper = mid - 4.6 * k;
    let lower = mid + 2.6 * k;
    let top = if chinese_punct {
        Glyph::Ring {
            cx: period_cx,
            cy: upper,
            r: 2.4 * k,
            hw: 0.8 * k,
        }
    } else {
        Glyph::Dot {
            cx: period_cx,
            cy: upper,
            r: 1.8 * k,
        }
    };
    [
        top,
        Glyph::Comma {
            cx: comma_cx,
            cy: lower,
            k,
        },
    ]
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
unsafe fn paint_bar(c: &mut Canvas, hover: i32, dark: bool) {
    let (w, h) = (c.w, c.h);
    let client = RECT {
        left: 0,
        top: 0,
        right: w,
        bottom: h,
    };
    // 主题由调用方决定（`present()` 读一次注册表就够；悬停重绘不必再读）。
    let pal = bar_palette(dark);

    fill_rect(c.dc, &client, pal.bg);

    let font = get_font();
    let rects = item_rects();
    // 每次现读：Shift 在别的动作里切了中英，这里立刻反映；不缓存。
    let (chinese, punct) = crate::state::snapshot();
    PAINTED_CHINESE.store(chinese, Ordering::Relaxed);
    PAINTED_CAPS.store(crate::caps_on(), Ordering::Relaxed);
    PAINTED_PUNCT.store(punct, Ordering::Relaxed);
    PAINTED_VALID.store(true, Ordering::Relaxed);

    // 悬停只在当前格后面垫一层浅色圆角，不改文字颜色。
    // 只缩 1px：格高 28、品牌标 24，缩多了垫底会被品牌标整个盖住，悬停没反馈。
    if (0..5).contains(&hover) {
        let r = inset(&rects[hover as usize], scale(1));
        fill_round_aa(c.pixels(), w, h, &r, scale(6) as f32, pal.hover);
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
    fill_rect(hdc, &sep, pal.border);

    // 第一格：品牌标（浮条里唯一一个"重"的元素，和搜狗那个橙色 S 一个位置）
    let badge = draw_badge(hdc, &rects[0]);
    if dark && badge.right > badge.left {
        // 暗色下深蓝底在深底上几乎看不见，换成橙底白字（用户指定）
        recolor_badge_to_orange(c.pixels(), w, &badge);
    }

    // 第二格：中/英。开=激活色，关=灰。
    // Caps Lock 开着时显示 A：这时打出来的就是大写字母，只显示"中"会误导
    let (mode_text, mode_fg) = if crate::caps_on() {
        ("A", pal.accent)
    } else if chinese {
        ("中", pal.accent)
    } else {
        ("英", pal.dim)
    };
    draw_text_centered(hdc, &rects[1], mode_text, mode_fg, font);

    // 第三格：中文标点 / 半角。像搜狗那样**上下叠放**（上句号、下逗号）。
    // 两个符号都用 GDI 画形状而不是打字体 —— 15px 字号下全角标点只剩一粒灰点。
    let cell = rects[2];
    let punct_fg = if punct { pal.accent } else { pal.dim };
    draw_glyphs_aa(hdc, &cell, &punct_glyphs(&cell, punct, dpi_k()), punct_fg);

    // 第四格：AI（点开是浮动面板，三个动作在面板里）。
    draw_text_centered(hdc, &rects[3], "AI", pal.accent, font);

    // 第五格：菜单。
    draw_menu_glyph(hdc, &rects[4], pal.text);

    // 状态字盖住整条（AI 处理中 / 失败）。放在 apply_frame 之前：
    // 圆角外的那几个像素必须保持透明，否则窗口会露出方形底。
    if let Some(text) = status_text() {
        let full = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        fill_rect(hdc, &full, pal.bg);
        draw_text_centered(hdc, &full, &text, pal.text, font);
    }

    // 外形：圆角 + 描边，都抗锯齿。描边 1px（高 DPI 跟着放大）。
    let border_w = scale(1).max(1) as f32;
    apply_frame(c.pixels(), w, h, scale(RADIUS) as f32, border_w, pal.border);
}

/// 画一帧并交给系统合成。窗口尺寸取 `window_size()`，位置不动（`pptDst` 传空）。
/// 在浮条上盖一行状态字，`ttl_ms` 之后自动消失（0 = 立刻清掉）。
pub fn set_status(text: &str, ttl_ms: u64) {
    {
        let mut g = STATUS.lock().unwrap_or_else(|p| p.into_inner());
        if ttl_ms == 0 {
            *g = None;
        } else {
            *g = Some((
                text.to_string(),
                Instant::now() + Duration::from_millis(ttl_ms),
            ));
        }
    }
    let hwnd = our_hwnd();
    if (hwnd as isize) != 0 {
        // 从编辑会话里调用时不能直接画（那会在 TSF 回调里做 GDI），投递到窗口队列
        unsafe {
            PostMessageW(hwnd, WM_STATUS, 0, 0);
        }
    }
}

/// 当前还没过期的状态字。过期的顺手清掉。
fn status_text() -> Option<String> {
    let mut g = STATUS.lock().unwrap_or_else(|p| p.into_inner());
    match g.as_ref() {
        Some((text, until)) if Instant::now() < *until => Some(text.clone()),
        Some(_) => {
            *g = None;
            None
        }
        None => None,
    }
}

/// 看门狗用：把已经过期的状态清掉，返回"清掉了"（调用方据此重画）。
fn clear_expired_status() -> bool {
    let mut g = STATUS.lock().unwrap_or_else(|p| p.into_inner());
    match g.as_ref() {
        Some((_, until)) if Instant::now() >= *until => {
            *g = None;
            true
        }
        _ => false,
    }
}

// ── AI 面板开着时的外圈彩光 ─────────────────────────────────────────────────
//
// 一圈贴着浮条外沿、一直在转的渐变光：颜色沿圆周按色标插值，色标大半是紫系
// （紫罗兰 / 紫 / 品红 / 靛蓝），中间夹一点粉、琥珀、青、蓝 —— "五彩、以紫为主"。
// 亮度 = 贴边一道细亮线 + 往外按指数衰减的光晕；再加一个跟着转的高光点，转动才看得出来。
// 全是纯函数，逐像素算（浮条外框 ~270×60，一帧几万次乘加，不到 1ms）。

/// 彩光的色标：`(位置 0..1, 0xRRGGBB)`。首尾同色，绕一圈接得上。
const GLOW_STOPS: [(f32, u32); 11] = [
    (0.00, 0x8B5CF6), // 紫罗兰
    (0.14, 0xA855F7), // 紫
    (0.26, 0xD946EF), // 品红
    (0.36, 0xEC4899), // 粉
    (0.44, 0xF59E0B), // 琥珀
    (0.52, 0x8B5CF6), // 紫罗兰
    (0.64, 0x6366F1), // 靛蓝
    (0.74, 0x22D3EE), // 青
    (0.82, 0x3B82F6), // 蓝
    (0.91, 0xA855F7), // 紫
    (1.00, 0x8B5CF6), // 紫罗兰
];

/// 沿圆周位置 `t`（0..1）上的颜色，返回 `(r, g, b)`（0..255）。
fn glow_color(t: f32) -> (f32, f32, f32) {
    let t = t.rem_euclid(1.0);
    let split = |c: u32| {
        (
            ((c >> 16) & 0xFF) as f32,
            ((c >> 8) & 0xFF) as f32,
            (c & 0xFF) as f32,
        )
    };
    for pair in GLOW_STOPS.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if t >= t0 && t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            let (a, b) = (split(c0), split(c1));
            return (
                a.0 + (b.0 - a.0) * f,
                a.1 + (b.1 - a.1) * f,
                a.2 + (b.2 - a.2) * f,
            );
        }
    }
    split(GLOW_STOPS[0].1)
}

/// 外圈彩光的一个像素：`(alpha 0..1, r, g, b)`，**未预乘**。
///
/// - `bar`：浮条本体在画布里的矩形，`radius` 是它的圆角；
/// - `pad`：画布四周留了多宽的边（光晕在这之前淡完，不会被画布边缘切出硬边）；
/// - `phase`：转到哪儿了（0..1），`k`：DPI 缩放。
#[allow(clippy::too_many_arguments)]
fn glow_pixel(
    px: f32,
    py: f32,
    bar: (f32, f32, f32, f32),
    radius: f32,
    pad: f32,
    phase: f32,
    k: f32,
) -> (f32, f32, f32, f32) {
    let (x0, y0, x1, y1) = bar;
    let d = round_rect_sdf(px, py, x0, y0, x1, y1, radius);
    // 浮条里面不发光（本体会盖上去）；留 1.5px 让它和本体的抗锯齿边接上
    if d < -1.5 * k {
        return (0.0, 0.0, 0.0, 0.0);
    }
    // 贴边的细亮线（中心在外沿外 0.8px，宽约 3px）
    let core = (1.0 - (d - 0.8 * k).abs() / (1.6 * k)).clamp(0.0, 1.0);
    // 往外衰减的光晕
    let halo = (-d.max(0.0) / (2.4 * k)).exp() * 0.55;
    // 到画布边缘前淡完
    let fade = ((pad - 0.5 * k - d) / (1.5 * k)).clamp(0.0, 1.0);

    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (hw, hh) = (((x1 - x0) / 2.0).max(1.0), ((y1 - y0) / 2.0).max(1.0));
    // 先按长宽归一化再取角度：长条形的浮条，角度沿周边走得才均匀
    let angle = ((py - cy) / hh).atan2((px - cx) / hw);
    let t = (angle / std::f32::consts::TAU + 0.5 + phase).rem_euclid(1.0);
    let (mut r, mut g, mut b) = glow_color(t);

    // 跟着转的高光：t 接近 0 的那一小段提亮、往白里靠
    let dist = t.min(1.0 - t);
    let spark = (-(dist / 0.07) * (dist / 0.07)).exp();
    r += (255.0 - r) * 0.55 * spark;
    g += (255.0 - g) * 0.55 * spark;
    b += (255.0 - b) * 0.55 * spark;

    let alpha = (core.max(halo) * (0.78 + 0.22 * spark) * fade).clamp(0.0, 1.0);
    (alpha, r, g, b)
}

/// 把整圈彩光画进一张（全透明的）预乘 alpha 画布。
fn paint_glow(px: &mut [u32], w: i32, h: i32, bar: (f32, f32, f32, f32), radius: f32, phase: f32) {
    let k = dpi_k();
    let pad = glow_pad() as f32;
    for y in 0..h {
        for x in 0..w {
            let (a, r, g, b) =
                glow_pixel(x as f32 + 0.5, y as f32 + 0.5, bar, radius, pad, phase, k);
            if a <= 0.0 {
                continue;
            }
            let ai = (a * 255.0 + 0.5) as u32;
            let pm = |c: f32| ((c * a) + 0.5).min(255.0) as u32;
            if let Some(slot) = px.get_mut((y * w + x) as usize) {
                *slot = (ai << 24) | (pm(r) << 16) | (pm(g) << 8) | pm(b);
            }
        }
    }
}

/// 预乘 alpha 的"源盖在目标上"：`out = src + dst × (1 − src.a)`。
fn over_premul(src: u32, dst: u32) -> u32 {
    let sa = src >> 24;
    if sa == 255 {
        return src;
    }
    let inv = 255 - sa;
    let ch = |shift: u32| {
        let s = (src >> shift) & 0xFF;
        let d = (dst >> shift) & 0xFF;
        (s + (d * inv + 127) / 255).min(255)
    };
    (ch(24) << 24) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// 拼出整个窗口的像素：四周 `glow_pad` 的边 + 正中的浮条本体。
///
/// `dst` 是外框大小（`outer_size()`）的画布，`bar` 是本体（`bw × bh`，预乘 alpha）。
/// `glow` 给了相位就先画彩光、本体盖在上面；`None` 时外圈全透明（鼠标会穿过去）。
fn compose_outer(dst: &mut [u32], bar: &[u32], bw: i32, bh: i32, glow: Option<f32>) {
    let g = glow_pad();
    let (w, h) = (bw + 2 * g, bh + 2 * g);
    dst.fill(0);
    if let Some(phase) = glow {
        let rect = (g as f32, g as f32, (g + bw) as f32, (g + bh) as f32);
        paint_glow(dst, w, h, rect, scale(RADIUS) as f32, phase);
    }
    for y in 0..bh {
        for x in 0..bw {
            let (Some(&s), Some(d)) = (
                bar.get((y * bw + x) as usize),
                dst.get_mut(((y + g) * w + (x + g)) as usize),
            ) else {
                continue;
            };
            *d = over_premul(s, *d);
        }
    }
}

unsafe fn present(hwnd: HWND) -> bool {
    let (bw, bh) = window_size();
    let Some(mut c) = Canvas::new(bw, bh) else {
        return false;
    };
    let dark = crate::config::snapshot().dark;
    PAINTED_DARK.store(dark, Ordering::Relaxed);
    paint_bar(&mut c, HOVER.load(Ordering::Relaxed), dark);

    // 外框：四周留出彩光的边。本体画在 (g, g)，外圈要么全透明，要么是彩光。
    let (w, h) = outer_size();
    let Some(mut outer) = Canvas::new(w, h) else {
        return false;
    };
    let glow = GLOW_ON.load(Ordering::SeqCst).then(glow_phase);
    let bar_px = c.pixels().to_vec();
    compose_outer(outer.pixels(), &bar_px, bw, bh, glow);
    let c = outer;
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

/// 切亮/暗。状态存在注册表的 `Config\Dark`，和设置窗里那个复选框是同一个值——
/// 浮条点一下、设置窗里也会跟着变（下次打开时现读）。
fn toggle_theme(hwnd: HWND) {
    let on = !crate::config::snapshot().dark;
    crate::config::set_dark(on);
    crate::log_line(&format!("bar toggle theme -> dark={on}"));
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
        crate::menu::CMD_TOGGLE_THEME => toggle_theme(hwnd),
        crate::menu::CMD_SETTINGS => open_settings(),
        _ => {}
    }
}

/// 打开设置窗（`inklink-settings.exe`，和 DLL 装在同一个目录）。
///
/// 用 `CreateProcessW` 而不是 `ShellExecuteW`：后者会走 shell 关联、可能弹 UAC，
/// 而且我们要的是"同一个目录下的兄弟程序"，路径自己算得出来。
/// 失败不弹窗（在宿主进程里弹窗会打断用户打字），只写日志。
pub(crate) fn open_settings() {
    let dll = crate::registry::dll_path();
    let dir = match std::path::Path::new(&dll).parent() {
        Some(d) => d.to_path_buf(),
        None => {
            crate::log_line("settings: 拿不到 DLL 目录");
            return;
        }
    };
    let exe = dir.join("inklink-settings.exe");
    if !exe.exists() {
        crate::log_line(&format!("settings: 找不到 {}", exe.display()));
        return;
    }
    let app = wide(&exe.to_string_lossy());
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        CreateProcessW(
            app.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null_mut(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    } != 0;
    if ok {
        unsafe {
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
        }
        crate::log_line("settings: launched");
    } else {
        crate::log_line("settings: CreateProcessW failed");
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
            let (x, y) = to_bar_coords(x, y);
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
                    // 传 0 会把窗口缩成 0x0（浮条"拖一下就不见了"）。这里是整个窗口（含彩光边）。
                    let (w, h) = outer_size();
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
            let (x, y) = to_bar_coords(x, y);
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
                // 存本体的左上角，和老版本存的是同一个东西。拖到一半出了屏幕就先挪回来，
                // 否则存下的位置下次一打开就有几格点不到（AI 格在最右边，最先出界）。
                let g = glow_pad();
                let (x, y) = fit_on_screen(wr.left + g, wr.top + g);
                if (x, y) != (wr.left + g, wr.top + g) {
                    let (w, h) = outer_size();
                    MoveWindow(hwnd, x - g, y - g, w, h, 0);
                }
                crate::state::write_pos(x, y);
            } else {
                match pressed {
                    // 品牌标 / 最右格：等同点 ☰（投递到队列，不在这里直接弹）
                    0 | 4 => {
                        PostMessageW(hwnd, WM_OPEN_MENU, 0, 0);
                    }
                    1 => toggle_mode(hwnd),
                    2 => toggle_punct(hwnd),
                    3 => crate::ai::open_panel(),
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
                // AI 有没有启用现读注册表：菜单每次弹出都可能是刚在设置里勾上的
                let dark = crate::config::snapshot().dark;
                // 菜单贴着浮条本体弹，不是贴着外圈彩光边
                let g = glow_pad();
                let cmd = crate::menu::popup(hwnd, wr.left + g, wr.top + g, chinese, punct, dark);
                MENU_OPEN.store(false, Ordering::SeqCst);
                if cmd != 0 {
                    crate::log_line(&format!("bar: menu choice {cmd}"));
                }
                handle_menu(hwnd, cmd);
            }
            0
        }
        WM_STATUS => {
            redraw(hwnd);
            0
        }
        crate::ai::WM_AI_DONE => {
            // 引擎的后台线程发完 HTTP 了：这里取结果并排一次写回（见 ai.rs / lib.rs）。
            // 只做内存读 + 排一个编辑会话，不阻塞宿主。
            crate::ai::on_done(wparam as u32);
            0
        }
        WM_TIMER if wparam == TIMER_GLOW => {
            // 彩光动画一帧。面板关了就停表；浮条收着（宿主在后台）就不画，省得白忙。
            if !GLOW_ON.load(Ordering::SeqCst) {
                KillTimer(hwnd, TIMER_GLOW);
                redraw(hwnd);
            } else if IsWindowVisible(hwnd) != 0 {
                present(hwnd);
            }
            0
        }
        WM_TIMER => {
            // 看门狗：本进程不是前台就把浮条收走（`Deactivate` 在 Alt+Tab 切程序时不会来），
            // 回到前台、本线程还想显示、而且进程里没有别的可见浮条时才复活 —— 能不能显示由
            // `VISIBLE_OWNER` 的比较交换决定，几条线程的看门狗同时到点也只放出一条。
            // 停用过的线程（`Deactivate`）不会被它弹回来。
            if wparam == TIMER_WATCH {
                drive_this_thread(hwnd, Trigger::Tick, foreground_is_self);
            }
            let wanted = INSTANCE_VISIBILITY.with(|cell| cell.get().wanted);
            if wparam == TIMER_WATCH && wanted {
                // 状态是全局的（注册表）：别的程序里点了中/英或标点，这一条也要跟着变，
                // 否则同一时刻不同窗口里的浮条会显示不同的状态。
                let (chinese, punct) = crate::state::snapshot();
                let dark = crate::config::snapshot().dark;
                if !PAINTED_VALID.load(Ordering::Relaxed)
                    || chinese != PAINTED_CHINESE.load(Ordering::Relaxed)
                    || punct != PAINTED_PUNCT.load(Ordering::Relaxed)
                    || dark != PAINTED_DARK.load(Ordering::Relaxed)
                    || crate::caps_on() != PAINTED_CAPS.load(Ordering::Relaxed)
                {
                    redraw(hwnd);
                }
            }
            // 状态字到点了就擦掉（否则会一直盖着浮条）
            if clear_expired_status() {
                redraw(hwnd);
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

    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    // -----------------------------------------------------------------------
    // 内存模拟宿主（可见性仲裁的测试设施）
    //
    // 一个 `SimHost` 模拟一个宿主进程：若干条 TSF 线程（实例）各有自己的
    // `InstanceVisibility` 和窗口句柄；全进程共享一个局部 `AtomicIsize`（代替进程级
    // 所有权静态量）和一张窗口表。`deliver` 只调用 `drive_visibility`，编排逻辑一律走
    // 生产代码，模拟器不重写。
    //
    // 两条线程的请求重叠用"嵌套"模拟：`on_next_redraw` 挂的回调在下一次 `redraw` 里执行
    // —— 那时外层请求正握着仲裁哨兵，回调里再 `deliver` 别的实例，就等于另一条线程在
    // 仲裁进行中插了进来。回调和外层共用同一个原子、同一张窗口表。
    //
    // 属性测试在测试上方注明
    // `Feature: fix-duplicate-floating-bars-punctuation-layout, Property N: <属性原文>`。
    // -----------------------------------------------------------------------

    /// 模拟窗口句柄从这里往上编：第 i 个实例是 `SIM_HWND_BASE + i`，日志里好认。
    const SIM_HWND_BASE: isize = 101;

    /// 一次性故障开关：生效一次就自动复位。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SimFault {
        /// 下一次窗口枚举（`find_other_visible` 或 `hide_others`，谁先调用算谁的）
        /// 返回 `Unknown`，不动任何窗口。
        QueryUnknown,
        /// 下一次 `hide_others` 留下第一条（句柄最小的）可见的别的浮条不收，其余照收，
        /// 返回 `StillVisible`。没有别的可见浮条时照常返回 `AllHidden`，开关同样算用掉。
        CleanupLeavesVisible,
        /// 下一次对表里存在的窗口 `redraw` 返回 `false`。
        DrawFails,
        /// 下一次 `show` 之后窗口仍不可见。
        ShowFails,
        /// 下一次 `show` 之后宿主立刻失去前台。
        LoseForegroundAfterShow,
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct SimFaults {
        query_unknown: bool,
        cleanup_leaves_visible: bool,
        draw_fails: bool,
        show_fails: bool,
        lose_foreground_after_show: bool,
    }

    impl SimFaults {
        fn slot(&mut self, f: SimFault) -> &mut bool {
            match f {
                SimFault::QueryUnknown => &mut self.query_unknown,
                SimFault::CleanupLeavesVisible => &mut self.cleanup_leaves_visible,
                SimFault::DrawFails => &mut self.draw_fails,
                SimFault::ShowFails => &mut self.show_fails,
                SimFault::LoseForegroundAfterShow => &mut self.lose_foreground_after_show,
            }
        }

        /// 开关开着就关掉并返回 `true`（用一次就复位）。
        fn take(&mut self, f: SimFault) -> bool {
            std::mem::take(self.slot(f))
        }
    }

    /// 窗口副作用日志，按发生顺序记。检查"先隐藏旧的、再显示新的"就比这里的下标。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SimOp {
        /// `redraw(h)`；第二项是返回值。
        Redraw(isize, bool),
        /// `show(h)`；第二项是调用之后 `h` 是否真的可见。
        Show(isize, bool),
        /// `h` 被隐藏：`hide(h)` 每次调用都记；`hide_others` 只记真正收起的那几条。
        Hide(isize),
        /// `LoseForegroundAfterShow` 生效，宿主失去前台。
        LostForeground,
    }

    type SimHook = Box<dyn FnOnce(&SimHost)>;

    /// 全进程共享的模拟状态。
    #[derive(Default)]
    struct SimWorld {
        /// 句柄 → 是否可见。`BTreeMap` 让枚举顺序固定（句柄从小到大）。
        windows: BTreeMap<isize, bool>,
        foreground: bool,
        faults: SimFaults,
        log: Vec<SimOp>,
        /// 下一次 `redraw` 时执行的回调（一次性）。
        on_redraw: Option<SimHook>,
        /// `nest_on_next_redraw` 插入的嵌套请求及其结果，按执行顺序记。
        nested: Vec<(usize, Trigger, DisplayOutcome)>,
    }

    impl SimWorld {
        fn visible_windows(&self) -> Vec<isize> {
            self.windows
                .iter()
                .filter(|(_, v)| **v)
                .map(|(h, _)| *h)
                .collect()
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct SimInstance {
        /// 0 表示这条线程没有窗口（不登记进窗口表）。
        hwnd: isize,
        state: InstanceVisibility,
        /// 正在 `deliver` 中。一条线程不会在处理请求的中途自己再收到请求。
        in_flight: bool,
    }

    struct SimCore {
        owner: AtomicIsize,
        world: RefCell<SimWorld>,
        instances: RefCell<Vec<SimInstance>>,
    }

    /// 模拟宿主进程。`clone()` 出来的是同一个进程的另一个把手，共享全部状态。
    #[derive(Clone)]
    struct SimHost {
        core: Rc<SimCore>,
    }

    /// `DisplayIo` 的内存实现，和所属 `SimHost` 共用同一张窗口表、同一个原子。
    struct SimIo {
        core: Rc<SimCore>,
    }

    // 部分查询/设置方法到任务 1.3–1.6 的测试里才用上。
    #[allow(dead_code)]
    impl SimHost {
        /// `n` 个实例，句柄 `SIM_HWND_BASE..`，窗口全隐藏、实例状态全默认；
        /// 宿主在前台，原子所有权为 `OWNER_NONE`。
        fn new(n: usize) -> Self {
            let host = SimHost {
                core: Rc::new(SimCore {
                    owner: AtomicIsize::new(OWNER_NONE),
                    world: RefCell::new(SimWorld {
                        foreground: true,
                        ..SimWorld::default()
                    }),
                    instances: RefCell::new(Vec::new()),
                }),
            };
            for i in 0..n {
                host.add_instance(SIM_HWND_BASE + i as isize);
            }
            host
        }

        /// 再加一个实例，返回它的下标。`hwnd == 0` 表示这条线程没有窗口。
        fn add_instance(&self, hwnd: isize) -> usize {
            assert!(hwnd >= 0, "模拟句柄不能是负数（-1 是仲裁哨兵）");
            if hwnd != 0 {
                let prev = self.core.world.borrow_mut().windows.insert(hwnd, false);
                assert!(prev.is_none(), "模拟句柄 {hwnd} 重复");
            }
            let mut insts = self.core.instances.borrow_mut();
            insts.push(SimInstance {
                hwnd,
                state: InstanceVisibility::default(),
                in_flight: false,
            });
            insts.len() - 1
        }

        /// 把一次触发投递给第 `i` 个实例：只调用 `drive_visibility`，然后写回实例状态。
        fn deliver(&self, i: usize, trigger: Trigger) -> DisplayOutcome {
            let (hwnd, mut state) = {
                let mut insts = self.core.instances.borrow_mut();
                let inst = &mut insts[i];
                assert!(
                    !inst.in_flight,
                    "实例 {i} 还在处理上一个请求时又被投递了 {trigger:?}"
                );
                inst.in_flight = true;
                (inst.hwnd, inst.state)
            };
            let io = self.io();
            let out = drive_visibility(&self.core.owner, &mut state, hwnd, trigger, &io);
            let mut insts = self.core.instances.borrow_mut();
            insts[i].state = state;
            insts[i].in_flight = false;
            out
        }

        fn io(&self) -> SimIo {
            SimIo {
                core: Rc::clone(&self.core),
            }
        }

        fn hwnd(&self, i: usize) -> isize {
            self.core.instances.borrow()[i].hwnd
        }

        fn state(&self, i: usize) -> InstanceVisibility {
            self.core.instances.borrow()[i].state
        }

        fn set_state(&self, i: usize, state: InstanceVisibility) {
            self.core.instances.borrow_mut()[i].state = state;
        }

        /// 第 `i` 个实例的窗口此刻是否可见。
        fn visible(&self, i: usize) -> bool {
            let h = self.hwnd(i);
            self.core
                .world
                .borrow()
                .windows
                .get(&h)
                .copied()
                .unwrap_or(false)
        }

        fn visible_windows(&self) -> Vec<isize> {
            self.core.world.borrow().visible_windows()
        }

        fn visible_count(&self) -> usize {
            self.visible_windows().len()
        }

        fn foreground(&self) -> bool {
            self.core.world.borrow().foreground
        }

        fn set_foreground(&self, foreground: bool) {
            self.core.world.borrow_mut().foreground = foreground;
        }

        fn owner(&self) -> isize {
            self.core.owner.load(Ordering::SeqCst)
        }

        /// 直接改原子（例如预置成 `OWNER_ARBITRATING`，模拟别的仲裁者没结束）。
        fn set_owner(&self, value: isize) {
            self.core.owner.store(value, Ordering::SeqCst);
        }

        /// 打开一个一次性故障开关。
        fn arm(&self, f: SimFault) {
            *self.core.world.borrow_mut().faults.slot(f) = true;
        }

        fn is_armed(&self, f: SimFault) -> bool {
            *self.core.world.borrow_mut().faults.slot(f)
        }

        /// 直接改可见位：不记日志、不做 ≤1 检查。用来摆出"没经过仲裁就可见"的遗留窗口；
        /// 句柄不在表里就当新窗口加进去。
        fn force_visible(&self, h: isize, visible: bool) {
            self.core.world.borrow_mut().windows.insert(h, visible);
        }

        fn log(&self) -> Vec<SimOp> {
            self.core.world.borrow().log.clone()
        }

        fn clear_log(&self) {
            self.core.world.borrow_mut().log.clear();
        }

        /// 下一次 `redraw` 时执行 `hook`（一次性）。外层请求那时正握着仲裁哨兵。
        fn on_next_redraw(&self, hook: impl FnOnce(&SimHost) + 'static) {
            let mut w = self.core.world.borrow_mut();
            assert!(w.on_redraw.is_none(), "上一个重绘回调还没执行");
            w.on_redraw = Some(Box::new(hook));
        }

        /// 下一次 `redraw` 时给第 `i` 个实例投递 `trigger`，结果记进 `nested_outcomes`。
        fn nest_on_next_redraw(&self, i: usize, trigger: Trigger) {
            self.on_next_redraw(move |h| {
                let out = h.deliver(i, trigger);
                h.core.world.borrow_mut().nested.push((i, trigger, out));
            });
        }

        fn nested_outcomes(&self) -> Vec<(usize, Trigger, DisplayOutcome)> {
            self.core.world.borrow().nested.clone()
        }
    }

    impl DisplayIo for SimIo {
        fn is_foreground(&self) -> bool {
            self.core.world.borrow().foreground
        }

        fn is_visible(&self, h: isize) -> bool {
            self.core
                .world
                .borrow()
                .windows
                .get(&h)
                .copied()
                .unwrap_or(false)
        }

        fn hide_others(&self, me: isize) -> CleanupResult {
            let mut w = self.core.world.borrow_mut();
            if w.faults.take(SimFault::QueryUnknown) {
                return CleanupResult::Unknown;
            }
            let leave_one = w.faults.take(SimFault::CleanupLeavesVisible);
            let others: Vec<isize> = w
                .visible_windows()
                .into_iter()
                .filter(|&h| h != me)
                .collect();
            let mut stuck = None;
            for h in others {
                if leave_one && stuck.is_none() {
                    stuck = Some(h);
                    continue;
                }
                w.windows.insert(h, false);
                w.log.push(SimOp::Hide(h));
            }
            match stuck {
                Some(h) => CleanupResult::StillVisible(h),
                None => CleanupResult::AllHidden,
            }
        }

        fn find_other_visible(&self, me: isize) -> VisibilityQuery {
            let mut w = self.core.world.borrow_mut();
            if w.faults.take(SimFault::QueryUnknown) {
                return VisibilityQuery::Unknown;
            }
            match w.visible_windows().into_iter().find(|&h| h != me) {
                Some(h) => VisibilityQuery::Visible(h),
                None => VisibilityQuery::NoneVisible,
            }
        }

        fn redraw(&self, me: isize) -> bool {
            let (ok, hook) = {
                let mut w = self.core.world.borrow_mut();
                let ok = w.windows.contains_key(&me) && !w.faults.take(SimFault::DrawFails);
                w.log.push(SimOp::Redraw(me, ok));
                (ok, w.on_redraw.take())
            };
            // 先放掉借用再跑回调：回调里的嵌套请求要读写同一张窗口表。
            if let Some(hook) = hook {
                hook(&SimHost {
                    core: Rc::clone(&self.core),
                });
            }
            ok
        }

        fn show(&self, me: isize) {
            let mut w = self.core.world.borrow_mut();
            assert!(
                w.windows.contains_key(&me),
                "show 了窗口表里没有的句柄 {me}"
            );
            let visible = !w.faults.take(SimFault::ShowFails);
            if visible {
                w.windows.insert(me, true);
            }
            w.log.push(SimOp::Show(me, visible));
            let shown = w.visible_windows();
            assert!(
                shown.len() <= 1,
                "show({me}) 之后同时有 {} 条可见：{shown:?}；操作日志 {:?}",
                shown.len(),
                w.log
            );
            if w.faults.take(SimFault::LoseForegroundAfterShow) {
                w.foreground = false;
                w.log.push(SimOp::LostForeground);
            }
        }

        fn hide(&self, me: isize) {
            let mut w = self.core.world.borrow_mut();
            if let Some(v) = w.windows.get_mut(&me) {
                *v = false;
            }
            w.log.push(SimOp::Hide(me));
        }
    }

    #[test]
    fn 模拟宿主的故障开关用一次就复位() {
        let host = SimHost::new(2);
        let io = host.io();
        let (a, b) = (host.hwnd(0), host.hwnd(1));

        host.arm(SimFault::DrawFails);
        assert!(!io.redraw(a), "开关开着：这次重绘失败");
        assert!(!host.is_armed(SimFault::DrawFails), "用一次就复位");
        assert!(io.redraw(a), "下一次重绘照常成功");

        host.arm(SimFault::ShowFails);
        io.show(a);
        assert!(!host.visible(0), "显示失败：调用之后仍不可见");
        io.show(a);
        assert!(host.visible(0), "下一次显示照常生效");

        host.arm(SimFault::QueryUnknown);
        assert_eq!(io.find_other_visible(b), VisibilityQuery::Unknown);
        assert_eq!(io.find_other_visible(b), VisibilityQuery::Visible(a));

        host.arm(SimFault::CleanupLeavesVisible);
        assert_eq!(io.hide_others(b), CleanupResult::StillVisible(a));
        assert!(host.visible(0), "收不起来的那条仍可见");
        assert_eq!(io.hide_others(b), CleanupResult::AllHidden);
        assert!(!host.visible(0), "下一次清理照常收起");

        host.arm(SimFault::LoseForegroundAfterShow);
        io.show(b);
        assert!(host.visible(1), "显示本身成功");
        assert!(!host.foreground(), "显示之后宿主随即失去前台");
        host.set_foreground(true);
        io.hide(b);
        io.show(b);
        assert!(host.foreground(), "开关已复位，这次不再丢前台");

        host.arm(SimFault::QueryUnknown);
        assert_eq!(io.hide_others(a), CleanupResult::Unknown);
        assert!(host.visible(1), "枚举不确定时不动任何窗口");
        assert_eq!(host.visible_count(), 1);

        assert_eq!(
            host.log(),
            vec![
                SimOp::Redraw(101, false),
                SimOp::Redraw(101, true),
                SimOp::Show(101, false),
                SimOp::Show(101, true),
                SimOp::Hide(101),
                SimOp::Show(102, true),
                SimOp::LostForeground,
                SimOp::Hide(102),
                SimOp::Show(102, true),
            ]
        );
    }

    #[test]
    #[should_panic(expected = "条可见")]
    fn 模拟宿主显示第二条浮条时立刻断言失败() {
        let host = SimHost::new(2);
        host.force_visible(host.hwnd(0), true);
        host.io().show(host.hwnd(1));
    }

    #[test]
    fn 模拟宿主的重绘回调能插入另一条线程的请求() {
        let host = SimHost::new(2);
        let io = host.io();
        // 外层请求握着仲裁哨兵时进入 redraw；回调看到的必须是同一个原子、同一张窗口表。
        host.set_owner(OWNER_ARBITRATING);
        let seen = Rc::new(Cell::new(OWNER_NONE));
        let probe = Rc::clone(&seen);
        host.on_next_redraw(move |h| {
            probe.set(h.owner());
            h.set_foreground(false);
        });
        assert!(io.redraw(host.hwnd(0)));
        assert_eq!(seen.get(), OWNER_ARBITRATING, "回调和外层共用同一个原子");
        assert!(!host.foreground(), "回调和外层共用同一张窗口表");

        host.nest_on_next_redraw(1, Trigger::Activate);
        assert!(io.redraw(host.hwnd(0)));
        assert!(io.redraw(host.hwnd(0)));
        let nested: Vec<(usize, Trigger)> = host
            .nested_outcomes()
            .iter()
            .map(|&(i, t, _)| (i, t))
            .collect();
        assert_eq!(
            nested,
            vec![(1, Trigger::Activate)],
            "嵌套请求只在下一次重绘里执行一次"
        );
    }

    // -----------------------------------------------------------------------
    // 实例状态归约（`reduce_visibility`）
    //
    // 期望值逐行抄自设计文档 "Pure Visibility State Reducer" 的规则表，写成字面量，
    // 不调用被测函数去算。每张表把该行适用的输入状态都列全。
    // -----------------------------------------------------------------------

    type Ev = VisibilityEvent;
    type Act = VisibilityAction;

    const fn iv(wanted: bool, auto_hidden: bool) -> InstanceVisibility {
        InstanceVisibility {
            wanted,
            auto_hidden,
        }
    }

    /// 一行归约用例：输入状态、事件、期望的新状态、期望的动作。
    type ReduceRow = (
        InstanceVisibility,
        VisibilityEvent,
        InstanceVisibility,
        VisibilityAction,
    );

    /// 逐行跑归约，把不符的行全部收集起来一起报，不在第一行就停。
    fn check_reduce_rows(rows: &[ReduceRow]) {
        let mut bad = Vec::new();
        for (i, &(state, event, want_state, want_act)) in rows.iter().enumerate() {
            let got = reduce_visibility(state, event);
            if got != (want_state, want_act) {
                bad.push(format!(
                    "第 {i} 行 {state:?} + {event:?}：期望 {:?}，实际 {got:?}",
                    (want_state, want_act)
                ));
            }
        }
        assert!(bad.is_empty(), "{} 行不符：\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn 归约_前台激活置为想显示并请求激活显示() {
        let ev = Ev::Activate { foreground: true };
        let req = Act::RequestActivationDisplay;
        check_reduce_rows(&[
            (iv(false, false), ev, iv(true, false), req),
            (iv(false, true), ev, iv(true, false), req),
            (iv(true, false), ev, iv(true, false), req),
            (iv(true, true), ev, iv(true, false), req),
        ]);
    }

    #[test]
    fn 归约_后台激活记为自动收起并隐藏() {
        let ev = Ev::Activate { foreground: false };
        check_reduce_rows(&[
            (iv(false, false), ev, iv(true, true), Act::HideAndRelease),
            (iv(false, true), ev, iv(true, true), Act::HideAndRelease),
            (iv(true, false), ev, iv(true, true), Act::HideAndRelease),
            (iv(true, true), ev, iv(true, true), Act::HideAndRelease),
        ]);
    }

    #[test]
    fn 归约_停用清空状态并隐藏() {
        let ev = Ev::Deactivate;
        check_reduce_rows(&[
            (iv(false, false), ev, iv(false, false), Act::HideAndRelease),
            (iv(false, true), ev, iv(false, false), Act::HideAndRelease),
            (iv(true, false), ev, iv(false, false), Act::HideAndRelease),
            (iv(true, true), ev, iv(false, false), Act::HideAndRelease),
        ]);
    }

    #[test]
    fn 归约_活动实例在后台的看门狗收起自己() {
        let hidden = Ev::Watchdog {
            foreground: false,
            self_visible: false,
        };
        let shown = Ev::Watchdog {
            foreground: false,
            self_visible: true,
        };
        check_reduce_rows(&[
            (iv(true, false), hidden, iv(true, true), Act::HideAndRelease),
            (iv(true, false), shown, iv(true, true), Act::HideAndRelease),
            (iv(true, true), hidden, iv(true, true), Act::HideAndRelease),
            (iv(true, true), shown, iv(true, true), Act::HideAndRelease),
        ]);
    }

    #[test]
    fn 归约_活动实例在前台且自身隐藏时请求看门狗显示() {
        let ev = Ev::Watchdog {
            foreground: true,
            self_visible: false,
        };
        let req = Act::RequestWatchdogDisplay;
        check_reduce_rows(&[
            (iv(true, false), ev, iv(true, false), req),
            (iv(true, true), ev, iv(true, true), req),
        ]);
    }

    #[test]
    fn 归约_活动实例在前台且自身可见时什么都不做() {
        let ev = Ev::Watchdog {
            foreground: true,
            self_visible: true,
        };
        check_reduce_rows(&[
            (iv(true, false), ev, iv(true, false), Act::None),
            (iv(true, true), ev, iv(true, true), Act::None),
        ]);
    }

    #[test]
    fn 归约_非活动实例的任意看门狗什么都不做() {
        let fg_hidden = Ev::Watchdog {
            foreground: true,
            self_visible: false,
        };
        let fg_shown = Ev::Watchdog {
            foreground: true,
            self_visible: true,
        };
        let bg_hidden = Ev::Watchdog {
            foreground: false,
            self_visible: false,
        };
        let bg_shown = Ev::Watchdog {
            foreground: false,
            self_visible: true,
        };
        check_reduce_rows(&[
            (iv(false, false), fg_hidden, iv(false, false), Act::None),
            (iv(false, false), fg_shown, iv(false, false), Act::None),
            (iv(false, false), bg_hidden, iv(false, false), Act::None),
            (iv(false, false), bg_shown, iv(false, false), Act::None),
            (iv(false, true), fg_hidden, iv(false, true), Act::None),
            (iv(false, true), fg_shown, iv(false, true), Act::None),
            (iv(false, true), bg_hidden, iv(false, true), Act::None),
            (iv(false, true), bg_shown, iv(false, true), Act::None),
        ]);
    }

    #[test]
    fn 归约_显示成功清掉自动收起标记且不改想不想显示() {
        let ev = Ev::DisplayFinished { shown: true };
        check_reduce_rows(&[
            (iv(false, false), ev, iv(false, false), Act::None),
            (iv(false, true), ev, iv(false, false), Act::None),
            (iv(true, false), ev, iv(true, false), Act::None),
            (iv(true, true), ev, iv(true, false), Act::None),
        ]);
    }

    #[test]
    fn 归约_显示失败保留状态以便重试() {
        let ev = Ev::DisplayFinished { shown: false };
        check_reduce_rows(&[
            (iv(false, false), ev, iv(false, false), Act::None),
            (iv(false, true), ev, iv(false, true), Act::None),
            (iv(true, false), ev, iv(true, false), Act::None),
            (iv(true, true), ev, iv(true, true), Act::None),
        ]);
    }

    /// 手写脚本的一步：目标实例下标、事件、两个实例的期望状态 `[A, B]`、期望动作。
    type ScriptStep = (
        usize,
        VisibilityEvent,
        [InstanceVisibility; 2],
        VisibilityAction,
    );

    /// 两个实例 A、B 的手写事件脚本，每一步把两个实例的期望状态都写出来：
    /// 只有收到事件的那个会变；B 停用后到再激活前，看门狗（包括一次迟到的"显示成功"之后）
    /// 都不让它复活；A 连续两次激活状态不变；B 后台再激活后重新获得复活资格。
    ///
    /// **Validates: Requirements 1.5, 1.7, 2.4**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 1: Instance state isolation and deactivation closure
    #[test]
    fn 属性1_两实例手写脚本只改目标实例且停用后不复活() {
        const A: usize = 0;
        const B: usize = 1;
        let fg_hidden = Ev::Watchdog {
            foreground: true,
            self_visible: false,
        };
        let bg_hidden = Ev::Watchdog {
            foreground: false,
            self_visible: false,
        };
        let bg_shown = Ev::Watchdog {
            foreground: false,
            self_visible: true,
        };
        let fg_on = Ev::Activate { foreground: true };
        let bg_on = Ev::Activate { foreground: false };
        let done = Ev::DisplayFinished { shown: true };
        let failed = Ev::DisplayFinished { shown: false };
        let (ff, tf, tt) = (iv(false, false), iv(true, false), iv(true, true));

        // (目标, 事件, 期望 [A, B], 期望动作)
        let script: [ScriptStep; 19] = [
            (A, fg_on, [tf, ff], Act::RequestActivationDisplay),
            (A, done, [tf, ff], Act::None),
            (B, fg_on, [tf, tf], Act::RequestActivationDisplay),
            (B, done, [tf, tf], Act::None),
            // A 的窗口被 B 顶掉了：A 的看门狗想复活（会被仲裁否决），A 的状态不变
            (A, fg_hidden, [tf, tf], Act::RequestWatchdogDisplay),
            (A, failed, [tf, tf], Act::None),
            // 宿主切到后台：两个实例各自记为自动收起
            (A, bg_hidden, [tt, tf], Act::HideAndRelease),
            (B, bg_shown, [tt, tt], Act::HideAndRelease),
            // B 停用：只清 B
            (B, Ev::Deactivate, [tt, ff], Act::HideAndRelease),
            // 回到前台：停用的 B 不复活，仍活动的 A 请求复活
            (B, fg_hidden, [tt, ff], Act::None),
            (A, fg_hidden, [tt, ff], Act::RequestWatchdogDisplay),
            (A, done, [tf, ff], Act::None),
            (B, fg_hidden, [tf, ff], Act::None),
            // 迟到的"显示成功"也不能让停用的 B 重新想显示
            (B, done, [tf, ff], Act::None),
            (B, fg_hidden, [tf, ff], Act::None),
            // A 连续两次前台激活：状态不变
            (A, fg_on, [tf, ff], Act::RequestActivationDisplay),
            (A, fg_on, [tf, ff], Act::RequestActivationDisplay),
            // B 在后台再激活：重新获得复活资格
            (B, bg_on, [tf, tt], Act::HideAndRelease),
            (B, fg_hidden, [tf, tt], Act::RequestWatchdogDisplay),
        ];

        let mut states = [ff, ff];
        for (step, &(target, event, want_states, want_act)) in script.iter().enumerate() {
            let (next, act) = reduce_visibility(states[target], event);
            states[target] = next;
            assert_eq!(
                (states, act),
                (want_states, want_act),
                "第 {step} 步：给 {} 投递 {event:?}",
                ["A", "B"][target]
            );
        }
    }

    /// 实例状态的全部 4 种取值。
    const ALL_STATES: [InstanceVisibility; 4] = [
        iv(false, false),
        iv(false, true),
        iv(true, false),
        iv(true, true),
    ];

    /// 穷举用的全部事件：两种激活、停用、四种看门狗、两种显示结束。
    const ALL_EVENTS: [VisibilityEvent; 9] = [
        Ev::Activate { foreground: true },
        Ev::Activate { foreground: false },
        Ev::Deactivate,
        Ev::Watchdog {
            foreground: true,
            self_visible: false,
        },
        Ev::Watchdog {
            foreground: true,
            self_visible: true,
        },
        Ev::Watchdog {
            foreground: false,
            self_visible: false,
        },
        Ev::Watchdog {
            foreground: false,
            self_visible: true,
        },
        Ev::DisplayFinished { shown: true },
        Ev::DisplayFinished { shown: false },
    ];

    #[derive(Default)]
    struct ClosureStats {
        sequences: usize,
        /// 停用后、再激活前投递给该实例的看门狗次数。
        closed_ticks: usize,
        /// 活动实例的看门狗产生显示请求的次数。非空性：证明"停用后不复活"不是因为谁都不复活。
        live_requests: usize,
    }

    /// 深度优先走完所有 `depth` 步的两实例事件序列（每步选目标实例 × 事件）。
    /// `closed[i]` 表示实例 i 停用后（或从未激活）还没再激活过。
    ///
    /// 归约函数只接收目标实例自己的状态，所以"只改目标实例"由签名保证；这里每步只把结果
    /// 写回目标的那一格，另一格原样带到下一步，再检查停用闭包、激活幂等和规则表里的硬事实。
    fn walk_two_instances(
        states: [InstanceVisibility; 2],
        closed: [bool; 2],
        depth: usize,
        stats: &mut ClosureStats,
    ) {
        if depth == 0 {
            stats.sequences += 1;
            return;
        }
        for target in 0..2 {
            for &event in &ALL_EVENTS {
                let before = states[target];
                let (next, act) = reduce_visibility(before, event);
                let mut now_closed = closed;
                match event {
                    Ev::Activate { .. } => {
                        assert!(next.wanted, "{before:?} + {event:?} 之后 wanted 必须为真");
                        assert_eq!(
                            reduce_visibility(next, event),
                            (next, act),
                            "{before:?} 上连续两次 {event:?}：第二次状态和动作都不该变"
                        );
                        now_closed[target] = false;
                    }
                    Ev::Deactivate => {
                        assert_eq!(
                            (next, act),
                            (iv(false, false), Act::HideAndRelease),
                            "{before:?} + 停用"
                        );
                        now_closed[target] = true;
                    }
                    Ev::Watchdog { .. } => {
                        let requests = matches!(
                            act,
                            Act::RequestActivationDisplay | Act::RequestWatchdogDisplay
                        );
                        if closed[target] {
                            stats.closed_ticks += 1;
                            assert!(
                                !requests,
                                "停用后未再激活的实例 {before:?} 收到 {event:?} 却请求显示 {act:?}"
                            );
                            assert!(!next.wanted, "看门狗不能把停用的实例改回想显示");
                        } else if requests {
                            stats.live_requests += 1;
                        }
                    }
                    Ev::DisplayFinished { .. } => {
                        assert_eq!(
                            next.wanted, before.wanted,
                            "{before:?} + {event:?}：显示结束不改 wanted"
                        );
                    }
                }
                let mut after = states;
                after[target] = next;
                walk_two_instances(after, now_closed, depth - 1, stats);
            }
        }
    }

    /// 两实例、每步 2 个目标 × 9 种事件、长度 4 的全部序列，起点取两个实例的 16 种初始状态
    /// 组合（共 16 × 18⁴ 条）。
    ///
    /// **Validates: Requirements 1.5, 1.7, 2.4**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 1: Instance state isolation and deactivation closure
    #[test]
    fn 属性1_两实例穷举序列停用后不复活且激活幂等() {
        let mut stats = ClosureStats::default();
        for a in ALL_STATES {
            for b in ALL_STATES {
                walk_two_instances([a, b], [!a.wanted, !b.wanted], 4, &mut stats);
            }
        }
        assert_eq!(stats.sequences, 16 * 18usize.pow(4), "穷举条数");
        assert!(stats.closed_ticks > 0, "穷举里没有出现停用后的看门狗");
        assert!(
            stats.live_requests > 0,
            "穷举里活动实例的看门狗一次显示请求都没产生，停用闭包的检查是空转"
        );
    }

    /// 同一条性质走生产编排入口 `drive_visibility`（经 `SimHost::deliver`）：
    /// 投递给一个实例的触发只改它自己的 `InstanceVisibility`；B 停用后，前台/后台的看门狗
    /// 都不让 B 复活，而同一时刻仍活动的 A 能补位（非空性）。
    ///
    /// **Validates: Requirements 1.5, 2.4**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 1: Instance state isolation and deactivation closure
    #[test]
    fn 属性1_模拟宿主里事件只改目标实例且停用后不复活() {
        let host = SimHost::new(2);
        let (ff, tf) = (iv(false, false), iv(true, false));

        host.deliver(0, Trigger::Activate);
        assert_eq!([host.state(0), host.state(1)], [tf, ff], "A 激活，B 不变");

        host.deliver(1, Trigger::Activate);
        assert_eq!(
            [host.state(0), host.state(1)],
            [tf, tf],
            "B 前台激活；A 的窗口可能被收起，但 A 的实例状态不归 B 改"
        );

        host.deliver(1, Trigger::Deactivate);
        assert_eq!([host.state(0), host.state(1)], [tf, ff], "B 停用只清 B");
        assert!(!host.visible(1), "B 停用后隐藏");

        for fg in [true, false, true] {
            host.set_foreground(fg);
            assert_eq!(
                host.deliver(1, Trigger::Tick),
                DisplayOutcome::KeptHidden,
                "前台={fg}：停用的 B 不复活"
            );
            assert!(!host.visible(1), "前台={fg}：停用的 B 保持隐藏");
            assert_eq!([host.state(0), host.state(1)], [tf, ff], "前台={fg}");
        }

        assert_eq!(
            host.deliver(0, Trigger::Tick),
            DisplayOutcome::Shown,
            "仍活动的 A 在前台看门狗里补位"
        );
        assert!(host.visible(0), "A 补位后可见");
        assert_eq!([host.state(0), host.state(1)], [tf, ff], "A 补位不改 B");
        assert_eq!(host.deliver(1, Trigger::Tick), DisplayOutcome::KeptHidden);
        assert!(!host.visible(1), "A 可见时停用的 B 仍隐藏");
    }

    // -----------------------------------------------------------------------
    // 进程级"最多一个可见"（Property 2）
    //
    // 每一步之后检查：可见数 ≤ 1；没有实例想显示时可见数为 0；原子所有权不停在仲裁中，
    // 并且等于唯一可见的那条（没有可见的就是 `OWNER_NONE`）。
    // -----------------------------------------------------------------------

    /// 穷举序列里的一步：给某个实例投递触发，或者切换宿主前后台。
    /// 切前后台只改前台标志、不顺带投递看门狗（看门狗由序列显式选择）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Step {
        To(usize, Trigger),
        Foreground(bool),
    }

    /// `n` 个实例的全部单步动作：每个实例 × {激活, 停用, 看门狗}，再加切前台、切后台。
    fn all_steps(n: usize) -> Vec<Step> {
        let mut v = Vec::new();
        for i in 0..n {
            for t in [Trigger::Activate, Trigger::Deactivate, Trigger::Tick] {
                v.push(Step::To(i, t));
            }
        }
        v.push(Step::Foreground(true));
        v.push(Step::Foreground(false));
        v
    }

    fn apply_step(host: &SimHost, step: Step) {
        match step {
            Step::To(i, t) => {
                host.deliver(i, t);
            }
            Step::Foreground(fg) => host.set_foreground(fg),
        }
    }

    /// 进程级不变量。返回此刻的可见数。
    fn check_at_most_one(host: &SimHost, n: usize, seq: &[Step]) -> usize {
        let shown = host.visible_windows();
        assert!(
            shown.len() <= 1,
            "序列 {seq:?} 之后同时有 {} 条可见：{shown:?}",
            shown.len()
        );
        if !(0..n).any(|i| host.state(i).wanted) {
            assert!(
                shown.is_empty(),
                "序列 {seq:?} 之后没有实例想显示，却有 {shown:?} 可见"
            );
        }
        let owner = host.owner();
        assert_ne!(owner, OWNER_ARBITRATING, "序列 {seq:?} 之后原子停在仲裁中");
        assert_eq!(
            owner,
            shown.first().copied().unwrap_or(OWNER_NONE),
            "序列 {seq:?} 之后原子所有权应等于唯一可见的那条（没有可见的就是 0）"
        );
        shown.len()
    }

    struct Sweep {
        sequences: usize,
        /// 出现"恰好一条可见"的步数。非空性：证明不变量不是靠"谁都不显示"成立的。
        saw_one: usize,
    }

    /// 走完 `n` 个实例、长度 `len` 的全部序列；每条都从全新的模拟宿主起步。
    fn sweep(n: usize, len: usize) -> Sweep {
        let steps = all_steps(n);
        let total = steps.len().pow(len as u32);
        let mut stats = Sweep {
            sequences: 0,
            saw_one: 0,
        };
        let mut seq = Vec::with_capacity(len);
        for code in 0..total {
            seq.clear();
            let mut c = code;
            for _ in 0..len {
                seq.push(steps[c % steps.len()]);
                c /= steps.len();
            }
            let host = SimHost::new(n);
            for k in 0..len {
                apply_step(&host, seq[k]);
                if check_at_most_one(&host, n, &seq[..=k]) == 1 {
                    stats.saw_one += 1;
                }
            }
            stats.sequences += 1;
        }
        stats
    }

    /// 两实例 × {激活, 停用, 看门狗} + {切前台, 切后台} = 8 种动作，长度 4 共 8⁴ 条。
    ///
    /// **Validates: Requirements 1.6, 2.1, 6.2**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 2: Process-wide at-most-one visibility invariant
    #[test]
    fn 属性2_两实例穷举序列任意时刻最多一个可见() {
        let s = sweep(2, 4);
        assert_eq!(s.sequences, 4096, "穷举条数");
        assert!(
            s.saw_one > 0,
            "4096 条序列里一次可见都没出现，最多一个可见的检查是空转"
        );
    }

    /// 三实例 11 种动作，长度 3 共 11³ 条。
    ///
    /// **Validates: Requirements 1.6, 2.1, 6.2**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 2: Process-wide at-most-one visibility invariant
    #[test]
    fn 属性2_三实例穷举序列任意时刻最多一个可见() {
        let s = sweep(3, 3);
        assert_eq!(s.sequences, 1331, "穷举条数");
        assert!(
            s.saw_one > 0,
            "1331 条序列里一次可见都没出现，最多一个可见的检查是空转"
        );
    }

    /// A 握着仲裁哨兵（正在重绘）时，B 的激活不等待、直接 `Busy`，期间和之后 B 都隐藏；
    /// A 完成后发布 A。B 之后的看门狗看到 A 可见，被否决。
    ///
    /// **Validates: Requirements 2.1, 2.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 2: Process-wide at-most-one visibility invariant
    #[test]
    fn 属性2_仲裁进行中另一实例的激活返回忙() {
        let host = SimHost::new(2);
        let a = host.hwnd(0);
        let probe = Rc::new(Cell::new((OWNER_NONE, true)));
        let p = Rc::clone(&probe);
        host.on_next_redraw(move |h| {
            let out = h.deliver(1, Trigger::Activate);
            h.core
                .world
                .borrow_mut()
                .nested
                .push((1, Trigger::Activate, out));
            p.set((h.owner(), h.visible(1)));
        });
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(
            host.nested_outcomes(),
            vec![(1, Trigger::Activate, DisplayOutcome::Busy)],
            "A 仲裁期间 B 的激活应直接返回忙"
        );
        assert_eq!(
            probe.get(),
            (OWNER_ARBITRATING, false),
            "B 被拒时原子仍是仲裁中，B 隐藏"
        );
        assert_eq!(host.visible_windows(), vec![a]);
        assert_eq!(host.owner(), a, "A 完成后发布 A");
        assert_eq!(host.state(1), iv(true, false), "B 仍想显示（等下次重试）");

        assert_eq!(host.deliver(1, Trigger::Tick), DisplayOutcome::KeptHidden);
        assert_eq!(host.visible_windows(), vec![a], "A 可见时 B 的复活被否决");
    }

    /// 两个都想显示、都被后台收起的实例回到前台后在同一个看门狗周期里复活：
    /// A 正在仲裁时 B 的复活返回 `Busy`，最后只有 A 可见。
    ///
    /// **Validates: Requirements 2.1, 2.3, 2.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 2: Process-wide at-most-one visibility invariant
    #[test]
    fn 属性2_两个隐藏实例同时复活只有一个胜出() {
        let host = SimHost::new(2);
        let a = host.hwnd(0);
        host.set_foreground(false);
        host.deliver(0, Trigger::Activate);
        host.deliver(1, Trigger::Activate);
        assert_eq!(host.visible_count(), 0, "后台激活不显示");
        host.set_foreground(true);

        host.nest_on_next_redraw(1, Trigger::Tick);
        assert_eq!(host.deliver(0, Trigger::Tick), DisplayOutcome::Shown);
        assert_eq!(
            host.nested_outcomes(),
            vec![(1, Trigger::Tick, DisplayOutcome::Busy)],
            "A 仲裁期间 B 的复活应直接返回忙"
        );
        assert_eq!(host.visible_windows(), vec![a]);
        assert_eq!(host.owner(), a);

        assert_eq!(host.deliver(1, Trigger::Tick), DisplayOutcome::KeptHidden);
        assert_eq!(host.visible_windows(), vec![a], "下一个周期 B 仍被否决");
    }

    /// A 可见、B 想显示但隐藏；A 重复激活取得仲裁权、重绘时 B 停用：
    /// B 的停用只管自己，不改原子（仍是仲裁中），也不收起 A；A 完成后发布 A。
    ///
    /// **Validates: Requirements 1.5, 2.4, 2.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 2: Process-wide at-most-one visibility invariant
    #[test]
    fn 属性2_仲裁进行中另一实例停用只隐藏自己不改原子() {
        let host = SimHost::new(2);
        let a = host.hwnd(0);
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        host.set_foreground(false);
        host.deliver(1, Trigger::Activate);
        host.set_foreground(true);
        assert_eq!(
            host.state(1),
            iv(true, true),
            "B 后台激活：想显示、被自动收起"
        );
        assert_eq!(host.owner(), a);

        let probe = Rc::new(Cell::new((OWNER_NONE, false, true)));
        let p = Rc::clone(&probe);
        host.clear_log();
        host.on_next_redraw(move |h| {
            h.deliver(1, Trigger::Deactivate);
            p.set((h.owner(), h.visible(0), h.visible(1)));
        });
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(
            probe.get(),
            (OWNER_ARBITRATING, true, false),
            "B 停用那一刻：原子仍是仲裁中、A 没被收起、B 隐藏"
        );
        assert!(
            !host.log().contains(&SimOp::Hide(a)),
            "B 的停用不能收起 A：{:?}",
            host.log()
        );
        assert_eq!(host.state(1), iv(false, false), "B 停用");
        assert_eq!(host.visible_windows(), vec![a]);
        assert_eq!(host.owner(), a, "A 完成后发布 A");
    }

    /// A 可见时 B 前台激活：日志里"收起 A"必须早于"显示 B"，最后只有 B 可见、原子是 B。
    ///
    /// **Validates: Requirements 1.2, 2.5**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 2: Process-wide at-most-one visibility invariant
    #[test]
    fn 属性2_所有权转移先收起旧的再显示新的() {
        let host = SimHost::new(2);
        let (a, b) = (host.hwnd(0), host.hwnd(1));
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        host.clear_log();
        assert_eq!(host.deliver(1, Trigger::Activate), DisplayOutcome::Shown);
        let log = host.log();
        let hide_a = log.iter().position(|o| *o == SimOp::Hide(a));
        let show_b = log.iter().position(|o| *o == SimOp::Show(b, true));
        match (hide_a, show_b) {
            (Some(h), Some(s)) => assert!(h < s, "先显示了 B 才收起 A：{log:?}"),
            _ => panic!("日志里应有收起 A 和显示 B：{log:?}"),
        }
        assert_eq!(host.visible_windows(), vec![b]);
        assert_eq!(host.owner(), b);
    }

    /// 已经有一条可见时，隐藏实例的看门狗复活被否决：不重绘、不显示，状态保留复活资格。
    ///
    /// **Validates: Requirements 2.2**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 2: Process-wide at-most-one visibility invariant
    #[test]
    fn 属性2_已有可见浮条时隐藏实例的复活被否决() {
        let host = SimHost::new(2);
        let (a, b) = (host.hwnd(0), host.hwnd(1));
        host.set_foreground(false);
        host.deliver(1, Trigger::Activate);
        host.set_foreground(true);
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        host.clear_log();

        assert_eq!(host.deliver(1, Trigger::Tick), DisplayOutcome::KeptHidden);
        assert_eq!(host.visible_windows(), vec![a]);
        assert_eq!(host.owner(), a);
        assert!(
            !host
                .log()
                .iter()
                .any(|o| matches!(o, SimOp::Show(h, _) | SimOp::Redraw(h, _) if *h == b)),
            "被否决的 B 不该重绘或显示：{:?}",
            host.log()
        );
        assert_eq!(host.state(1), iv(true, true), "B 仍有复活资格");
    }

    // -----------------------------------------------------------------------
    // 生命周期收敛（Property 3）
    // -----------------------------------------------------------------------

    struct ConvergeSweep {
        sequences: usize,
        /// 收敛阶段开始时至少有一个实例想显示的序列数。非空性：证明"前台收敛到一个"真被检查过。
        with_wanted: usize,
    }

    /// 在任意序列之后追加收敛阶段：切到前台、每个实例各看门狗一次 → 有实例想显示时恰好一个可见，
    /// 否则零个；再切到后台、各看门狗一次 → 零个。收敛阶段的每一步也检查进程级不变量。
    fn converge(host: &SimHost, n: usize, seq: &[Step]) -> bool {
        let any_wanted = (0..n).any(|i| host.state(i).wanted);
        host.set_foreground(true);
        for i in 0..n {
            host.deliver(i, Trigger::Tick);
            check_at_most_one(host, n, seq);
        }
        assert_eq!(
            host.visible_count(),
            usize::from(any_wanted),
            "序列 {seq:?} 之后回到前台、各看门狗一次：有实例想显示={any_wanted}"
        );
        host.set_foreground(false);
        for i in 0..n {
            host.deliver(i, Trigger::Tick);
            check_at_most_one(host, n, seq);
        }
        assert_eq!(
            host.visible_count(),
            0,
            "序列 {seq:?} 之后切到后台、各看门狗一次，应全部收起"
        );
        any_wanted
    }

    fn sweep_converge(n: usize, len: usize) -> ConvergeSweep {
        let steps = all_steps(n);
        let total = steps.len().pow(len as u32);
        let mut stats = ConvergeSweep {
            sequences: 0,
            with_wanted: 0,
        };
        let mut seq = Vec::with_capacity(len);
        for code in 0..total {
            seq.clear();
            let mut c = code;
            for _ in 0..len {
                seq.push(steps[c % steps.len()]);
                c /= steps.len();
            }
            let host = SimHost::new(n);
            for k in 0..len {
                apply_step(&host, seq[k]);
                check_at_most_one(&host, n, &seq[..=k]);
            }
            if converge(&host, n, &seq) {
                stats.with_wanted += 1;
            }
            stats.sequences += 1;
        }
        stats
    }

    /// 两个活动实例都被后台看门狗收起，回到前台后分别按 A→B、B→A 顺序看门狗：
    /// 都恰好一个可见，而且是先到的那个；再来一轮看门狗也不变。
    ///
    /// **Validates: Requirements 1.4, 2.3, 6.1**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 3: Successful lifecycle convergence
    #[test]
    fn 属性3_两实例被后台收起后按两种顺序复活都恰好一个() {
        for order in [[0usize, 1], [1, 0]] {
            let host = SimHost::new(2);
            assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
            assert_eq!(host.deliver(1, Trigger::Activate), DisplayOutcome::Shown);
            host.set_foreground(false);
            host.deliver(0, Trigger::Tick);
            host.deliver(1, Trigger::Tick);
            assert_eq!(
                host.visible_count(),
                0,
                "顺序 {order:?}：后台看门狗全部收起"
            );
            assert_eq!(
                [host.state(0), host.state(1)],
                [iv(true, true), iv(true, true)],
                "顺序 {order:?}：两个都记为自动收起"
            );
            assert_eq!(host.owner(), OWNER_NONE, "顺序 {order:?}：收起后释放所有权");

            host.set_foreground(true);
            for &i in &order {
                host.deliver(i, Trigger::Tick);
            }
            let first = host.hwnd(order[0]);
            assert_eq!(
                host.visible_windows(),
                vec![first],
                "顺序 {order:?}：只有先到的那个复活"
            );
            assert_eq!(host.owner(), first);

            for &i in &order {
                host.deliver(i, Trigger::Tick);
            }
            assert_eq!(
                host.visible_windows(),
                vec![first],
                "顺序 {order:?}：再一轮看门狗不变"
            );
        }
    }

    /// 任务 1.4 的两实例 4096 条序列，每条后面接收敛阶段。
    ///
    /// **Validates: Requirements 1.1, 1.3, 1.4, 1.6, 6.1**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 3: Successful lifecycle convergence
    #[test]
    fn 属性3_两实例穷举序列收敛到前台一个后台零个() {
        let s = sweep_converge(2, 4);
        assert_eq!(s.sequences, 4096, "穷举条数");
        assert!(
            s.with_wanted > 0,
            "没有一条序列在收敛前有实例想显示，前台收敛到一个的检查是空转"
        );
    }

    /// 任务 1.4 的三实例 1331 条序列，每条后面接收敛阶段。
    ///
    /// **Validates: Requirements 1.1, 1.3, 1.4, 1.6, 6.1**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 3: Successful lifecycle convergence
    #[test]
    fn 属性3_三实例穷举序列收敛到前台一个后台零个() {
        let s = sweep_converge(3, 3);
        assert_eq!(s.sequences, 1331, "穷举条数");
        assert!(
            s.with_wanted > 0,
            "没有一条序列在收敛前有实例想显示，前台收敛到一个的检查是空转"
        );
    }

    /// 可见的所有者 B 停用后零个可见、所有权释放；仍活动的 A 在下一次前台看门狗补位。
    ///
    /// **Validates: Requirements 1.4, 1.5**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 3: Successful lifecycle convergence
    #[test]
    fn 属性3_所有者停用后另一个活动实例补位() {
        let host = SimHost::new(2);
        let (a, b) = (host.hwnd(0), host.hwnd(1));
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(host.deliver(1, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(host.visible_windows(), vec![b]);

        host.deliver(1, Trigger::Deactivate);
        assert_eq!(host.visible_count(), 0, "所有者停用后零个可见");
        assert_eq!(host.owner(), OWNER_NONE, "所有者停用后释放所有权");

        assert_eq!(host.deliver(0, Trigger::Tick), DisplayOutcome::Shown);
        assert_eq!(host.visible_windows(), vec![a], "A 补位");
        assert_eq!(host.owner(), a);
    }

    /// 同一实例连续激活、不停用：始终恰好一个可见；两个实例交替重复激活也一样。
    ///
    /// **Validates: Requirements 1.1, 1.7**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 3: Successful lifecycle convergence
    #[test]
    fn 属性3_重复激活不停用始终一个() {
        let host = SimHost::new(2);
        let (a, b) = (host.hwnd(0), host.hwnd(1));
        for k in 0..3 {
            assert_eq!(
                host.deliver(0, Trigger::Activate),
                DisplayOutcome::Shown,
                "第 {k} 次激活 A"
            );
            assert_eq!(host.visible_windows(), vec![a], "第 {k} 次激活 A");
            assert_eq!(host.owner(), a);
        }
        for k in 0..3 {
            assert_eq!(host.deliver(1, Trigger::Activate), DisplayOutcome::Shown);
            assert_eq!(host.visible_windows(), vec![b], "第 {k} 轮激活 B");
            assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
            assert_eq!(host.visible_windows(), vec![a], "第 {k} 轮激活 A");
        }
        assert_eq!(host.owner(), a);
    }

    // -----------------------------------------------------------------------
    // 保守失败与可重试（Property 4）
    //
    // 每种失败之后：请求者隐藏、没被发布为所有者、原子不停在仲裁中；故障开关用一次就复位，
    // 紧跟一次有效请求必须显示成功（不用重启宿主）。
    // -----------------------------------------------------------------------

    /// 失败之后的公共检查：请求者 `i` 隐藏，原子不是它、也不是仲裁中，并且等于 `owner`。
    fn assert_failed_conservatively(host: &SimHost, i: usize, owner: isize, what: &str) {
        assert!(!host.visible(i), "{what}：请求者应保持隐藏");
        assert_ne!(
            host.owner(),
            host.hwnd(i),
            "{what}：请求者不该被发布为所有者"
        );
        assert_ne!(
            host.owner(),
            OWNER_ARBITRATING,
            "{what}：原子不能停在仲裁中"
        );
        assert_eq!(host.owner(), owner, "{what}：原子所有权");
        assert!(host.visible_count() <= 1, "{what}：最多一个可见");
    }

    /// 没有窗口的线程（`ensure_window` 失败）：返回 `NoWindow`，原子不变，实例仍记着想显示；
    /// 别的实例照常能显示。
    ///
    /// **Validates: Requirements 3.1, 3.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 4: Conservative failure and retryability
    #[test]
    fn 属性4_没有窗口时返回无窗口且不动原子() {
        let host = SimHost::new(1);
        let a = host.hwnd(0);
        let z = host.add_instance(0);
        assert_eq!(host.deliver(z, Trigger::Activate), DisplayOutcome::NoWindow);
        assert_eq!(host.owner(), OWNER_NONE, "没窗口时原子不变");
        assert!(host.state(z).wanted, "没窗口也记着想显示，下次激活可以重试");
        assert_eq!(host.deliver(z, Trigger::Tick), DisplayOutcome::NoWindow);

        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(host.deliver(z, Trigger::Activate), DisplayOutcome::NoWindow);
        assert_eq!(host.owner(), a, "没窗口的请求不动已有的所有权");
        assert_eq!(host.visible_windows(), vec![a]);
    }

    /// 别的请求正在仲裁（原子预置为哨兵）：激活和复活都返回 `Busy`，不改原子；
    /// 仲裁者结束（原子复位）后下一次看门狗显示成功。
    ///
    /// **Validates: Requirements 3.3, 3.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 4: Conservative failure and retryability
    #[test]
    fn 属性4_仲裁忙时不等待不改原子且之后可重试() {
        let host = SimHost::new(1);
        let a = host.hwnd(0);
        host.set_owner(OWNER_ARBITRATING);
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Busy);
        assert!(!host.visible(0), "忙时不显示");
        assert_eq!(host.owner(), OWNER_ARBITRATING, "失败者不改原子");
        assert_eq!(host.deliver(0, Trigger::Tick), DisplayOutcome::Busy);
        assert_eq!(host.owner(), OWNER_ARBITRATING, "失败者不改原子");
        assert!(host.state(0).wanted, "忙时保留想显示");

        host.set_owner(OWNER_NONE); // 模拟别的仲裁者结束
        assert_eq!(host.deliver(0, Trigger::Tick), DisplayOutcome::Shown);
        assert_eq!(host.visible_windows(), vec![a]);
        assert_eq!(host.owner(), a);
    }

    /// 激活时枚举不确定：不显示；下一次激活成功。
    ///
    /// **Validates: Requirements 3.2, 3.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 4: Conservative failure and retryability
    #[test]
    fn 属性4_激活时枚举不确定就不显示() {
        let host = SimHost::new(1);
        host.arm(SimFault::QueryUnknown);
        assert_eq!(
            host.deliver(0, Trigger::Activate),
            DisplayOutcome::KeptHidden
        );
        assert_failed_conservatively(&host, 0, OWNER_NONE, "激活时枚举不确定");
        assert!(
            !host.log().iter().any(|o| matches!(o, SimOp::Show(..))),
            "枚举不确定时不该调用显示：{:?}",
            host.log()
        );
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(host.owner(), host.hwnd(0));
    }

    /// 看门狗复活时枚举不确定：不显示；下一次看门狗成功。
    ///
    /// **Validates: Requirements 3.2, 3.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 4: Conservative failure and retryability
    #[test]
    fn 属性4_复活时枚举不确定就不显示() {
        let host = SimHost::new(1);
        host.set_foreground(false);
        host.deliver(0, Trigger::Activate);
        host.set_foreground(true);
        host.arm(SimFault::QueryUnknown);
        assert_eq!(host.deliver(0, Trigger::Tick), DisplayOutcome::KeptHidden);
        assert_failed_conservatively(&host, 0, OWNER_NONE, "复活时枚举不确定");
        assert_eq!(host.deliver(0, Trigger::Tick), DisplayOutcome::Shown);
        assert_eq!(host.owner(), host.hwnd(0));
    }

    /// A 可见时 B 激活，但收不起 A：不显示 B，A 保留所有权；下一次 B 激活成功接管。
    ///
    /// **Validates: Requirements 3.3, 3.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 4: Conservative failure and retryability
    #[test]
    fn 属性4_收不起别的浮条时不显示自己() {
        let host = SimHost::new(2);
        let (a, b) = (host.hwnd(0), host.hwnd(1));
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        host.arm(SimFault::CleanupLeavesVisible);
        assert_eq!(
            host.deliver(1, Trigger::Activate),
            DisplayOutcome::KeptHidden
        );
        assert_failed_conservatively(&host, 1, a, "收不起 A");
        assert_eq!(host.visible_windows(), vec![a], "仍可见的 A 保留");

        assert_eq!(host.deliver(1, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(host.visible_windows(), vec![b]);
        assert_eq!(host.owner(), b);
    }

    /// 绘制失败：不调用显示；下一次激活成功。接管途中绘制失败时旧的已被收起，
    /// 结果是零个可见、所有权释放，旧实例下一次看门狗补位。
    ///
    /// **Validates: Requirements 3.4, 3.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 4: Conservative failure and retryability
    #[test]
    fn 属性4_绘制失败就不显示() {
        let host = SimHost::new(2);
        let (a, b) = (host.hwnd(0), host.hwnd(1));
        host.arm(SimFault::DrawFails);
        assert_eq!(
            host.deliver(0, Trigger::Activate),
            DisplayOutcome::KeptHidden
        );
        assert_failed_conservatively(&host, 0, OWNER_NONE, "绘制失败");
        assert!(
            !host.log().iter().any(|o| matches!(o, SimOp::Show(..))),
            "绘制失败时不该调用显示：{:?}",
            host.log()
        );
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(host.owner(), a);

        host.arm(SimFault::DrawFails);
        assert_eq!(
            host.deliver(1, Trigger::Activate),
            DisplayOutcome::KeptHidden
        );
        assert_failed_conservatively(&host, 1, OWNER_NONE, "接管途中绘制失败");
        assert_eq!(host.visible_count(), 0, "旧的已被收起");
        assert_eq!(host.deliver(0, Trigger::Tick), DisplayOutcome::Shown);
        assert_eq!(host.visible_windows(), vec![a], "旧实例补位");
        let _ = b;
    }

    /// 显示之后窗口仍不可见：不发布；下一次激活成功。
    ///
    /// **Validates: Requirements 3.3, 3.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 4: Conservative failure and retryability
    #[test]
    fn 属性4_显示后仍不可见就不发布() {
        let host = SimHost::new(1);
        host.arm(SimFault::ShowFails);
        assert_eq!(
            host.deliver(0, Trigger::Activate),
            DisplayOutcome::KeptHidden
        );
        assert_failed_conservatively(&host, 0, OWNER_NONE, "显示后仍不可见");
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(host.owner(), host.hwnd(0));
    }

    /// 刚显示宿主就离开前台：立刻收起自己、释放所有权；回到前台后下一次激活成功。
    ///
    /// **Validates: Requirements 1.3, 3.3, 3.6**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 4: Conservative failure and retryability
    #[test]
    fn 属性4_显示后宿主离开前台就收起() {
        let host = SimHost::new(1);
        host.arm(SimFault::LoseForegroundAfterShow);
        assert_eq!(
            host.deliver(0, Trigger::Activate),
            DisplayOutcome::KeptHidden
        );
        assert!(!host.foreground(), "故障让宿主离开了前台");
        assert_failed_conservatively(&host, 0, OWNER_NONE, "显示后离开前台");

        host.set_foreground(true);
        assert_eq!(host.deliver(0, Trigger::Activate), DisplayOutcome::Shown);
        assert_eq!(host.owner(), host.hwnd(0));
    }

    // -----------------------------------------------------------------------
    // 窗口枚举上下文（Property 5）
    // -----------------------------------------------------------------------

    /// 喂给 `EnumContext::observe` 的一个窗口：(句柄, PID, 类名判断, 是否可见)。
    type Seen = (isize, u32, Option<bool>, bool);

    /// 按 `windows` 顺序喂一个上下文，模拟回调：`HideThis` 时按 `hide_ok` 决定收起后是否仍可见。
    /// 返回模拟的 `EnumWindows` 返回值（回调主动停下时为 false）。
    fn feed(ctx: &mut EnumContext, windows: &[Seen], hide_ok: bool) -> bool {
        for &(h, pid, is_bar, visible) in windows {
            match ctx.observe(h, pid, is_bar, visible) {
                EnumStep::Continue => {}
                EnumStep::Stop => return false,
                EnumStep::HideThis => ctx.hidden(h, !hide_ok),
            }
        }
        true
    }

    /// 两个上下文交错观察不同的窗口序列，各自的结果只由自己的序列决定（期望值按序列手写）；
    /// 两个都看到"没有别的可见浮条"时，对同一个原子各比较交换一次，只有一个拿到哨兵。
    ///
    /// **Validates: Requirements 2.6, 3.2**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 5: Visibility query isolation
    #[test]
    fn 属性5_两个枚举上下文互不影响且只有一个拿到哨兵() {
        const PID: u32 = 7;
        const OTHER_PID: u32 = 9;
        // A 的序列：自己、别的进程的可见浮条、本进程别的类名、本进程隐藏的浮条、本进程可见的 205
        let seq_a: [Seen; 5] = [
            (101, PID, Some(true), true),
            (300, OTHER_PID, Some(true), true),
            (201, PID, Some(false), true),
            (202, PID, Some(true), false),
            (205, PID, Some(true), true),
        ];
        // B 的序列：全是看不到的（别的进程、隐藏、别的类名）
        let seq_b: [Seen; 3] = [
            (300, OTHER_PID, Some(true), true),
            (202, PID, Some(true), false),
            (201, PID, Some(false), true),
        ];

        let mut a = EnumContext::new(PID, 101, EnumMode::Find);
        let mut b = EnumContext::new(PID, 102, EnumMode::Find);
        // 交错喂：A 一个、B 一个……
        let mut a_ok = true;
        let mut b_ok = true;
        for k in 0..seq_a.len().max(seq_b.len()) {
            if a_ok
                && !a.stopped
                && let Some(w) = seq_a.get(k)
            {
                a_ok = feed(&mut a, std::slice::from_ref(w), true);
            }
            if b_ok
                && !b.stopped
                && let Some(w) = seq_b.get(k)
            {
                b_ok = feed(&mut b, std::slice::from_ref(w), true);
            }
        }
        assert_eq!(a.query_result(a_ok), VisibilityQuery::Visible(205));
        assert_eq!(b.query_result(b_ok), VisibilityQuery::NoneVisible);

        // 类名取不到 → 不确定；之后找到可见的照样算找到
        let mut c = EnumContext::new(PID, 101, EnumMode::Find);
        let ok = feed(&mut c, &[(201, PID, None, true)], true);
        assert_eq!(c.query_result(ok), VisibilityQuery::Unknown);
        let ok = feed(&mut c, &[(205, PID, Some(true), true)], true);
        assert_eq!(c.query_result(ok), VisibilityQuery::Visible(205));
        // `EnumWindows` 本身失败（不是回调主动停下）→ 不确定
        let d = EnumContext::new(PID, 101, EnumMode::Find);
        assert_eq!(d.query_result(false), VisibilityQuery::Unknown);

        // 清理模式：收起成功 → AllHidden；收不起 → StillVisible；类名不确定 → Unknown
        let mut e = EnumContext::new(PID, 101, EnumMode::Hide);
        let ok = feed(&mut e, &seq_a, true);
        assert_eq!(e.cleanup_result(ok), CleanupResult::AllHidden);
        let mut f = EnumContext::new(PID, 101, EnumMode::Hide);
        let ok = feed(&mut f, &seq_a, false);
        assert_eq!(f.cleanup_result(ok), CleanupResult::StillVisible(205));
        let mut g = EnumContext::new(PID, 101, EnumMode::Hide);
        let ok = feed(&mut g, &[(201, PID, None, true)], true);
        assert_eq!(g.cleanup_result(ok), CleanupResult::Unknown);

        // 两个查询都说"没有"：同一个原子各比较交换一次，只有一个拿到哨兵
        let mut b2 = EnumContext::new(PID, 101, EnumMode::Find);
        let ok2 = feed(&mut b2, &seq_b, true);
        assert_eq!(b2.query_result(ok2), VisibilityQuery::NoneVisible);
        let owner = AtomicIsize::new(OWNER_NONE);
        let take = || {
            owner
                .compare_exchange(
                    OWNER_NONE,
                    OWNER_ARBITRATING,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
        };
        let winners = [take(), take()].iter().filter(|&&w| w).count();
        assert_eq!(winners, 1, "两个都看到空，也只能有一个拿到哨兵");
    }

    // -----------------------------------------------------------------------
    // 第三格标点：句号左移、逗号右移各 1 个设计像素（× DPI），其余几何不变
    // -----------------------------------------------------------------------

    /// 96 DPI 下第三格的真实矩形（`item_rects()[2]`），格子中心 106.0、纵向中点 18.0。
    const PUNCT_CELL: RECT = RECT {
        left: 89,
        top: 4,
        right: 123,
        bottom: 32,
    };

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// 取出 `punct_glyphs` 的结果：(上面图形是不是空心环, 上 cx, 上 cy, 上 r, 上 hw（点为 0）,
    /// 逗号 cx, 逗号 cy, 逗号 k)。
    fn unpack_punct(g: [Glyph; 2]) -> (bool, f32, f32, f32, f32, f32, f32, f32) {
        let (ring, tcx, tcy, tr, thw) = match g[0] {
            Glyph::Ring { cx, cy, r, hw } => (true, cx, cy, r, hw),
            Glyph::Dot { cx, cy, r } => (false, cx, cy, r, 0.0),
            Glyph::Comma { .. } => panic!("上面不该是逗号"),
        };
        let Glyph::Comma { cx, cy, k } = g[1] else {
            panic!("下面应该是逗号");
        };
        (ring, tcx, tcy, tr, thw, cx, cy, k)
    }

    /// 期望值来自旧公式手算（`upper = mid - 4.6k`、`lower = mid + 2.6k`、环 `2.4k / 0.8k`、
    /// 点 `1.8k`），只有横坐标换成 `center ∓ k`。
    #[test]
    fn 标点_句号左移逗号右移且其余几何不变() {
        // (k, 句号/点 cx, 逗号 cx, 上 cy, 下 cy, 环 r, 环 hw, 点 r)
        type Row = (f32, f32, f32, f32, f32, f32, f32, f32);
        let table: [Row; 3] = [
            (1.0, 105.0, 107.0, 13.4, 20.6, 2.4, 0.8, 1.8),
            (1.5, 104.5, 107.5, 11.1, 21.9, 3.6, 1.2, 2.7),
            (2.0, 104.0, 108.0, 8.8, 23.2, 4.8, 1.6, 3.6),
        ];
        for (k, pcx, ccx, ucy, lcy, ring_r, ring_hw, dot_r) in table {
            for chinese in [true, false] {
                let what = format!("k={k} 中文标点={chinese}");
                let (ring, tcx, tcy, tr, thw, cx, cy, ck) =
                    unpack_punct(punct_glyphs(&PUNCT_CELL, chinese, k));
                assert_eq!(ring, chinese, "{what}：中文标点是空心环、半角是实心点");
                assert!(close(tcx, pcx), "{what}：句号 cx {tcx}，期望 {pcx}");
                assert!(close(cx, ccx), "{what}：逗号 cx {cx}，期望 {ccx}");
                assert!(close(cx - tcx, 2.0 * k), "{what}：两者横向中心距应为 2k");
                assert!(close(tcy, ucy), "{what}：句号 cy {tcy}，期望 {ucy}");
                assert!(close(cy, lcy), "{what}：逗号 cy {cy}，期望 {lcy}");
                assert!(close(ck, k), "{what}：逗号比例 {ck}，期望 {k}");
                if chinese {
                    assert!(close(tr, ring_r), "{what}：环半径 {tr}，期望 {ring_r}");
                    assert!(
                        close(thw, ring_hw),
                        "{what}：环半线宽 {thw}，期望 {ring_hw}"
                    );
                } else {
                    assert!(close(tr, dot_r), "{what}：点半径 {tr}，期望 {dot_r}");
                }
            }
        }
    }

    /// 点击区不随这次改动变化：第三格矩形按布局常量逐项累加；中心和四个内缩 1px 的角
    /// 都命中第三格，左右各外移一格像素就不是。
    #[test]
    fn 标点格点击区不变() {
        let cell = item_rects()[2];
        let left = scale(PAD)
            + scale(W_BRAND)
            + scale(GAP)
            + 2 * scale(SEP_GAP)
            + scale(SEP_W)
            + scale(W_MODE)
            + scale(GAP);
        assert_eq!(
            (cell.left, cell.top, cell.right, cell.bottom),
            (
                left,
                scale(PAD),
                left + scale(W_PUNCT),
                scale(PAD) + scale(ITEM_H)
            ),
            "第三格矩形"
        );
        if scale(96) == 96 {
            assert_eq!(
                (cell.left, cell.top, cell.right, cell.bottom),
                (
                    PUNCT_CELL.left,
                    PUNCT_CELL.top,
                    PUNCT_CELL.right,
                    PUNCT_CELL.bottom
                ),
                "96 DPI 下就是测试里写死的那个矩形"
            );
        }
        let (cx, cy) = ((cell.left + cell.right) / 2, (cell.top + cell.bottom) / 2);
        for (x, y) in [
            (cx, cy),
            (cell.left + 1, cell.top + 1),
            (cell.right - 2, cell.top + 1),
            (cell.left + 1, cell.bottom - 2),
            (cell.right - 2, cell.bottom - 2),
        ] {
            assert_eq!(hit_test(x, y), 2, "({x},{y}) 应命中第三格");
        }
        assert_ne!(hit_test(cell.left - 1, cy), 2, "左边外移一像素不是第三格");
        assert_ne!(hit_test(cell.right, cy), 2, "右边外移一像素不是第三格");
    }

    /// 扫描用的格子：96 DPI 真实格子、贴着原点的、更宽的、带负坐标的。
    const SCAN_CELLS: [RECT; 4] = [
        PUNCT_CELL,
        RECT {
            left: 0,
            top: 0,
            right: 34,
            bottom: 28,
        },
        RECT {
            left: 133,
            top: 6,
            right: 184,
            bottom: 48,
        },
        RECT {
            left: -10,
            top: -3,
            right: 31,
            bottom: 40,
        },
    ];

    /// 120 个正比例 `k = 0.5, 0.525, …, 3.475` × 4 个格子 × 两种标点状态。
    fn for_each_scan(mut f: impl FnMut(f32, RECT, bool, [Glyph; 2])) -> usize {
        let mut n = 0;
        for i in 0..120 {
            let k = 0.5 + i as f32 * 0.025;
            for cell in SCAN_CELLS {
                for chinese in [true, false] {
                    f(k, cell, chinese, punct_glyphs(&cell, chinese, k));
                    n += 1;
                }
            }
        }
        n
    }

    /// **Validates: Requirements 4.1, 4.2, 4.3, 4.4, 4.5, 4.6, 6.3, 6.4**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 6: DPI-scaled punctuation horizontal geometry
    #[test]
    fn 属性6_任意正比例下句号在中心左k逗号在中心右k() {
        let n = for_each_scan(|k, cell, chinese, g| {
            let center = (cell.left + cell.right) as f32 / 2.0;
            let (_, tcx, _, _, _, cx, _, _) = unpack_punct(g);
            let what = format!(
                "k={k} 格子={:?} 中文标点={chinese}",
                (cell.left, cell.right)
            );
            assert!(close(tcx, center - k), "{what}：句号 cx {tcx}");
            assert!(close(cx, center + k), "{what}：逗号 cx {cx}");
            assert!(close(cx - tcx, 2.0 * k), "{what}：中心距 {}", cx - tcx);
        });
        assert_eq!(n, 960, "扫描条数");
    }

    /// 期望是改动前的公式：`upper = mid - 4.6k`、`lower = mid + 2.6k`、环 `2.4k / 0.8k`、
    /// 点 `1.8k`、逗号比例 `k`。点击区不吃 `k`，由 `标点格点击区不变` 覆盖。
    ///
    /// **Validates: Requirements 4.7, 4.8, 4.10**
    // Feature: fix-duplicate-floating-bars-punctuation-layout, Property 7: Non-horizontal punctuation preservation
    #[test]
    fn 属性7_任意正比例下纵向和尺寸按旧公式不变() {
        let n = for_each_scan(|k, cell, chinese, g| {
            let mid = (cell.top + cell.bottom) as f32 / 2.0;
            let (ring, _, tcy, tr, thw, _, cy, ck) = unpack_punct(g);
            let what = format!(
                "k={k} 格子={:?} 中文标点={chinese}",
                (cell.top, cell.bottom)
            );
            assert_eq!(ring, chinese, "{what}：图形种类");
            assert!(close(tcy, mid - 4.6 * k), "{what}：句号 cy {tcy}");
            assert!(close(cy, mid + 2.6 * k), "{what}：逗号 cy {cy}");
            assert!(close(ck, k), "{what}：逗号比例 {ck}");
            if chinese {
                assert!(close(tr, 2.4 * k), "{what}：环半径 {tr}");
                assert!(close(thw, 0.8 * k), "{what}：环半线宽 {thw}");
            } else {
                assert!(close(tr, 1.8 * k), "{what}：点半径 {tr}");
            }
        });
        assert_eq!(n, 960, "扫描条数");
    }

    /// 只画第三格（中文标点或半角），颜色规则同 `paint_bar`。返回不透明像素
    /// （`0xAARRGGBB`；GDI 不写 alpha，这里统一置 255）和宽高。
    unsafe fn render_punct_cell(chinese: bool) -> (Vec<u32>, i32, i32) {
        let cell = item_rects()[2];
        let (w, h) = (cell.right - cell.left, cell.bottom - cell.top);
        let local = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        let mut c = unsafe { Canvas::new(w, h) }.expect("画布创建失败");
        let pal = bar_palette(false);
        let fg = if chinese { pal.accent } else { pal.dim };
        unsafe {
            fill_rect(c.dc, &local, pal.bg);
            draw_glyphs_aa(c.dc, &local, &punct_glyphs(&local, chinese, dpi_k()), fg);
        }
        let mut px = unsafe { c.pixels() }.to_vec();
        for p in px.iter_mut() {
            *p |= 0xFF00_0000;
        }
        (px, w, h)
    }

    /// 墨迹横向质心：只看 `y0..y1` 这几行，权重 = 765 - (r + g + b)（白底为 0）。
    fn ink_centroid_x(px: &[u32], w: i32, y0: f32, y1: f32) -> f32 {
        let (mut sum, mut wsum) = (0.0f32, 0.0f32);
        for (i, &p) in px.iter().enumerate() {
            let (x, y) = ((i as i32 % w) as f32 + 0.5, (i as i32 / w) as f32 + 0.5);
            if y < y0 || y > y1 {
                continue;
            }
            let ink =
                765.0 - ((p >> 16) & 0xFF) as f32 - ((p >> 8) & 0xFF) as f32 - (p & 0xFF) as f32;
            sum += ink * x;
            wsum += ink;
        }
        assert!(wsum > 0.0, "第 {y0}..{y1} 行没有墨迹");
        sum / wsum
    }

    /// 在实际渲染出来的第三格里量：句号那几行的墨迹质心在格子中心左边，
    /// 逗号圆头上半部（避开往左下甩的尾巴）的质心在右边。两种标点状态都量。
    #[test]
    fn 标点预览里句号在左逗号在右() {
        let k = dpi_k();
        for chinese in [true, false] {
            let (px, w, h) = unsafe { render_punct_cell(chinese) };
            let center = w as f32 / 2.0;
            let mid = h as f32 / 2.0;
            let upper = mid - 4.6 * k;
            let lower = mid + 2.6 * k;
            let reach = if chinese { 2.4 + 0.8 + 1.0 } else { 1.8 + 1.0 } * k;
            let period = ink_centroid_x(&px, w, upper - reach, upper + reach);
            let comma = ink_centroid_x(&px, w, lower - 2.3 * k, lower);
            assert!(
                period < center - 0.5 * k,
                "中文标点={chinese}：句号质心 {period}，格子中心 {center}"
            );
            assert!(
                comma > center + 0.5 * k,
                "中文标点={chinese}：逗号头质心 {comma}，格子中心 {center}"
            );
        }
    }

    #[test]
    fn 五格从左到右不重叠() {
        let rects = item_rects();
        for i in 1..5 {
            assert!(
                rects[i].left >= rects[i - 1].right,
                "第 {i} 格左边要 >= 上一格右边"
            );
        }
    }

    #[test]
    fn 窗口尺寸等于最后一格右边加内边距() {
        let (w, h) = window_size();
        let last = item_rects()[4];
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
        let last = rects[4];
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

        // `cargo test` 进程一般不在前台，而后台激活按设计不显示；这里把"前台"固定为真，
        // 走的仍是生产的仲裁路径（真实的枚举、绘制、ShowWindow）。
        show_with_foreground(|| true);
        let raw = HWND_CELL.with(|c| c.get());
        assert_ne!(raw, 0, "show() 之后应该有窗口句柄");
        let hwnd = raw as HWND;
        assert_ne!(
            unsafe { IsWindowVisible(hwnd) },
            0,
            "前台激活之后窗口应该可见"
        );
        assert_eq!(
            VISIBLE_OWNER.load(Ordering::SeqCst),
            raw,
            "显示之后原子所有权是这条浮条"
        );

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
        assert_eq!(
            VISIBLE_OWNER.load(Ordering::SeqCst),
            OWNER_NONE,
            "hide() 之后释放所有权"
        );
        assert!(
            !INSTANCE_VISIBILITY.with(|c| c.get().wanted),
            "hide() 之后本线程不再想显示"
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

    /// 用和窗口完全相同的 `paint_bar` 渲一帧，浅色/暗色各一份，叠在对应的"桌面"上：
    /// `bar-preview.bmp`（浅色 + 白底）、`bar-preview-dark.bmp`（暗色 + 深底）、
    /// `bar-preview-hover.bmp`（悬停在标点格上，看垫底的圆角）。
    /// 改配色/布局时先看这几张图（圆角抗锯齿在深色桌面上最容易看出来）。
    #[test]
    fn 渲染预览图() {
        let (w, h) = window_size();
        unsafe {
            let mut c = Canvas::new(w, h).expect("画布创建失败");
            paint_bar(&mut c, -1, false);
            let px = c.pixels().to_vec();
            write_preview("bar-preview.bmp", &px, w, h, (0xFF, 0xFF, 0xFF));

            let mut c = Canvas::new(w, h).expect("画布创建失败");
            paint_bar(&mut c, -1, true);
            let px = c.pixels().to_vec();
            write_preview("bar-preview-dark.bmp", &px, w, h, (0x1E, 0x1E, 0x22));

            // 悬停在标点格上（看垫底的圆角是不是也平滑）
            let mut c = Canvas::new(w, h).expect("画布创建失败");
            paint_bar(&mut c, 2, false);
            let px = c.pixels().to_vec();
            write_preview("bar-preview-hover.bmp", &px, w, h, (0xFF, 0xFF, 0xFF));

            // 第三格单独两张：中文标点和半角（`paint_bar` 只画注册表里的当前状态）
            for (chinese, name) in [
                (true, "bar-preview-punct-cn.bmp"),
                (false, "bar-preview-punct-half.bmp"),
            ] {
                let (px, w, h) = render_punct_cell(chinese);
                write_preview(name, &px, w, h, (0xFF, 0xFF, 0xFF));
            }
        }
    }

    /// 相对亮度（0–255，ITU-R BT.601 权重）。COLORREF 低字节是红。
    fn luma(c: u32) -> i32 {
        let r = (c & 0xFF) as i32;
        let g = ((c >> 8) & 0xFF) as i32;
        let b = ((c >> 16) & 0xFF) as i32;
        (r * 299 + g * 587 + b * 114) / 1000
    }

    /// 两套主题都得能看清：底色深度对、正文和底色拉得开、
    /// "开/关"两个状态之间也要能一眼分出来。改配色时这条会先炸。
    #[test]
    fn 两套主题的底色和墨迹都分得开() {
        assert!(luma(PALETTE_LIGHT.bg) > 200, "浅色主题底色应该是亮的");
        assert!(luma(PALETTE_DARK.bg) < 70, "暗色主题底色应该是深的");

        for (name, pal) in [("浅色", &PALETTE_LIGHT), ("暗色", &PALETTE_DARK)] {
            let text_gap = (luma(pal.text) - luma(pal.bg)).abs();
            assert!(text_gap >= 100, "{name}：正文与底色亮度差 {text_gap}，太小");
            let accent_gap = (luma(pal.accent) - luma(pal.bg)).abs();
            assert!(
                accent_gap >= 100,
                "{name}：激活色与底色亮度差 {accent_gap}，太小"
            );
            let dim_gap = (luma(pal.dim) - luma(pal.bg)).abs();
            assert!(
                dim_gap >= 40,
                "{name}：未激活色与底色亮度差 {dim_gap}，太小"
            );
            // 开/关要能分开：激活色与灰至少差 40
            let on_off = (luma(pal.accent) - luma(pal.dim)).abs();
            assert!(on_off >= 40, "{name}：开与关只差 {on_off}，分不出来");
            // 悬停垫底不能和底色一样
            let hover_gap = (luma(pal.hover) - luma(pal.bg)).abs();
            assert!(
                hover_gap >= 3,
                "{name}：悬停垫底和底色几乎一样（{hover_gap}）"
            );
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

    // -----------------------------------------------------------------------
    // AI 面板开着时的外圈彩光
    // -----------------------------------------------------------------------

    /// 一个 200×40、圆角 8 的浮条，四周留 7 的边（96 DPI 的设计值）。
    const GBAR: (f32, f32, f32, f32) = (7.0, 7.0, 207.0, 47.0);

    fn gp(x: f32, y: f32, phase: f32) -> (f32, f32, f32, f32) {
        glow_pixel(x, y, GBAR, 8.0, 7.0, phase, 1.0)
    }

    /// HSV 色相（度）。
    fn hue(r: f32, g: f32, b: f32) -> f32 {
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        if mx - mn < 1.0 {
            return 0.0;
        }
        let d = mx - mn;
        let h = if mx == r {
            ((g - b) / d).rem_euclid(6.0)
        } else if mx == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h * 60.0
    }

    #[test]
    fn 彩光贴边最亮往外变淡且到画布边缘前淡完() {
        // 上边正中往外走：贴边那一像素最亮，越往外越淡
        let at = |dy: f32| gp(107.0, 7.0 - dy, 0.3).0;
        assert!(at(0.8) > 0.7, "贴边亮线 alpha {}", at(0.8));
        assert!(at(3.0) < at(0.8), "往外变淡");
        assert!(at(5.0) < at(3.0), "继续变淡");
        // 画布最外一像素（离本体 6.5）必须已经透明，否则会被画布边缘切出一道硬边
        assert!(at(6.5) < 0.01, "画布边缘 alpha {}", at(6.5));
        // 本体里面不发光
        assert_eq!(gp(107.0, 27.0, 0.3).0, 0.0, "浮条正中不该有光");
    }

    #[test]
    fn 彩光以紫色为主但不止一种颜色() {
        // 沿圆周均匀取 360 个色，数一数落在紫系（色相 250°–320°）的有多少
        let mut purple = 0;
        let mut other_hues = std::collections::BTreeSet::new();
        for i in 0..360 {
            let (r, g, b) = glow_color(i as f32 / 360.0);
            let h = hue(r, g, b);
            if (250.0..=320.0).contains(&h) {
                purple += 1;
            } else {
                other_hues.insert((h / 60.0) as i32);
            }
        }
        assert!(purple * 100 / 360 >= 50, "紫系只占 {purple}/360");
        assert!(
            other_hues.len() >= 3,
            "五彩：紫之外至少还有三个色区，实际 {other_hues:?}"
        );
    }

    #[test]
    fn 彩光会转() {
        // 同一个像素，相位不同颜色就不同（转动就是靠这个）
        let a = gp(107.0, 6.0, 0.0);
        let b = gp(107.0, 6.0, 0.25);
        let diff = (a.1 - b.1).abs() + (a.2 - b.2).abs() + (a.3 - b.3).abs();
        assert!(diff > 40.0, "转了四分之一圈颜色几乎没变：{a:?} vs {b:?}");
        // 转满一圈回到原样
        let c = gp(107.0, 6.0, 1.0);
        assert!((a.1 - c.1).abs() < 0.5 && (a.3 - c.3).abs() < 0.5);
    }

    #[test]
    fn 合成时不透明的本体盖住彩光_外圈不亮时全透明() {
        let opaque = 0xFF_11_22_33u32;
        assert_eq!(
            over_premul(opaque, 0x80_40_00_40),
            opaque,
            "本体不透明就完全盖住"
        );
        assert_eq!(
            over_premul(0, 0x80_40_00_40),
            0x80_40_00_40,
            "本体透明就露出彩光"
        );

        let (bw, bh) = (20, 10);
        let g = glow_pad();
        let (w, h) = (bw + 2 * g, bh + 2 * g);
        let bar = vec![opaque; (bw * bh) as usize];
        let mut out = vec![0xDEAD_BEEFu32; (w * h) as usize];
        compose_outer(&mut out, &bar, bw, bh, None);
        assert_eq!(out[0], 0, "不亮时外圈全透明（鼠标能穿过去）");
        assert_eq!(out[(g * w + g) as usize], opaque, "本体放在 (pad, pad)");
        assert_eq!(out[((g + bh - 1) * w + g + bw - 1) as usize], opaque);

        compose_outer(&mut out, &bar, bw, bh, Some(0.0));
        let top_mid = ((g - 1) * w + g + bw / 2) as usize;
        assert!(out[top_mid] >> 24 > 100, "亮的时候本体外沿有光");
        assert_eq!(
            out[(g * w + g + 3) as usize],
            opaque,
            "本体本身不被彩光改色"
        );
    }

    #[test]
    fn 窗口坐标换成浮条本体坐标() {
        let g = glow_pad();
        assert_eq!(to_bar_coords(g, g), (0, 0));
        let r = item_rects()[3];
        // 鼠标在 AI 格正中：窗口坐标多出一圈彩光边，换算后要命中 AI 格
        let (x, y) = to_bar_coords((r.left + r.right) / 2 + g, (r.top + r.bottom) / 2 + g);
        assert_eq!(hit_test(x, y), 3);
        let (ow, oh) = outer_size();
        let (w, h) = window_size();
        assert_eq!((ow - w, oh - h), (2 * g, 2 * g));
    }

    /// 彩光预览：三个相位并排存成 `tsf/target/bar-preview-glow.bmp`（叠在白和深灰两种桌面上）。
    #[test]
    fn 渲染彩光预览图() {
        let (bw, bh) = window_size();
        let (w, h) = outer_size();
        unsafe {
            let mut c = Canvas::new(bw, bh).expect("画布创建失败");
            paint_bar(&mut c, -1, false);
            let bar = c.pixels().to_vec();
            let mut sheet = vec![0u32; (w * h * 3) as usize];
            for (i, phase) in [0.0f32, 0.33, 0.66].iter().enumerate() {
                let mut out = vec![0u32; (w * h) as usize];
                compose_outer(&mut out, &bar, bw, bh, Some(*phase));
                for y in 0..h {
                    for x in 0..w {
                        sheet[((y + h * i as i32) * w + x) as usize] = out[(y * w + x) as usize];
                    }
                }
            }
            write_preview("bar-preview-glow.bmp", &sheet, w, h * 3, (0xFF, 0xFF, 0xFF));
            write_preview(
                "bar-preview-glow-dark.bmp",
                &sheet,
                w,
                h * 3,
                (0x2A, 0x2A, 0x2E),
            );
        }
    }

    #[test]
    fn 出界的浮条挪回工作区() {
        // 用户实测：工作区右边 1910，存的位置 (1776, 983)，浮条本体 275×45
        let work = RECT {
            left: 0,
            top: 0,
            right: 1910,
            bottom: 1030,
        };
        assert_eq!(
            clamp_into(&work, 1776, 983, 275, 45),
            (1635, 983),
            "右边出界往左挪"
        );
        assert_eq!(
            clamp_into(&work, 1480, 930, 275, 45),
            (1480, 930),
            "在屏幕里不动"
        );
        assert_eq!(
            clamp_into(&work, 100, 1020, 275, 45),
            (100, 985),
            "压住任务栏往上挪"
        );
        assert_eq!(clamp_into(&work, -40, -10, 275, 45), (0, 0), "左上出界");
        // 副屏在左边（负坐标）：按那块屏的工作区挪，不会被拽到主屏
        let left = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        assert_eq!(clamp_into(&left, -100, 500, 275, 45), (-275, 500));
        // 比工作区还宽：贴左
        let tiny = RECT {
            left: 10,
            top: 10,
            right: 200,
            bottom: 300,
        };
        assert_eq!(clamp_into(&tiny, 50, 50, 275, 45), (10, 50));
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
