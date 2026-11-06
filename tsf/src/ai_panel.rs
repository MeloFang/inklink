//! AI 浮动面板（1.1.42 起；1.1.43 常驻；1.1.44 改版成卡片样式、可拖动）。
//!
//! 浮条上的「AI」按钮打开它：面板一直读当前选中的文字显示在上面的输入卡里，
//! 下面列出 翻译 / 改写 / 续写；点一个动作后，下半部分换成结果卡，
//! 最后 复制 / 重新生成 / 取消 / 确定。
//!
//! ## 外观（1.1.44，参照 WPS 的 AI 助手）
//!
//! 白底大圆角卡片 + 一圈粉→紫→蓝的渐变描边 + 柔和阴影；动作是一行一行的列表
//! （图标 + 名字 + 一句说明），悬停时浅紫垫底。窗口是分层窗口（`UpdateLayeredWindow`），
//! 圆角、描边、阴影都按像素算 alpha，所以边缘平滑。画法和浮条一样：先往 32 位 DIB 里
//! 画底色和圆角块，再用 GDI 写字，最后统一算外形的 alpha。
//!
//! ## 拖动
//!
//! 按住标题栏或任何空白处拖动；松手时记下位置（`state::write_panel_pos`），
//! 下次打开还在那里（位置跑到屏幕外就退回屏幕底部正中）。
//!
//! ## 1.1.43 起的行为
//!
//! - **常驻**：只有右上角 × 能关；宿主切到后台时先收起，回来再出现。
//! - **实时读选区**：开着时每 400ms 读一次（`ai::poll_selection`）。
//! - 开着的时候浮条外圈有一圈转动的彩光（`bar::set_glow`）。
//!
//! ## 硬约束
//!
//! - **不抢焦点**：`WS_EX_NOACTIVATE`。抢焦点会让文档失去 TSF 上下文。
//! - **非模态**，所有状态写在面板上，选区没变不重画。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, BeginPaint, CreateFontW, DT_CENTER, DT_EDITCONTROL,
    DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject,
    DrawTextW, EndPaint, GetMonitorInfoW, HDC, HGDIOBJ, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint, MonitorFromWindow, PAINTSTRUCT,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetForegroundWindow,
    GetWindowRect, GetWindowThreadProcessId, IDC_ARROW, IsWindowVisible, KillTimer, LoadCursorW,
    MA_NOACTIVATE, MoveWindow, PostMessageW, RegisterClassW, SPI_GETWORKAREA, SW_HIDE,
    SW_SHOWNOACTIVATE, SetTimer, ShowWindow, SystemParametersInfoW, ULW_ALPHA, UpdateLayeredWindow,
    WM_CLOSE, WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT,
    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

use crate::bar::{Canvas, coverage, round_rect_sdf};

const CLASS_NAME: &str = "inklink-ai-panel";
const TIMER_POLL: usize = 1;
/// 定时器周期。每一拍检查"该不该显示"，每 `SELECTION_EVERY` 拍读一次选区。
const POLL_MS: u32 = 100;
/// 400ms 读一次选区：选完字半秒内面板就有，又不至于每拍都排编辑会话。
const SELECTION_EVERY: u32 = 4;
static TICKS: AtomicU32 = AtomicU32::new(0);

/// 面板本体（96 DPI 设计尺寸，不含阴影）。
const BODY_W: i32 = 440;
const BODY_H: i32 = 344;
/// 本体四周留给阴影的边。
const SHADOW: i32 = 12;
const RADIUS: i32 = 14;
const PAD: i32 = 18;
/// 面板离工作区底边的距离（任务栏上方留一点空）。
const BOTTOM_MARGIN: i32 = 24;

const FONT_UI: &str = "Microsoft YaHei UI";
/// Win10/11 自带的图标字体（齿轮、关闭、地球、笔……）。
const FONT_ICON: &str = "Segoe MDL2 Assets";
const ICON_SETTINGS: &str = "\u{E713}";
const ICON_CLOSE: &str = "\u{E8BB}";
const ICON_SPARKLE: &str = "\u{2726}";
/// 三个动作的图标、名字、说明。
const ACTIONS: [(&str, &str, &str); 3] = [
    ("\u{E774}", "翻译", "译成中文，替换选中的文字"),
    ("\u{E70F}", "改写", "改错别字，理顺用词和逻辑"),
    ("\u{E710}", "续写", "顺着选中的文字往下写"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Disabled,
    Loading,
    NoSelection,
    /// 选区超过 `MAX_INPUT_CHARS`
    TooLong,
    Ready,
    Busy,
    Done,
    /// 点了确定，等编辑会话写回
    Applying,
    Error,
}

struct State {
    phase: Phase,
    source: String,
    result: String,
    /// 上一次用的动作（重新生成要用）。
    action: u8,
    /// 写回之后的一句话：`(写进去了吗, 说明)`。
    note: Option<(bool, String)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    phase: Phase::Disabled,
    source: String::new(),
    result: String::new(),
    action: 0,
    note: None,
});
static HWND_CELL: AtomicIsize = AtomicIsize::new(0);
/// 鼠标悬停在哪儿（只用来画浅色垫底）。
static HOVER: Mutex<Hit> = Mutex::new(Hit::None);

/// 一次按下：按在哪、按下时鼠标和窗口的屏幕位置。
struct Press {
    hit: Hit,
    cursor: POINT,
    window: POINT,
}
static PRESS: Mutex<Option<Press>> = Mutex::new(None);
static DRAGGING: AtomicBool = AtomicBool::new(false);

// COLORREF 是 0x00BBGGRR
struct Palette {
    bg: u32,
    text: u32,
    dim: u32,
    /// 主色（紫）和渐变的另一端（偏粉的紫）
    accent: u32,
    accent2: u32,
    field: u32,
    field_border: u32,
    hover: u32,
    chip: u32,
    danger: u32,
    ok: u32,
    shadow: f32,
}

const LIGHT: Palette = Palette {
    bg: 0x00_FF_FF_FF,
    text: 0x00_24_1F_1F,         // #1f1f24
    dim: 0x00_94_8A_8A,          // #8a8a94
    accent: 0x00_ED_3A_7C,       // #7c3aed
    accent2: 0x00_F7_55_A8,      // #a855f7
    field: 0x00_FA_F7_F7,        // #f7f7fa
    field_border: 0x00_EA_E4_E4, // #e4e4ea
    hover: 0x00_FD_F0_F3,        // #f3f0fd
    chip: 0x00_FE_EB_F1,         // #f1ebfe
    danger: 0x00_20_2D_D9,       // #d92d20
    ok: 0x004A_A316,             // #16a34a
    shadow: 0.16,
};

const DARK: Palette = Palette {
    bg: 0x00_26_22_22,           // #222226
    text: 0x00_F0_ED_ED,         // #ededf0
    dim: 0x00_A6_9A_9A,          // #9a9aa6
    accent: 0x00_FA_8B_A7,       // #a78bfa
    accent2: 0x00_FC_84_C0,      // #c084fc
    field: 0x00_33_2D_2D,        // #2d2d33
    field_border: 0x00_44_3C_3C, // #3c3c44
    hover: 0x00_3F_30_34,        // #34303f
    chip: 0x00_50_31_3A,         // #3a3150
    danger: 0x00_6B_6B_FF,       // #ff6b6b
    ok: 0x00_80_DE_4A,           // #4ade80
    shadow: 0.45,
};

fn palette(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

/// 描边渐变的色标（`0xRRGGBB`）：粉 → 紫 → 蓝 → 紫 → 粉，绕一圈接得上。
const BORDER_STOPS: [(f32, u32); 5] = [
    (0.00, 0xF472B6),
    (0.30, 0xA855F7),
    (0.55, 0x60A5FA),
    (0.80, 0x8B5CF6),
    (1.00, 0xF472B6),
];
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(v: i32) -> i32 {
    let dpi = unsafe { GetDpiForSystem() };
    let dpi = if dpi == 0 { 96 } else { dpi as i32 };
    (v * dpi + 48) / 96
}

fn dpi_k() -> f32 {
    scale(96) as f32 / 96.0
}

fn make_font(px: i32, weight: i32, face: &str) -> HGDIOBJ {
    let face = wide(face);
    unsafe {
        CreateFontW(
            -scale(px),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            1,
            0,
            0,
            4, // 灰度抗锯齿：小字号彩色字不能用 ClearType
            0,
            face.as_ptr(),
        ) as HGDIOBJ
    }
}

/// COLORREF（0x00BBGGRR）→ (r, g, b)
fn rgb(c: u32) -> (f32, f32, f32) {
    (
        (c & 0xFF) as f32,
        ((c >> 8) & 0xFF) as f32,
        ((c >> 16) & 0xFF) as f32,
    )
}

/// 0xRRGGBB → (r, g, b)
fn rgb_hex(c: u32) -> (f32, f32, f32) {
    (
        ((c >> 16) & 0xFF) as f32,
        ((c >> 8) & 0xFF) as f32,
        (c & 0xFF) as f32,
    )
}

/// 描边沿圆周位置 `t`（0..1）上的颜色。
fn border_color(t: f32) -> (f32, f32, f32) {
    let t = t.rem_euclid(1.0);
    for pair in BORDER_STOPS.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if t >= t0 && t <= t1 {
            let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            let (a, b) = (rgb_hex(c0), rgb_hex(c1));
            return (
                a.0 + (b.0 - a.0) * f,
                a.1 + (b.1 - a.1) * f,
                a.2 + (b.2 - a.2) * f,
            );
        }
    }
    rgb_hex(BORDER_STOPS[0].1)
}

/// 在不透明的画布上叠一块抗锯齿圆角。`color(t)` 给出横向位置 t（0..1）处的颜色，
/// 纯色就是一个常量闭包，渐变按钮按 t 插值。
fn fill_round(
    px: &mut [u32],
    w: i32,
    h: i32,
    r: &RECT,
    radius: f32,
    color: &dyn Fn(f32) -> (f32, f32, f32),
) {
    let (x0, y0, x1, y1) = (r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
    let span = (x1 - x0).max(1.0);
    for y in r.top.max(0)..r.bottom.min(h) {
        for x in r.left.max(0)..r.right.min(w) {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let a = coverage(round_rect_sdf(fx, fy, x0, y0, x1, y1, radius));
            if a <= 0.0 {
                continue;
            }
            let Some(slot) = px.get_mut((y * w + x) as usize) else {
                continue;
            };
            let (cr, cg, cb) = color((fx - x0) / span);
            let p = *slot;
            let (pr, pg, pb) = (
                ((p >> 16) & 0xFF) as f32,
                ((p >> 8) & 0xFF) as f32,
                (p & 0xFF) as f32,
            );
            let mix = |o: f32, n: f32| (o + (n - o) * a + 0.5).clamp(0.0, 255.0) as u32;
            *slot = (mix(pr, cr) << 16) | (mix(pg, cg) << 8) | mix(pb, cb);
        }
    }
}

fn solid(c: u32) -> impl Fn(f32) -> (f32, f32, f32) {
    let v = rgb(c);
    move |_| v
}

fn gradient(c1: u32, c2: u32) -> impl Fn(f32) -> (f32, f32, f32) {
    let (a, b) = (rgb(c1), rgb(c2));
    move |t| {
        (
            a.0 + (b.0 - a.0) * t,
            a.1 + (b.1 - a.1) * t,
            a.2 + (b.2 - a.2) * t,
        )
    }
}

/// 面板放在哪：工作区（已扣任务栏）底部正中，离底边 `margin`。纯函数，好测。
///
/// 面板比工作区还宽/高时贴左/贴顶，不让它跑出屏幕。
fn bottom_center(work: &RECT, w: i32, h: i32, margin: i32) -> (i32, i32) {
    let x = work.left + ((work.right - work.left) - w) / 2;
    let y = work.bottom - h - margin;
    (x.max(work.left), y.max(work.top))
}

/// 把面板整个挪进工作区（拖到一半被屏幕边缘切掉、或者换了分辨率）。纯函数。
fn clamp_into(work: &RECT, x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    let x = x.min(work.right - w).max(work.left);
    let y = y.min(work.bottom - h).max(work.top);
    (x, y)
}

/// 下半部分显示结果卡（而不是动作列表）的阶段。
fn result_view(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Busy | Phase::Done | Phase::Applying | Phase::Error
    )
}

/// 这个阶段能不能用「粘贴」：AI 没启用、正在生成、正在写回时都不收。
fn accepts_paste(phase: Phase) -> bool {
    !matches!(phase, Phase::Disabled | Phase::Busy | Phase::Applying)
}

/// 哪些阶段算"手里有一段原文、可以发请求"（重新生成也走这里）。
fn actions_enabled(phase: Phase) -> bool {
    matches!(phase, Phase::Ready | Phase::Done | Phase::Error)
}

/// 面板里所有区域（**本体**坐标，不含阴影边）。纯函数，好测。
struct Layout {
    header: RECT,
    close: RECT,
    settings: RECT,
    source_box: RECT,
    chip: RECT,
    /// 输入卡右下角的「粘贴」小药丸：没读到选区时用户自己把文字弄进来
    paste: RECT,
    rows: [RECT; 3],
    note: RECT,
    open_settings: RECT,
    result_label: RECT,
    result_box: RECT,
    copy: RECT,
    regen: RECT,
    cancel: RECT,
    confirm: RECT,
}

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
    RECT {
        left,
        top,
        right,
        bottom,
    }
}

fn layout(w: i32, h: i32) -> Layout {
    let pad = scale(PAD);
    let icon = scale(28);
    let header = rect(0, 0, w, scale(52));
    let close = rect(
        w - pad - icon + scale(6),
        scale(12),
        w - pad + scale(6),
        scale(12) + icon,
    );
    let settings = rect(
        close.left - scale(4) - icon,
        close.top,
        close.left - scale(4),
        close.bottom,
    );
    let source_box = rect(pad, scale(52), w - pad, scale(52) + scale(96));
    let chip = rect(
        source_box.left + scale(12),
        source_box.bottom - scale(12) - scale(24),
        source_box.left + scale(12) + scale(120),
        source_box.bottom - scale(12),
    );
    let paste = rect(
        source_box.right - scale(12) - scale(52),
        chip.top,
        source_box.right - scale(12),
        chip.bottom,
    );
    let row_h = scale(42);
    let ry = source_box.bottom + scale(10);
    let mut rows = [rect(0, 0, 0, 0); 3];
    for (i, r) in rows.iter_mut().enumerate() {
        let i = i as i32;
        *r = rect(
            pad - scale(8),
            ry + row_h * i,
            w - pad + scale(8),
            ry + row_h * (i + 1),
        );
    }
    let btn_h = scale(32);
    let by = h - pad - btn_h;
    let note = rect(pad, rows[2].bottom + scale(6), w - pad, h - scale(10));
    let open_settings = rect(w - pad - scale(96), by, w - pad, by + btn_h);
    let result_label = rect(
        pad,
        source_box.bottom + scale(12),
        w - pad,
        source_box.bottom + scale(34),
    );
    let result_box = rect(pad, result_label.bottom + scale(4), w - pad, by - scale(12));
    let confirm = rect(w - pad - scale(76), by, w - pad, by + btn_h);
    let cancel = rect(
        confirm.left - scale(8) - scale(68),
        by,
        confirm.left - scale(8),
        by + btn_h,
    );
    let copy = rect(pad, by, pad + scale(72), by + btn_h);
    let regen = rect(
        copy.right + scale(8),
        by,
        copy.right + scale(8) + scale(96),
        by + btn_h,
    );
    Layout {
        header,
        close,
        settings,
        source_box,
        chip,
        paste,
        rows,
        note,
        open_settings,
        result_label,
        result_box,
        copy,
        regen,
        cancel,
        confirm,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    None,
    /// 空白处：按住可以拖动面板
    Drag,
    Close,
    Settings,
    Action(u8),
    Confirm,
    Cancel,
    Regenerate,
    Copy,
    Paste,
}

fn inside(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// 本体坐标 (x, y) 落在哪个可点的地方。按状态区分：不可点的按钮当空白（可以拖）。
fn hit(x: i32, y: i32, w: i32, h: i32, phase: Phase) -> Hit {
    if x < 0 || y < 0 || x >= w || y >= h {
        return Hit::None;
    }
    let l = layout(w, h);
    if inside(&l.close, x, y) {
        return Hit::Close;
    }
    if inside(&l.settings, x, y) {
        return Hit::Settings;
    }
    // 「粘贴」任何时候都能点（除了不可用/正忙）：读取失败、或者用户压根不想在文档里选字
    if accepts_paste(phase) && inside(&l.paste, x, y) {
        return Hit::Paste;
    }
    if result_view(phase) {
        let done = phase == Phase::Done;
        let settled = matches!(phase, Phase::Done | Phase::Error);
        if done && inside(&l.confirm, x, y) {
            return Hit::Confirm;
        }
        if done && inside(&l.copy, x, y) {
            return Hit::Copy;
        }
        if settled && inside(&l.regen, x, y) {
            return Hit::Regenerate;
        }
        // 生成中也能取消：不等了，回到选动作那一步
        if (settled || phase == Phase::Busy) && inside(&l.cancel, x, y) {
            return Hit::Cancel;
        }
    } else {
        if phase == Phase::Disabled && inside(&l.open_settings, x, y) {
            return Hit::Settings;
        }
        if phase == Phase::Ready {
            for (i, r) in l.rows.iter().enumerate() {
                if inside(r, x, y) {
                    return Hit::Action(i as u8);
                }
            }
        }
    }
    Hit::Drag
}
/// 画一帧要的全部状态：(阶段, 原文, 结果, 动作, 提示)。
type Snap = (Phase, String, String, u8, Option<(bool, String)>);

fn state_snapshot() -> Snap {
    let g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    (
        g.phase,
        g.source.clone(),
        g.result.clone(),
        g.action,
        g.note.clone(),
    )
}

/// 改状态；`f` 返回 false 表示"没变"，就不重画（轮询每 400ms 一次，不能每次都闪）。
fn update(f: impl FnOnce(&mut State) -> bool) {
    let changed = {
        let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    };
    if changed {
        redraw();
    }
}

/// 分层窗口没有 WM_PAINT 那一套：状态一变就直接画一帧交给系统合成。
fn redraw() {
    let h = HWND_CELL.load(Ordering::SeqCst);
    if h != 0 {
        present(h as HWND);
    }
}

/// 「正在读选区」已经过了多少拍（50ms 一拍）。读不回来就退到可粘贴状态。
static LOADING_TICKS: AtomicU32 = AtomicU32::new(0);

/// 选区轮询的结果落到哪个阶段（纯函数，好测）。
///
/// 只有"等选区"的几个阶段才跟着选区变；生成中、出了结果、正在写回时，选区变了也不动 ——
/// 否则用户点确定之前手一抖点到别处，结果就被冲掉了。
fn follow_selection(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Loading | Phase::NoSelection | Phase::TooLong | Phase::Ready
    )
}

pub fn show_disabled() {
    update(|s| {
        s.phase = Phase::Disabled;
        s.source.clear();
        s.result.clear();
        s.note = None;
        true
    });
    ensure_window();
}

pub fn show_loading() {
    update(|s| {
        s.phase = Phase::Loading;
        s.source.clear();
        s.result.clear();
        s.note = None;
        true
    });
    ensure_window();
}

/// 面板开着、而且现在要跟着选区变（`ai::poll_selection` 据此决定读不读）。
pub fn wants_selection() -> bool {
    is_open() && follow_selection(state_snapshot().0)
}

/// 正在等模型。`ai::on_done` 用它判断结果还要不要（用户可能已经点了取消）。
pub fn is_busy() -> bool {
    state_snapshot().0 == Phase::Busy
}

pub fn set_no_selection() {
    update(|s| {
        if !follow_selection(s.phase) || (s.phase == Phase::NoSelection && s.source.is_empty()) {
            return false;
        }
        s.phase = Phase::NoSelection;
        s.source.clear();
        true
    });
}

pub fn set_too_long(chars: usize, max: usize) {
    update(|s| {
        if !follow_selection(s.phase) {
            return false;
        }
        let msg = format!("选中了 {chars} 字，超过 {max} 字的上限。选短一点再试。");
        if s.phase == Phase::TooLong && s.source == msg {
            return false;
        }
        s.phase = Phase::TooLong;
        s.source = msg;
        true
    });
}

pub fn set_source(text: &str) {
    update(|s| {
        if !follow_selection(s.phase) || (s.phase == Phase::Ready && s.source == text) {
            return false;
        }
        s.phase = Phase::Ready;
        s.source = text.to_string();
        s.result.clear();
        // 选了新的一段，上一次"已写入"的提示就过时了
        s.note = None;
        true
    });
}

/// 用户自己粘贴进来的文字：不管当前在哪个阶段都收（出了结果之后也要能换一段重来）。
pub fn set_source_pasted(text: &str) {
    update(|s| {
        s.phase = Phase::Ready;
        s.source = text.to_string();
        s.result.clear();
        s.note = None;
        true
    });
}

/// 粘贴进来的文字太长：同样绕开"只在等选区时更新"的限制。
pub fn set_too_long_pasted(chars: usize, max: usize) {
    update(|s| {
        s.phase = Phase::TooLong;
        s.source.clear();
        s.result.clear();
        s.note = Some((
            false,
            format!("{chars} 字，一次最多 {max} 字。换短一点的一段。"),
        ));
        true
    });
}

pub fn set_busy(action: u8) {
    update(|s| {
        s.phase = Phase::Busy;
        s.action = action;
        s.result.clear();
        s.note = None;
        true
    });
}

pub fn set_result(text: &str) {
    update(|s| {
        s.phase = Phase::Done;
        s.result = text.to_string();
        s.note = None;
        true
    });
}

pub fn set_error(msg: &str) {
    update(|s| {
        s.phase = Phase::Error;
        s.result = msg.to_string();
        true
    });
}

pub fn set_applying() {
    update(|s| {
        s.phase = Phase::Applying;
        s.note = None;
        true
    });
}

/// 写回的结果。写进去了 → 回到"等选区"（文档里的选区已经没了）；
/// 没写进去 → 留在结果页，用户可以重选原文再点确定，或者复制结果。
pub fn set_applied(outcome: Result<&str, &str>) {
    update(|s| {
        match outcome {
            Ok(msg) => {
                s.phase = Phase::NoSelection;
                s.source.clear();
                s.result.clear();
                s.note = Some((true, msg.to_string()));
            }
            Err(msg) => {
                s.phase = Phase::Done;
                s.note = Some((false, msg.to_string()));
            }
        }
        true
    });
}

/// 「取消」：丢掉结果（或不等了），回到"选中文字 → 选动作"。面板不关。
pub fn discard_result() {
    update(|s| {
        s.result.clear();
        s.note = None;
        s.phase = if s.source.is_empty() {
            Phase::NoSelection
        } else {
            Phase::Ready
        };
        true
    });
}

/// 面板里当前显示的那段原文（发请求要用）。
pub fn current_source() -> Option<String> {
    let (phase, source, ..) = state_snapshot();
    if actions_enabled(phase) && !source.is_empty() {
        Some(source)
    } else {
        None
    }
}

pub fn last_action() -> Option<u8> {
    let (phase, _, _, action, _) = state_snapshot();
    if matches!(phase, Phase::Done | Phase::Error) {
        Some(action)
    } else {
        None
    }
}

/// 给 ai::confirm 用的（动作、原文、结果）。
pub fn result_for_apply() -> Option<(u8, String, String)> {
    let (phase, source, result, action, _) = state_snapshot();
    if phase != Phase::Done || source.is_empty() || result.is_empty() {
        return None;
    }
    Some((action, source, result))
}

pub fn is_open() -> bool {
    HWND_CELL.load(Ordering::SeqCst) != 0
}

/// 关掉面板（只有 × 和浮条上再点一次「AI」会走到这里）。
///
/// 窗口只能由建它的线程销毁：别的线程调过来就投一个 `WM_CLOSE` 让它自己关。
pub fn hide() {
    let h = HWND_CELL.load(Ordering::SeqCst);
    if h == 0 {
        return;
    }
    let owner = unsafe { GetWindowThreadProcessId(h as HWND, std::ptr::null_mut()) };
    if owner == unsafe { GetCurrentThreadId() } {
        unsafe { DestroyWindow(h as HWND) };
    } else {
        unsafe { PostMessageW(h as HWND, WM_CLOSE, 0, 0) };
    }
}

/// 某个点所在显示器的工作区；`None` = 这个点不在任何显示器上。
fn work_area_at(pt: POINT, nearest: bool) -> Option<RECT> {
    unsafe {
        let flag = if nearest {
            MONITOR_DEFAULTTONEAREST
        } else {
            MONITOR_DEFAULTTONULL
        };
        let mon = MonitorFromPoint(pt, flag);
        if mon.is_null() {
            return None;
        }
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) != 0 {
            Some(mi.rcWork)
        } else {
            None
        }
    }
}

/// 当前前台窗口所在显示器的工作区；取不到退回主显示器工作区。
fn foreground_work_area() -> RECT {
    let mut area: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        let mon = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if !mon.is_null() && GetMonitorInfoW(mon, &mut mi) != 0 {
            return mi.rcWork;
        }
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
    }
    area
}

/// 面板本体放在哪（屏幕坐标）：拖过就用记下的位置（整个挪进那块屏幕），
/// 没拖过、或者那个位置已经不在任何屏幕上了，就放在前台屏幕的底部正中。
fn body_position(w: i32, h: i32) -> (i32, i32) {
    if let Some((x, y)) = crate::state::read_panel_pos() {
        let center = POINT {
            x: x + w / 2,
            y: y + h / 2,
        };
        if let Some(work) = work_area_at(center, false) {
            return clamp_into(&work, x, y, w, h);
        }
    }
    bottom_center(&foreground_work_area(), w, h, scale(BOTTOM_MARGIN))
}

/// 窗口大小（本体 + 四周阴影边）。
fn outer_size() -> (i32, i32) {
    let m = scale(SHADOW);
    (scale(BODY_W) + 2 * m, scale(BODY_H) + 2 * m)
}

fn ensure_window() {
    if is_open() {
        redraw();
        return;
    }
    ensure_class();
    let m = scale(SHADOW);
    let (bw, bh) = (scale(BODY_W), scale(BODY_H));
    let (w, h) = outer_size();
    let (x, y) = body_position(bw, bh);
    // 第一次摆好的位置立刻记下来。
    //
    // 面板每次打开都是新窗口（`hide()` 会 DestroyWindow），没记过位置的话每次都要
    // 按"当前前台那块屏的底部正中"重算 —— 两块屏之间来回开，面板就一会儿在左边、
    // 一会儿在右边（2026-11-06 用户反馈"悬浮窗位置动来动去"）。
    if crate::state::read_panel_pos().is_none() {
        crate::state::write_panel_pos(x, y);
    }

    let name = wide(CLASS_NAME);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            name.as_ptr(),
            name.as_ptr(),
            WS_POPUP,
            x - m,
            y - m,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        crate::log_line("ai-panel: CreateWindowExW failed");
        return;
    }
    HWND_CELL.store(hwnd as isize, Ordering::SeqCst);
    TICKS.store(0, Ordering::Relaxed);
    *HOVER.lock().unwrap_or_else(|p| p.into_inner()) = Hit::None;
    unsafe {
        // 分层窗口要先有内容再显示，否则会闪一下空白
        present(hwnd);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_POLL, POLL_MS, None);
    }
    crate::bar::set_glow(true);
    crate::log_line(&format!("ai-panel: shown at {x},{y} {w}x{h}"));
}

/// 面板该不该露出来：宿主在前台、而且这条线程的输入法还开着。
fn should_be_visible() -> bool {
    crate::bar::foreground_is_self() && crate::ai::has_service()
}

fn cursor_in(hwnd: HWND) -> bool {
    let mut pt = POINT { x: 0, y: 0 };
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut pt);
        GetWindowRect(hwnd, &mut r);
    }
    inside(&r, pt.x, pt.y)
}

/// 定时器一拍：按宿主前后台收起/放出，隔几拍读一次选区，鼠标离开了清掉悬停。
fn on_tick(hwnd: HWND) {
    let want = should_be_visible() || DRAGGING.load(Ordering::Relaxed);
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    if want && !visible {
        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    } else if !want && visible {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
    if !want {
        return;
    }
    if !cursor_in(hwnd) {
        set_hover(Hit::None);
    }
    let n = TICKS.fetch_add(1, Ordering::Relaxed);
    // 读选区超时（2026-11-06：WPS 的 AI 面板里读不回来，面板一直卡在「读取中…」）。
    // 25 拍 × 50ms ≈ 1.25 秒还没结果就退成"没选中"，把「粘贴」这条路露出来。
    if state_snapshot().0 == Phase::Loading {
        if LOADING_TICKS.fetch_add(1, Ordering::Relaxed) >= 25 {
            LOADING_TICKS.store(0, Ordering::Relaxed);
            crate::log_line("ai-panel: 读选区超时，退到可粘贴状态");
            set_no_selection();
        }
    } else {
        LOADING_TICKS.store(0, Ordering::Relaxed);
    }
    if n.is_multiple_of(SELECTION_EVERY) {
        crate::ai::poll_selection();
    }
}

fn set_hover(h: Hit) {
    let changed = {
        let mut g = HOVER.lock().unwrap_or_else(|p| p.into_inner());
        let changed = *g != h;
        *g = h;
        changed
    };
    if changed {
        redraw();
    }
}

/// 读剪贴板里的文字（CF_UNICODETEXT）。不是文字 / 打不开 → None。
fn clipboard_text() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            return None;
        }
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let mut out = None;
        let handle = GetClipboardData(CF_UNICODETEXT);
        if !handle.is_null() {
            let p = GlobalLock(handle) as *const u16;
            if !p.is_null() {
                let mut len = 0usize;
                while *p.add(len) != 0 {
                    len += 1;
                }
                out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, len)));
                GlobalUnlock(handle);
            }
        }
        CloseClipboard();
        out
    }
}

/// 点「粘贴」：把剪贴板里的文字当成本次的原文。
///
/// 为什么要有这个口子：有的宿主（WPS 的 AI 面板就是）读不到选区，面板会一直停在
/// "正在读取选中文字…"；让用户自己复制一段再粘进来，比卡在那里强。
fn paste_source() {
    let Some(raw) = clipboard_text() else {
        update(|s| {
            s.note = Some((
                false,
                "剪贴板里没有文字。先在文档里复制一段，再点「粘贴」。".to_string(),
            ));
            true
        });
        return;
    };
    let text = raw.trim().to_string();
    let chars = text.chars().count();
    crate::log_line(&format!("ai-panel: paste chars={chars}"));
    if chars == 0 {
        update(|s| {
            s.note = Some((false, "剪贴板是空的。".to_string()));
            true
        });
    } else if chars > crate::ai::MAX_INPUT_CHARS {
        set_too_long_pasted(chars, crate::ai::MAX_INPUT_CHARS);
    } else {
        set_source_pasted(&text);
    }
}

/// 把结果写进剪贴板（用户点「复制」时）。宿主不让输入法改字时的退路。
fn copy_to_clipboard(text: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    let buf = wide(text);
    let bytes = buf.len() * 2;
    let h = HWND_CELL.load(Ordering::SeqCst) as HWND;
    unsafe {
        if OpenClipboard(h) == 0 {
            return false;
        }
        EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        let mut ok = false;
        if !mem.is_null() {
            let dst = GlobalLock(mem) as *mut u16;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), dst, buf.len());
                GlobalUnlock(mem);
                // 成功后内存归剪贴板所有，不能再释放
                ok = !SetClipboardData(CF_UNICODETEXT as u32, mem).is_null();
            }
        }
        CloseClipboard();
        ok
    }
}
fn inset(r: &RECT, d: i32) -> RECT {
    rect(r.left + d, r.top + d, r.right - d, r.bottom - d)
}

/// COLORREF → DIB 里的像素值（0x00RRGGBB）。
fn pixel(c: u32) -> u32 {
    let (r, g, b) = rgb(c);
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

fn text(hdc: HDC, r: &RECT, s: &str, fg: u32, font: HGDIOBJ, flags: u32) {
    if s.is_empty() {
        return;
    }
    let buf = wide(s);
    let mut rr = *r;
    unsafe {
        let old = SelectObject(hdc, font);
        SetTextColor(hdc, fg);
        DrawTextW(
            hdc,
            buf.as_ptr(),
            (buf.len() - 1) as i32,
            &mut rr,
            flags | DT_NOPREFIX,
        );
        SelectObject(hdc, old);
    }
}

fn text_width(hdc: HDC, s: &str, font: HGDIOBJ) -> i32 {
    let buf: Vec<u16> = s.encode_utf16().collect();
    if buf.is_empty() {
        return 0;
    }
    let mut sz = SIZE { cx: 0, cy: 0 };
    unsafe {
        let old = SelectObject(hdc, font);
        windows_sys::Win32::Graphics::Gdi::GetTextExtentPoint32W(
            hdc,
            buf.as_ptr(),
            buf.len() as i32,
            &mut sz,
        );
        SelectObject(hdc, old);
    }
    sz.cx
}

struct Fonts {
    title: HGDIOBJ,
    body: HGDIOBJ,
    bold: HGDIOBJ,
    small: HGDIOBJ,
    icon: HGDIOBJ,
    sym: HGDIOBJ,
}

impl Fonts {
    fn new() -> Self {
        Fonts {
            title: make_font(15, 700, FONT_UI),
            body: make_font(13, 400, FONT_UI),
            bold: make_font(13, 600, FONT_UI),
            small: make_font(12, 400, FONT_UI),
            icon: make_font(14, 400, FONT_ICON),
            sym: make_font(14, 400, "Segoe UI Symbol"),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        for f in [
            self.title, self.body, self.bold, self.small, self.icon, self.sym,
        ] {
            if !f.is_null() {
                unsafe { DeleteObject(f) };
            }
        }
    }
}

/// 输入卡里那句话。纯函数：哪个阶段说什么，测试里钉住。
fn source_line(phase: Phase, source: &str) -> &str {
    match phase {
        Phase::Disabled => "AI 还没启用，点右下角「打开设置」",
        Phase::Loading => "正在读取选中文字…",
        Phase::NoSelection => "在文档里选中一段文字，会自动出现在这里",
        _ if source.is_empty() => "（没有选中文字）",
        _ => source,
    }
}

/// 输入卡左下角那个小标签。
fn chip_text(phase: Phase, chars: usize) -> String {
    match phase {
        Phase::Disabled => "AI 未启用".to_string(),
        Phase::Loading => "读取中…".to_string(),
        Phase::NoSelection => "未选中文字".to_string(),
        Phase::TooLong => "选中的太多了".to_string(),
        _ => format!("已选中 {chars} 字"),
    }
}

/// 动作列表下面那行灰字。
fn hint_line(phase: Phase) -> &'static str {
    match phase {
        Phase::Disabled => "在设置里勾选「启用 AI」、填好接口地址，再回到这里",
        Phase::Loading => "正在读取选中的文字…（读不到就复制一段，点上面的「粘贴」）",
        Phase::NoSelection => "选中文字后点一种处理方式；也可以复制一段点「粘贴」",
        Phase::TooLong => "一次最多处理 2000 字",
        Phase::Ready => "点一种处理方式。按住空白处可以拖动面板",
        _ => "",
    }
}

fn result_title(phase: Phase, action: u8) -> String {
    let name = crate::ai::action_name(action);
    match phase {
        Phase::Busy => format!("正在{name}"),
        Phase::Error => format!("{name}失败"),
        _ => format!("{name}结果"),
    }
}

/// 最后一步：按外形给整张图上 alpha，混进渐变描边，外面加一圈往下偏的柔和阴影。
/// 输出是预乘 alpha（`UpdateLayeredWindow` 要的格式）。
fn finish(px: &mut [u32], w: i32, h: i32, m: i32, radius: f32, border_w: f32, shadow: f32) {
    let k = dpi_k();
    let (x0, y0, x1, y1) = (m as f32, m as f32, (w - m) as f32, (h - m) as f32);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (hw, hh) = (((x1 - x0) / 2.0).max(1.0), ((y1 - y0) / 2.0).max(1.0));
    let spread = (m as f32 - 1.0).max(1.0);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = round_rect_sdf(fx, fy, x0, y0, x1, y1, radius);
            let outer = coverage(d);
            let inner = coverage(d + border_w);
            let ds = round_rect_sdf(fx, fy - 2.0 * k, x0, y0, x1, y1, radius);
            let fall = (1.0 - ds.max(0.0) / spread).clamp(0.0, 1.0);
            let sh = shadow * fall * fall;
            let Some(slot) = px.get_mut((y * w + x) as usize) else {
                continue;
            };
            let p = *slot;
            let (cr, cg, cb) = (
                ((p >> 16) & 0xFF) as f32,
                ((p >> 8) & 0xFF) as f32,
                (p & 0xFF) as f32,
            );
            let ring = (outer - inner).max(0.0);
            let (br, bg, bb) = if ring > 0.0 {
                let ang = ((fy - cy) / hh).atan2((fx - cx) / hw);
                border_color(ang / std::f32::consts::TAU + 0.5)
            } else {
                (0.0, 0.0, 0.0)
            };
            let ch = |c: f32, b: f32| (c * inner + b * ring + 0.5).clamp(0.0, 255.0) as u32;
            let a = outer + sh * (1.0 - outer);
            let ai = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u32;
            *slot = (ai << 24) | (ch(cr, br) << 16) | (ch(cg, bg) << 8) | ch(cb, bb);
        }
    }
}
/// 把整块面板画进画布（`m` = 四周阴影边）。不碰窗口，测试可以直接渲预览图。
fn paint_panel(c: &mut Canvas, m: i32, dark: bool, hover: Hit, snap: Snap) {
    let (w, h) = (c.w, c.h);
    let (bw, bh) = (w - 2 * m, h - 2 * m);
    let pal = palette(dark);
    let (phase, source, result, action, note) = snap;
    let l = layout(bw, bh);
    let o = |r: &RECT| rect(r.left + m, r.top + m, r.right + m, r.bottom + m);
    let fonts = Fonts::new();
    let hdc = c.dc;
    let k = dpi_k();

    let chars = source.chars().count();
    let chip_label = chip_text(phase, chars);
    let chip = {
        let r = o(&l.chip);
        rect(
            r.left,
            r.top,
            r.left + text_width(hdc, &chip_label, fonts.small) + scale(20),
            r.bottom,
        )
    };
    let sb = o(&l.source_box);
    let in_result = result_view(phase);
    let done = phase == Phase::Done;
    let settled = matches!(phase, Phase::Done | Phase::Error);
    let can_cancel = settled || phase == Phase::Busy;

    // ── 1. 底色和圆角块（直接改像素，抗锯齿）──
    {
        let px = unsafe { c.pixels() };
        px.fill(pixel(pal.bg));
        let (r10, r8, r6) = (scale(10) as f32, scale(8) as f32, scale(6) as f32);
        let one = scale(1).max(1);
        for (hh, r) in [(Hit::Close, &l.close), (Hit::Settings, &l.settings)] {
            if hover == hh {
                fill_round(px, w, h, &o(r), r6, &solid(pal.hover));
            }
        }
        // 输入卡：浅色描边 + 底色 + 左下角标签
        fill_round(px, w, h, &sb, r10, &solid(pal.field_border));
        fill_round(px, w, h, &inset(&sb, one), r10 - one as f32, &solid(pal.bg));
        let pill = (chip.bottom - chip.top) as f32 / 2.0;
        fill_round(px, w, h, &chip, pill, &solid(pal.chip));
        if in_result {
            fill_round(px, w, h, &o(&l.result_box), r10, &solid(pal.field));
            for (hh, r, on) in [
                (Hit::Copy, &l.copy, done),
                (Hit::Regenerate, &l.regen, settled),
                (Hit::Cancel, &l.cancel, can_cancel),
            ] {
                let rr = o(r);
                fill_round(px, w, h, &rr, r8, &solid(pal.field_border));
                let fill = if on && hover == hh { pal.hover } else { pal.bg };
                fill_round(px, w, h, &inset(&rr, one), r8 - one as f32, &solid(fill));
            }
            let cf = o(&l.confirm);
            if done {
                fill_round(px, w, h, &cf, r8, &gradient(pal.accent, pal.accent2));
            } else {
                fill_round(px, w, h, &cf, r8, &solid(pal.field_border));
            }
        } else {
            if phase == Phase::Ready
                && let Hit::Action(i) = hover
                && let Some(r) = l.rows.get(i as usize)
            {
                fill_round(px, w, h, &o(r), r8, &solid(pal.hover));
            }
            if phase == Phase::Disabled {
                let os = o(&l.open_settings);
                fill_round(px, w, h, &os, r8, &gradient(pal.accent, pal.accent2));
            }
        }
    }

    // ── 2. 文字和图标（GDI）──
    unsafe { SetBkMode(hdc, TRANSPARENT as i32) };
    let pad = scale(PAD);
    let line = DT_SINGLELINE | DT_VCENTER;
    let hdr = o(&l.header);
    let gear = o(&l.settings);
    text(
        hdc,
        &rect(
            hdr.left + pad,
            hdr.top,
            hdr.left + pad + scale(20),
            hdr.bottom,
        ),
        ICON_SPARKLE,
        pal.accent,
        fonts.sym,
        line | DT_LEFT,
    );
    text(
        hdc,
        &rect(
            hdr.left + pad + scale(22),
            hdr.top,
            gear.left - scale(8),
            hdr.bottom,
        ),
        "AI 助手",
        pal.text,
        fonts.title,
        line | DT_LEFT | DT_END_ELLIPSIS,
    );
    let icon_fg = |hh: Hit| if hover == hh { pal.text } else { pal.dim };
    text(
        hdc,
        &gear,
        ICON_SETTINGS,
        icon_fg(Hit::Settings),
        fonts.icon,
        line | DT_CENTER,
    );
    text(
        hdc,
        &o(&l.close),
        ICON_CLOSE,
        icon_fg(Hit::Close),
        fonts.icon,
        line | DT_CENTER,
    );

    // 输入卡
    text(
        hdc,
        &rect(
            sb.left + scale(12),
            sb.top + scale(9),
            sb.left + scale(30),
            sb.top + scale(29),
        ),
        ICON_SPARKLE,
        pal.accent,
        fonts.sym,
        DT_LEFT | DT_SINGLELINE,
    );
    let src_fg = match phase {
        Phase::Disabled | Phase::Loading | Phase::NoSelection => pal.dim,
        Phase::TooLong => pal.danger,
        _ if source.is_empty() => pal.dim,
        _ => pal.text,
    };
    let wrap = DT_LEFT | DT_WORDBREAK | DT_EDITCONTROL | DT_END_ELLIPSIS;
    text(
        hdc,
        &rect(
            sb.left + scale(34),
            sb.top + scale(10),
            sb.right - scale(14),
            chip.top - scale(4),
        ),
        source_line(phase, &source),
        src_fg,
        fonts.body,
        wrap,
    );
    text(
        hdc,
        &chip,
        &chip_label,
        pal.accent,
        fonts.small,
        line | DT_CENTER,
    );
    if accepts_paste(phase) {
        // 右下角「粘贴」：没读到选区时的备用入口（用户 2026-11-06 反馈 WPS 里读不到）
        let r = o(&l.paste);
        let hot = hover == Hit::Paste;
        {
            let px = unsafe { c.pixels() };
            fill_round(
                px,
                w,
                h,
                &r,
                (r.bottom - r.top) as f32 / 2.0,
                &solid(if hot { pal.hover } else { pal.field_border }),
            );
        }
        text(
            hdc,
            &r,
            "粘贴",
            if hot { pal.text } else { pal.dim },
            fonts.small,
            line | DT_CENTER,
        );
    }

    if !in_result {
        // 动作列表
        let enabled = phase == Phase::Ready;
        for (i, (icon, name, desc)) in ACTIONS.iter().enumerate() {
            let Some(row) = l.rows.get(i) else { continue };
            let r = o(row);
            let (ifg, tfg) = if enabled {
                (pal.accent, pal.text)
            } else {
                (pal.dim, pal.dim)
            };
            text(
                hdc,
                &rect(r.left + scale(14), r.top, r.left + scale(36), r.bottom),
                icon,
                ifg,
                fonts.icon,
                line | DT_LEFT,
            );
            text(
                hdc,
                &rect(r.left + scale(42), r.top, r.left + scale(86), r.bottom),
                name,
                tfg,
                fonts.bold,
                line | DT_LEFT,
            );
            text(
                hdc,
                &rect(r.left + scale(90), r.top, r.right - scale(12), r.bottom),
                desc,
                pal.dim,
                fonts.small,
                line | DT_LEFT | DT_END_ELLIPSIS,
            );
        }
        let mut nr = o(&l.note);
        if phase == Phase::Disabled {
            let os = o(&l.open_settings);
            nr.right = os.left - scale(10);
            text(
                hdc,
                &os,
                "打开设置",
                0x00FF_FFFF,
                fonts.bold,
                line | DT_CENTER,
            );
        }
        let (msg, fg) = match &note {
            Some((true, s)) => (s.as_str(), pal.ok),
            Some((false, s)) => (s.as_str(), pal.danger),
            None => (hint_line(phase), pal.dim),
        };
        text(
            hdc,
            &nr,
            msg,
            fg,
            fonts.small,
            DT_LEFT | DT_WORDBREAK | DT_EDITCONTROL,
        );
    } else {
        // 结果卡
        let lr = o(&l.result_label);
        let icon = ACTIONS
            .get(action as usize)
            .map(|a| a.0)
            .unwrap_or(ACTIONS[0].0);
        text(
            hdc,
            &rect(lr.left, lr.top, lr.left + scale(22), lr.bottom),
            icon,
            pal.accent,
            fonts.icon,
            line | DT_LEFT,
        );
        text(
            hdc,
            &rect(lr.left + scale(24), lr.top, lr.right, lr.bottom),
            &result_title(phase, action),
            pal.text,
            fonts.bold,
            line | DT_LEFT,
        );
        let rb = o(&l.result_box);
        let (body, fg) = match phase {
            Phase::Busy => ("正在生成…（大模型第一次加载要十几秒）".to_string(), pal.dim),
            Phase::Applying => ("正在写入文档…".to_string(), pal.dim),
            Phase::Error => (format!("失败：{result}"), pal.danger),
            _ => (result.clone(), pal.text),
        };
        let mut br = rect(
            rb.left + scale(12),
            rb.top + scale(10),
            rb.right - scale(12),
            rb.bottom - scale(8),
        );
        if let Some((ok, msg)) = &note {
            // 写回/复制的结果压在结果卡最下面一行
            let nr = rect(
                br.left,
                rb.bottom - scale(28),
                br.right,
                rb.bottom - scale(6),
            );
            text(
                hdc,
                &nr,
                msg,
                if *ok { pal.ok } else { pal.danger },
                fonts.small,
                line | DT_LEFT | DT_END_ELLIPSIS,
            );
            br.bottom = nr.top - scale(2);
        }
        text(hdc, &br, &body, fg, fonts.body, wrap);
        for (r, label, on) in [
            (&l.copy, "复制", done),
            (&l.regen, "重新生成", settled),
            (&l.cancel, "取消", can_cancel),
        ] {
            text(
                hdc,
                &o(r),
                label,
                if on { pal.text } else { pal.dim },
                fonts.body,
                line | DT_CENTER,
            );
        }
        let cf_fg = if done { 0x00FF_FFFF } else { pal.dim };
        text(
            hdc,
            &o(&l.confirm),
            "确定",
            cf_fg,
            fonts.bold,
            line | DT_CENTER,
        );
    }
    drop(fonts);

    // ── 3. 外形：圆角 + 渐变描边 + 阴影 ──
    let px = unsafe { c.pixels() };
    finish(px, w, h, m, scale(RADIUS) as f32, 1.6 * k, pal.shadow);
}

/// 画一帧并交给系统合成。
fn present(hwnd: HWND) -> bool {
    let m = scale(SHADOW);
    let (w, h) = outer_size();
    let Some(mut c) = (unsafe { Canvas::new(w, h) }) else {
        return false;
    };
    let dark = crate::config::snapshot().dark;
    let hover = *HOVER.lock().unwrap_or_else(|p| p.into_inner());
    paint_panel(&mut c, m, dark, hover, state_snapshot());
    let size = SIZE { cx: w, cy: h };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
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
}
/// 鼠标消息里的客户区坐标 → 面板本体坐标（减掉四周阴影边）。
fn body_xy(lp: LPARAM) -> (i32, i32) {
    let m = scale(SHADOW);
    let x = (lp & 0xFFFF) as i16 as i32;
    let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
    (x - m, y - m)
}

fn hit_at(lp: LPARAM) -> Hit {
    let (x, y) = body_xy(lp);
    let (phase, ..) = state_snapshot();
    hit(x, y, scale(BODY_W), scale(BODY_H), phase)
}

fn on_click(h: Hit) {
    match h {
        Hit::Close => crate::ai::close_panel(),
        Hit::Cancel => crate::ai::cancel_result(),
        Hit::Action(i) => crate::ai::send(i),
        Hit::Confirm => crate::ai::confirm(),
        Hit::Regenerate => crate::ai::regenerate(),
        Hit::Settings => crate::ai::open_settings(),
        Hit::Copy => {
            let (_, _, result, _, _) = state_snapshot();
            let ok = copy_to_clipboard(&result);
            crate::log_line(&format!("ai-panel: copy result -> {ok}"));
            update(|s| {
                s.note = Some(if ok {
                    (true, "结果已复制，可以粘贴到任何地方。".to_string())
                } else {
                    (
                        false,
                        "复制失败：剪贴板被别的程序占着，再点一次。".to_string(),
                    )
                });
                true
            });
        }
        Hit::Paste => {
            paste_source();
        }
        Hit::Drag | Hit::None => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            // 分层窗口正常不会收到；万一来了，验证掉区域再画一帧，免得反复来
            let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
            unsafe {
                BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
            }
            present(hwnd);
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_LBUTTONDOWN => {
            let h = hit_at(lp);
            let mut cursor = POINT { x: 0, y: 0 };
            let mut wr: RECT = unsafe { std::mem::zeroed() };
            unsafe {
                GetCursorPos(&mut cursor);
                GetWindowRect(hwnd, &mut wr);
                SetCapture(hwnd);
            }
            *PRESS.lock().unwrap_or_else(|p| p.into_inner()) = Some(Press {
                hit: h,
                cursor,
                window: POINT {
                    x: wr.left,
                    y: wr.top,
                },
            });
            DRAGGING.store(false, Ordering::Relaxed);
            0
        }
        WM_MOUSEMOVE => {
            let press = PRESS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .map(|p| (p.hit, p.cursor, p.window));
            match press {
                Some((Hit::Drag, cur0, win0)) => {
                    let mut pt = POINT { x: 0, y: 0 };
                    unsafe { GetCursorPos(&mut pt) };
                    let (dx, dy) = (pt.x - cur0.x, pt.y - cur0.y);
                    if !DRAGGING.load(Ordering::Relaxed) && (dx.abs() > 2 || dy.abs() > 2) {
                        DRAGGING.store(true, Ordering::Relaxed);
                        set_hover(Hit::None);
                    }
                    if DRAGGING.load(Ordering::Relaxed) {
                        // 尺寸必须原样传回：MoveWindow 的宽高是新尺寸，传 0 窗口就没了
                        let (w, h) = outer_size();
                        unsafe { MoveWindow(hwnd, win0.x + dx, win0.y + dy, w, h, 0) };
                    }
                }
                Some(_) => {}
                None => {
                    let h = hit_at(lp);
                    set_hover(if matches!(h, Hit::Drag | Hit::None) {
                        Hit::None
                    } else {
                        h
                    });
                }
            }
            0
        }
        WM_LBUTTONUP => {
            unsafe { ReleaseCapture() };
            let press = PRESS.lock().unwrap_or_else(|p| p.into_inner()).take();
            if DRAGGING.swap(false, Ordering::Relaxed) {
                // 存本体左上角（不含阴影边），下次打开放回这里
                let mut wr: RECT = unsafe { std::mem::zeroed() };
                unsafe { GetWindowRect(hwnd, &mut wr) };
                let m = scale(SHADOW);
                crate::state::write_panel_pos(wr.left + m, wr.top + m);
                crate::log_line(&format!(
                    "ai-panel: moved to {},{}",
                    wr.left + m,
                    wr.top + m
                ));
                return 0;
            }
            // 按下和松开在同一个按钮上才算点击（按下去又挪开 = 反悔）
            if let Some(p) = press
                && hit_at(lp) == p.hit
            {
                on_click(p.hit);
            }
            0
        }
        WM_TIMER => {
            if wp == TIMER_POLL {
                on_tick(hwnd);
            }
            0
        }
        WM_DESTROY => {
            unsafe { KillTimer(hwnd, TIMER_POLL) };
            HWND_CELL.store(0, Ordering::SeqCst);
            *PRESS.lock().unwrap_or_else(|p| p.into_inner()) = None;
            DRAGGING.store(false, Ordering::Relaxed);
            crate::bar::set_glow(false);
            crate::log_line("ai-panel: closed");
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn ensure_class() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let name = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: std::ptr::null_mut(),
            hIcon: std::ptr::null_mut(),
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
        };
        let _ = unsafe { RegisterClassW(&wc) };
    });
}
#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Phase; 9] = [
        Phase::Disabled,
        Phase::Loading,
        Phase::NoSelection,
        Phase::TooLong,
        Phase::Ready,
        Phase::Busy,
        Phase::Done,
        Phase::Applying,
        Phase::Error,
    ];

    fn body() -> (i32, i32) {
        (scale(BODY_W), scale(BODY_H))
    }

    fn mid(r: &RECT) -> (i32, i32) {
        ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
    }

    #[test]
    fn 布局_各块从上到下不重叠且都在面板里() {
        let (w, h) = body();
        let l = layout(w, h);
        assert!(l.header.bottom <= l.source_box.top);
        assert!(l.source_box.bottom <= l.rows[0].top);
        for i in 1..3 {
            assert!(l.rows[i - 1].bottom <= l.rows[i].top, "第 {i} 行");
        }
        assert!(l.rows[2].bottom <= l.note.top);
        assert!(l.note.bottom <= h);
        assert!(l.result_box.bottom <= l.confirm.top, "结果卡不能压住按钮");
        assert!(l.copy.right <= l.regen.left);
        assert!(l.regen.right <= l.cancel.left);
        assert!(l.cancel.right <= l.confirm.left);
        assert!(l.confirm.right <= w && l.confirm.bottom <= h);
        assert!(l.settings.right <= l.close.left && l.close.right <= w);
        assert!(l.chip.top >= l.source_box.top && l.chip.bottom <= l.source_box.bottom);
    }

    #[test]
    fn 命中_动作只在有选区时可点() {
        let (w, h) = body();
        let l = layout(w, h);
        for (i, r) in l.rows.iter().enumerate() {
            let (x, y) = mid(r);
            assert_eq!(hit(x, y, w, h, Phase::Ready), Hit::Action(i as u8));
            assert_eq!(
                hit(x, y, w, h, Phase::NoSelection),
                Hit::Drag,
                "没选中时当空白"
            );
            assert_eq!(hit(x, y, w, h, Phase::Disabled), Hit::Drag);
        }
    }

    #[test]
    fn 命中_结果卡按钮按状态区分() {
        let (w, h) = body();
        let l = layout(w, h);
        let at = |r: &RECT, p: Phase| {
            let (x, y) = mid(r);
            hit(x, y, w, h, p)
        };
        assert_eq!(at(&l.confirm, Phase::Done), Hit::Confirm);
        assert_eq!(at(&l.copy, Phase::Done), Hit::Copy);
        assert_eq!(at(&l.regen, Phase::Done), Hit::Regenerate);
        assert_eq!(at(&l.cancel, Phase::Done), Hit::Cancel);
        assert_eq!(
            at(&l.confirm, Phase::Error),
            Hit::Drag,
            "失败了没东西可确定"
        );
        assert_eq!(at(&l.copy, Phase::Error), Hit::Drag);
        assert_eq!(at(&l.regen, Phase::Error), Hit::Regenerate);
        assert_eq!(at(&l.cancel, Phase::Busy), Hit::Cancel, "生成中可以不等了");
        assert_eq!(at(&l.confirm, Phase::Busy), Hit::Drag);
        assert_eq!(at(&l.cancel, Phase::Applying), Hit::Drag);
    }

    #[test]
    fn 命中_关闭和设置任何状态都能点() {
        let (w, h) = body();
        let l = layout(w, h);
        for p in ALL {
            let (x, y) = mid(&l.close);
            assert_eq!(hit(x, y, w, h, p), Hit::Close, "{p:?}");
            let (x, y) = mid(&l.settings);
            assert_eq!(hit(x, y, w, h, p), Hit::Settings, "{p:?}");
        }
        let (x, y) = mid(&l.open_settings);
        assert_eq!(hit(x, y, w, h, Phase::Disabled), Hit::Settings);
    }

    #[test]
    fn 命中_空白处可以拖_面板外不算() {
        let (w, h) = body();
        assert_eq!(
            hit(scale(PAD) + 30, scale(20), w, h, Phase::Ready),
            Hit::Drag,
            "标题栏"
        );
        assert_eq!(hit(-1, -1, w, h, Phase::Ready), Hit::None, "阴影边");
        assert_eq!(hit(w, h, w, h, Phase::Ready), Hit::None);
    }

    #[test]
    fn 面板放在工作区底部正中() {
        let work = rect(0, 0, 1920, 1040);
        assert_eq!(bottom_center(&work, 460, 384, 24), (730, 632));
        let left = rect(-1920, 0, 0, 1080);
        assert_eq!(bottom_center(&left, 460, 384, 24), (-1190, 672));
        let tiny = rect(100, 50, 400, 300);
        assert_eq!(bottom_center(&tiny, 460, 384, 24), (100, 50));
    }

    #[test]
    fn 拖出屏幕的位置会被挪回来() {
        let work = rect(0, 0, 1920, 1040);
        assert_eq!(clamp_into(&work, 1800, 900, 464, 368), (1456, 672));
        assert_eq!(clamp_into(&work, -50, -20, 464, 368), (0, 0));
        assert_eq!(
            clamp_into(&work, 100, 100, 464, 368),
            (100, 100),
            "在屏幕里就不动"
        );
    }

    #[test]
    fn 只有等选区的阶段才跟着选区变() {
        for p in [
            Phase::Loading,
            Phase::NoSelection,
            Phase::TooLong,
            Phase::Ready,
        ] {
            assert!(follow_selection(p), "{p:?}");
        }
        for p in [
            Phase::Busy,
            Phase::Done,
            Phase::Applying,
            Phase::Error,
            Phase::Disabled,
        ] {
            assert!(!follow_selection(p), "{p:?}");
        }
    }

    #[test]
    fn 输入卡的提示和标签() {
        assert!(source_line(Phase::NoSelection, "").contains("选中"));
        assert_eq!(source_line(Phase::Ready, "hello"), "hello");
        assert_eq!(source_line(Phase::Ready, ""), "（没有选中文字）");
        assert_eq!(chip_text(Phase::Ready, 12), "已选中 12 字");
        assert_eq!(chip_text(Phase::NoSelection, 0), "未选中文字");
    }

    #[test]
    fn 描边渐变首尾接得上且中间是紫色() {
        let (a, b) = (border_color(0.0), border_color(1.0));
        assert!((a.0 - b.0).abs() < 0.5 && (a.1 - b.1).abs() < 0.5 && (a.2 - b.2).abs() < 0.5);
        assert_eq!(border_color(0.30), (168.0, 85.0, 247.0), "#a855f7");
    }

    #[test]
    fn 外形_角上透明_中间不透明_边上是渐变色() {
        let (w, h, m) = (80, 50, 8);
        let mut px = vec![0x00FF_FFFFu32; (w * h) as usize];
        finish(&mut px, w, h, m, 10.0, 1.6, 0.16);
        let at = |x: i32, y: i32| px[(y * w + x) as usize];
        assert_eq!(at(0, 0) >> 24, 0, "阴影边的角是透明的");
        assert_eq!(at(w / 2, h / 2), 0xFFFF_FFFF, "正中不透明、颜色不变");
        let edge = at(w / 2, m);
        assert!(edge >> 24 > 200, "上边沿不透明");
        assert_ne!(edge & 0x00FF_FFFF, 0x00FF_FFFF, "上边沿是描边色，不是白");
        let below = at(w / 2, h - m + 1) >> 24;
        assert!(below > 0 && below < 128, "下面有一点阴影：{below}");
    }

    /// 预乘像素叠到浅灰"桌面"上，写成 BMP（自上而下）。
    fn write_bmp(name: &str, px: &[u32], w: i32, h: i32) {
        let size = (w * h * 4) as usize;
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
        out.extend_from_slice(&[0u8; 24]);
        for &p in px {
            let a = p >> 24;
            let over = |c: u32| (c + 0xEE * (255 - a) / 255).min(255) as u8;
            out.push(over(p & 0xFF));
            out.push(over((p >> 8) & 0xFF));
            out.push(over((p >> 16) & 0xFF));
            out.push(255);
        }
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(name);
        std::fs::write(path, &out).expect("预览图写入失败");
    }

    /// 三个典型状态各渲一张：`tsf/target/ai-panel-ready.bmp` / `-done.bmp` / `-disabled.bmp`。
    #[test]
    fn 渲染面板预览图() {
        let m = scale(SHADOW);
        let (w, h) = outer_size();
        let cases: [(&str, Snap, Hit); 4] = [
            (
                "ai-panel-ready.bmp",
                (
                    Phase::Ready,
                    "Inklink is an open-source Chinese input method for Windows.".into(),
                    String::new(),
                    0,
                    None,
                ),
                Hit::Action(1),
            ),
            (
                "ai-panel-done.bmp",
                (
                    Phase::Done,
                    "我今天去公园玩的很开心，天气也非常的好。".into(),
                    "今日前往公园游玩，心情十分愉悦，加之天气晴朗宜人，更是锦上添花。".into(),
                    1,
                    None,
                ),
                Hit::Confirm,
            ),
            (
                "ai-panel-disabled.bmp",
                (Phase::Disabled, String::new(), String::new(), 0, None),
                Hit::None,
            ),
            (
                "ai-panel-nosel.bmp",
                (Phase::NoSelection, String::new(), String::new(), 0, None),
                Hit::Paste,
            ),
        ];
        for (name, snap, hover) in cases {
            let mut c = unsafe { Canvas::new(w, h) }.expect("画布");
            paint_panel(&mut c, m, false, hover, snap);
            let px = unsafe { c.pixels() }.to_vec();
            write_bmp(name, &px, w, h);
        }
    }
}
