//! inklink 设置 —— 六页原生小窗（输入 / 外观 / 学习 / 词库 / AI / 关于），
//! 只改 `HKCU\Software\inklink\Config` 里那几个值。
//!
//! 为什么分页：配置涨到十几项、还带着 AI 和笔记导入，挤在一页里普通用户分不清
//! 哪些该动、改完什么时候生效。每页底部都写着「生效时机」。页签只管可见性，
//! 读写注册表的数据流（`read_form` / `load_config` / `save_config`）没有变。
//!
//! 为什么用系统控件而不是自绘：浮条和候选窗自绘是因为它们要跟着主题变、还要在
//! 宿主进程里跑（不能引 UI 框架）；设置窗是独立进程、一天用一次，系统控件够用。
//! 代价是它不跟暗色 —— 这一点写进 README 比让用户猜好。
//!
//! 值的读写规则与 `tsf/src/config.rs`、`tsf/src/ai.rs` 必须一致，否则设置台和 DLL
//! 会各说各话：`Dark`/`Learn`/`AI_Enable` 是 DWORD（0/1），列数 3–9、行数 1–9、
//! 撤回窗口 0–10000，`AI_Endpoint`/`AI_Model` 是 SZ（引擎自己读那两个）。

use std::cell::RefCell;

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_FILE_NOT_FOUND, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{GetStockObject, UpdateWindow, DEFAULT_GUI_FONT, HGDIOBJ};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_DWORD, REG_EXPAND_SZ,
    REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, GetWindowTextW, IsDialogMessageW, MessageBoxW, PostQuitMessage, RegisterClassW,
    SendMessageW, SetWindowTextW, ShowWindow, TranslateMessage, BM_GETCHECK, BM_SETCHECK,
    BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_PUSHLIKE, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL,
    CB_GETLBTEXT, CB_GETLBTEXTLEN, CB_SETCURSEL, CW_USEDEFAULT, ES_AUTOHSCROLL, ES_NUMBER,
    ES_READONLY, MB_ICONERROR, MB_OK, MSG, SW_HIDE, SW_SHOW, WM_COMMAND, WM_CREATE, WM_DESTROY,
    WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_GROUP, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};

mod corpus;

const KEY_PATH: &str = r"Software\inklink\Config";

const ID_COLS: usize = 101;
const ID_ROWS: usize = 102;
const ID_DARK: usize = 103;
const ID_LEARN: usize = 104;
const ID_UNDO: usize = 105;
const ID_AI_ENABLE: usize = 106;
const ID_AI_ENDPOINT: usize = 107;
const ID_AI_MODEL: usize = 108;
const ID_AI_PROVIDER: usize = 111;
const ID_HINT: usize = 113;
const ID_AI_KEY: usize = 112;

/// 提供商：ollama（本机）| openai（任意 OpenAI 兼容 API：DeepSeek/智谱/月暗/官方）
const AI_PROVIDERS: &[&str] = &["ollama", "openai"];
const DEF_ENDPOINT_OLLAMA: &str = "http://127.0.0.1:11434";
const DEF_ENDPOINT_OPENAI: &str = "https://api.deepseek.com";
const DEF_MODEL_OPENAI: &str = "deepseek-chat";
/// 语料表路径（笔记词汇进候选用的那张表）
const ID_CORPUS: usize = 109;
const ID_IMPORT: usize = 110;
const ID_SAVE: usize = 1001;
const ID_RESET: usize = 1002;
const ID_CLOSE: usize = 1003;

const COLS_RANGE: (u32, u32) = (3, 9);
const ROWS_RANGE: (u32, u32) = (1, 9);
const UNDO_RANGE: (u32, u32) = (0, 10_000);

/// 与 `tsf/src/config.rs` 的默认值保持一致：升级安装不能改变手感。
const DEF_COLS: u32 = 5;
const DEF_ROWS: u32 = 5;
const DEF_UNDO: u32 = 2000;

/// 与引擎 `rime-server.cc` 的 `ai_config()` 默认值一致：本机 Ollama。
const DEF_ENDPOINT: &str = "http://127.0.0.1:11434";
const DEF_MODEL: &str = "qwen2.5:7b";

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

/// 页签按钮的 ID：2001..2006，顺序就是页序。
const ID_TAB_FIRST: usize = 2001;
/// 六页的标题。改顺序要连带改 `build_ui` 里每个控件挂在哪一页。
const PAGE_TITLES: [&str; 6] = ["输入", "外观", "学习", "词库", "AI", "关于"];
const PAGE_COUNT: usize = PAGE_TITLES.len();
const PAGE_INPUT: usize = 0;
const PAGE_LOOK: usize = 1;
const PAGE_LEARN: usize = 2;
const PAGE_CORPUS: usize = 3;
const PAGE_AI: usize = 4;
const PAGE_ABOUT: usize = 5;

/// 客户区大小：按控件最多的 AI 页定，切页不改窗口大小。
const CLIENT_W: i32 = 600;
const CLIENT_H: i32 = 480;
/// 页内容的左边距和宽度；每页「生效时机」都在同一个高度，切页时好对照。
const X0: i32 = 24;
const PAGE_W: i32 = CLIENT_W - 2 * X0;
const Y_WHEN: i32 = 352;

const PROJECT_URL: &str = "https://github.com/MeloFang/inklink";
const DIAGNOSE_PATH: &str = r"C:\Program Files\inklink\diagnose.cmd";

/// 页签按钮 ID → 页序；不是页签就 `None`。
fn page_of_tab(id: usize) -> Option<usize> {
    id.checked_sub(ID_TAB_FIRST).filter(|p| *p < PAGE_COUNT)
}

#[derive(Default, Clone)]
struct Ui {
    cols: HWND,
    rows: HWND,
    dark: HWND,
    learn: HWND,
    undo: HWND,
    ai_enable: HWND,
    ai_endpoint: HWND,
    ai_model: HWND,
    ai_provider: HWND,
    ai_key: HWND,
    hint: HWND,
    corpus: HWND,
    status: HWND,
    /// 顶部六个页签（下标 = 页序）
    tabs: Vec<HWND>,
    /// 每页的控件（下标 = 页序）。建窗时一次全建好，切页只改可见性，
    /// 所以 `read_form` 不管当前停在哪一页，都能读到六页的全部值。
    pages: [Vec<HWND>; PAGE_COUNT],
}
/// 词库页的生效时机：导入成功后设置窗会自己重启引擎（见 `import_and_reload`）。
const CORPUS_WHEN: &str =
    "生效时机：立即。导入成功后会自动重启输入引擎（几秒），之后打字就能出现笔记里的词；那一下正在打的拼音会被打断。";

/// 把界面上填的值收成配置。返回 `Err` 就弹提示、不写注册表。
fn read_form(ui: &Ui) -> Result<Config, String> {
    let cols = parse_in(ui.cols, "每页候选的列数", COLS_RANGE)?;
    let rows = parse_in(ui.rows, "每页候选的行数", ROWS_RANGE)?;
    let undo = parse_in(ui.undo, "撤回窗口毫秒数", UNDO_RANGE)?;
    Ok(Config {
        dark: checked(ui.dark),
        learn: checked(ui.learn),
        cols,
        rows,
        undo_ms: undo,
        ai_enabled: checked(ui.ai_enable),
        ai_endpoint: endpoint_or_default(&text_of(ui.ai_endpoint)),
        ai_model: model_or_default(&text_of(ui.ai_model)),
        ai_provider: AI_PROVIDERS
            .iter()
            .find(|p| **p == combo_sel(ui.ai_provider))
            .map(|p| p.to_string())
            .unwrap_or_else(|| "ollama".to_string()),
        ai_key: text_of(ui.ai_key).trim().to_string(),
        hint: checked(ui.hint),
        // 笔记目录可以留空（= 导入脚本用内置默认）
        corpus_path: text_of(ui.corpus).trim().to_string(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Config {
    dark: bool,
    learn: bool,
    cols: u32,
    rows: u32,
    undo_ms: u32,
    ai_enabled: bool,
    ai_endpoint: String,
    ai_model: String,
    /// ollama | openai
    ai_provider: String,
    /// openai 模式的 API key；ollama 忽略
    ai_key: String,
    /// 联想条开关（缺失=开）
    hint: bool,
    /// 笔记目录（Obsidian 库）；空 = 导入脚本用内置默认
    corpus_path: String,
}

/// 地址留空就当默认值。**不能把空串写进注册表**：引擎那边空串也会退回默认，
/// 但设置窗再打开时输入框会空着，用户会以为坏了。
fn endpoint_or_default(s: &str) -> String {
    let t = s.trim().trim_end_matches('/');
    if t.is_empty() {
        DEF_ENDPOINT.to_string()
    } else {
        t.to_string()
    }
}

fn model_or_default(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        DEF_MODEL.to_string()
    } else {
        t.to_string()
    }
}

/// 实体表的默认位置（%LOCALAPPDATA%\inklink\entities.tsv）。
fn dirs_entities() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("inklink").join("entities.tsv")
}

/// 语料表的默认位置（%LOCALAPPDATA%\inklink\corpus.tsv）。
fn dirs_corpus() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    base.join("inklink").join("corpus.tsv")
}

/// 下拉框当前选中的文本（没有选中则空串）。
fn combo_sel(h: HWND) -> String {
    let idx = unsafe { SendMessageW(h, CB_GETCURSEL as u32, 0, 0) };
    if idx < 0 {
        return String::new();
    }
    let len = unsafe { SendMessageW(h, CB_GETLBTEXTLEN as u32, idx as usize, 0) } as usize;
    if len == 0 || len > 64 {
        return String::new();
    }
    let mut buf = vec![0u16; len + 1];
    unsafe {
        SendMessageW(
            h,
            CB_GETLBTEXT as u32,
            idx as usize,
            buf.as_mut_ptr() as isize,
        )
    };
    String::from_utf16_lossy(&buf[..len])
}

fn checked(h: HWND) -> bool {
    unsafe { SendMessageW(h, BM_GETCHECK as u32, 0, 0) == 1 }
}

fn text_of(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) };
    let n = if n < 0 { 0 } else { n as usize };
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

/// 解析并夹到合法区间。空白直接当默认值（用户清空输入框是常见操作）。
fn parse_in(h: HWND, what: &str, range: (u32, u32)) -> Result<u32, String> {
    parse_clamped(&text_of(h), range)
        .ok_or_else(|| format!("{what} 要在 {} 到 {} 之间。", range.0, range.1))
}

/// 空白 → `None` 由调用方决定；这里只做"能不能解析成区间内的数"。
/// 抽成纯函数是为了能单测：UI 逻辑没法在这里跑。
fn parse_clamped(s: &str, range: (u32, u32)) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let v: u32 = t.parse().ok()?;
    if v < range.0 || v > range.1 {
        return None;
    }
    Some(v)
}

// ── 注册表 ─────────────────────────────────────────────────────

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn open_key(access: u32) -> Option<HKEY> {
    let path = wide(KEY_PATH);
    let mut h: HKEY = std::ptr::null_mut();
    let rc = RegCreateKeyExW(
        HKEY_CURRENT_USER,
        path.as_ptr(),
        0,
        std::ptr::null_mut(),
        REG_OPTION_NON_VOLATILE,
        access,
        std::ptr::null_mut(),
        &mut h,
        std::ptr::null_mut(),
    );
    if rc == 0 {
        Some(h)
    } else {
        None
    }
}

unsafe fn read_dword(h: HKEY, name: &str) -> Option<u32> {
    let n = wide(name);
    let mut ty = 0u32;
    let mut val = 0u32;
    let mut cb = 4u32;
    let rc = RegQueryValueExW(
        h,
        n.as_ptr(),
        std::ptr::null_mut(),
        &mut ty,
        &mut val as *mut u32 as *mut u8,
        &mut cb,
    );
    if rc == 0 && ty == REG_DWORD {
        Some(val)
    } else {
        None
    }
}

unsafe fn write_dword(h: HKEY, name: &str, v: u32) -> bool {
    let n = wide(name);
    RegSetValueExW(
        h,
        n.as_ptr(),
        0,
        REG_DWORD,
        &v as *const u32 as *const u8,
        4,
    ) == 0
}

unsafe fn read_sz(h: HKEY, name: &str) -> Option<String> {
    let n = wide(name);
    let mut ty = 0u32;
    let mut buf = [0u16; 512];
    let mut cb = (buf.len() * 2) as u32;
    let rc = RegQueryValueExW(
        h,
        n.as_ptr(),
        std::ptr::null_mut(),
        &mut ty,
        buf.as_mut_ptr() as *mut u8,
        &mut cb,
    );
    if rc != 0 || (ty != REG_SZ && ty != REG_EXPAND_SZ) {
        return None;
    }
    let len = ((cb as usize / 2).saturating_sub(1)).min(buf.len());
    let s: String = String::from_utf16_lossy(&buf[..len]);
    let s = s.trim_end_matches('\0').trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

unsafe fn write_sz(h: HKEY, name: &str, v: &str) -> bool {
    let n = wide(name);
    let data: Vec<u16> = v.encode_utf16().chain(std::iter::once(0)).collect();
    RegSetValueExW(
        h,
        n.as_ptr(),
        0,
        REG_SZ,
        data.as_ptr() as *const u8,
        (data.len() * 2) as u32,
    ) == 0
}

fn default_config() -> Config {
    Config {
        dark: false,
        learn: true,
        cols: DEF_COLS,
        rows: DEF_ROWS,
        undo_ms: DEF_UNDO,
        ai_enabled: false,
        ai_endpoint: DEF_ENDPOINT.to_string(),
        ai_model: DEF_MODEL.to_string(),
        ai_provider: "ollama".to_string(),
        ai_key: String::new(),
        hint: false,
        corpus_path: String::new(),
    }
}

fn load_config() -> Config {
    unsafe {
        let Some(h) = open_key(KEY_READ) else {
            return default_config();
        };
        // 缺省语义必须和 DLL 一致：Dark/AI_Enable 缺失=关（只有非 0 才算开），
        // Learn 缺失=开（只有明确写 0 才算关）。
        let dark = matches!(read_dword(h, "Dark"), Some(v) if v != 0);
        let learn = !matches!(read_dword(h, "Learn"), Some(0));
        let cols = read_dword(h, "Cols")
            .filter(|v| *v >= COLS_RANGE.0 && *v <= COLS_RANGE.1)
            .unwrap_or(DEF_COLS);
        let rows = read_dword(h, "Rows")
            .filter(|v| *v >= ROWS_RANGE.0 && *v <= ROWS_RANGE.1)
            .unwrap_or(DEF_ROWS);
        let undo = read_dword(h, "UndoMs")
            .filter(|v| *v <= UNDO_RANGE.1)
            .unwrap_or(DEF_UNDO);
        let ai_enabled = matches!(read_dword(h, "AI_Enable"), Some(v) if v != 0);
        let ai_endpoint = read_sz(h, "AI_Endpoint").unwrap_or_else(|| DEF_ENDPOINT.to_string());
        let ai_model = read_sz(h, "AI_Model").unwrap_or_else(|| DEF_MODEL.to_string());
        let ai_provider = read_sz(h, "AI_Provider").unwrap_or_else(|| "ollama".to_string());
        let ai_key = read_sz(h, "AI_ApiKey").unwrap_or_default();
        let hint = matches!(read_dword(h, "Hint"), Some(v) if v != 0);
        let corpus_path = read_sz(h, "NotesDir").unwrap_or_default();
        RegCloseKey(h);
        Config {
            dark,
            learn,
            cols,
            rows,
            undo_ms: undo,
            ai_enabled,
            ai_endpoint,
            ai_model,
            ai_provider,
            ai_key,
            hint,
            corpus_path,
        }
    }
}

fn save_config(c: Config) -> bool {
    unsafe {
        let Some(h) = open_key(KEY_READ | KEY_WRITE) else {
            return false;
        };
        let ok = write_dword(h, "Dark", if c.dark { 1 } else { 0 })
            && write_dword(h, "Learn", if c.learn { 1 } else { 0 })
            && write_dword(h, "Cols", c.cols)
            && write_dword(h, "Rows", c.rows)
            && write_dword(h, "UndoMs", c.undo_ms)
            && write_dword(h, "AI_Enable", if c.ai_enabled { 1 } else { 0 })
            && write_sz(h, "AI_Endpoint", &c.ai_endpoint)
            && write_sz(h, "AI_Model", &c.ai_model)
            && write_sz(h, "AI_Provider", &c.ai_provider)
            && write_sz(h, "AI_ApiKey", &c.ai_key)
            && write_dword(h, "Hint", if c.hint { 1 } else { 0 })
            && write_sz(h, "NotesDir", &c.corpus_path);
        RegCloseKey(h);
        ok
    }
}

// ── 窗口 ───────────────────────────────────────────────────────

fn wide_static(s: &str) -> &'static [u16] {
    // 控件文字要在窗口生命周期内一直有效；泄漏几个几十字节的 Vec 比管生命周期划算。
    Box::leak(wide(s).into_boxed_slice())
}

unsafe fn child(
    class: &str,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    parent: HWND,
    id: usize,
) -> HWND {
    let cls = wide(class);
    CreateWindowExW(
        0,
        cls.as_ptr(),
        wide_static(text).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        x,
        y,
        w,
        h,
        parent,
        id as *mut core::ffi::c_void,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    )
}

unsafe fn set_text(h: HWND, s: &str) {
    SetWindowTextW(h, wide_static(s).as_ptr());
}

unsafe fn set_check(h: HWND, on: bool) {
    SendMessageW(h, BM_SETCHECK, on as usize, 0);
}

/// 把一份配置填进六页的控件。建窗（读注册表）和「恢复默认」（出厂值）共用这一个，
/// 这样两条路不会漏掉某一页；末尾同步 AI 四项的可用状态。
unsafe fn fill_form(ui: &Ui, c: &Config) {
    set_text(ui.cols, &c.cols.to_string());
    set_text(ui.rows, &c.rows.to_string());
    set_check(ui.hint, c.hint);
    set_check(ui.dark, c.dark);
    set_check(ui.learn, c.learn);
    set_text(ui.undo, &c.undo_ms.to_string());
    set_text(ui.corpus, &c.corpus_path);
    set_check(ui.ai_enable, c.ai_enabled);
    let sel = AI_PROVIDERS
        .iter()
        .position(|p| *p == c.ai_provider)
        .unwrap_or(0);
    SendMessageW(ui.ai_provider, CB_SETCURSEL, sel, 0);
    set_text(ui.ai_endpoint, &c.ai_endpoint);
    set_text(ui.ai_model, &c.ai_model);
    set_text(ui.ai_key, &c.ai_key);
    apply_ai_enabled(
        ui.ai_enable,
        ui.ai_provider,
        ui.ai_endpoint,
        ui.ai_model,
        ui.ai_key,
    );
}

/// 只显示第 `page` 页的控件，其余页全部隐藏；页签按下的状态跟着走。
/// 隐藏的控件拿不到焦点，Tab 顺序自然只在当前页里转。
unsafe fn show_page(ui: &Ui, page: usize) {
    for (i, group) in ui.pages.iter().enumerate() {
        let cmd = if i == page { SW_SHOW } else { SW_HIDE };
        for h in group {
            ShowWindow(*h, cmd);
        }
    }
    for (i, t) in ui.tabs.iter().enumerate() {
        set_check(*t, i == page);
    }
}

unsafe fn build_ui(hwnd: HWND) {
    let font = GetStockObject(DEFAULT_GUI_FONT) as HGDIOBJ;
    // 每个控件建好就记进它所属的页；页签、分隔线、状态行、底部按钮不属于任何页（None）
    let pages: RefCell<[Vec<HWND>; PAGE_COUNT]> = RefCell::new(Default::default());
    let make = |page: Option<usize>,
                class: &str,
                text: &str,
                style: u32,
                (x, y, w, h): (i32, i32, i32, i32),
                id: usize|
     -> HWND {
        let c = child(class, text, style, x, y, w, h, hwnd, id);
        SendMessageW(c, 0x0030 /* WM_SETFONT */, font as usize, 1);
        if let Some(p) = page {
            pages.borrow_mut()[p].push(c);
        }
        c
    };
    let label =
        |p: usize, t: &str, x: i32, y: i32, w: i32| make(Some(p), "STATIC", t, 0, (x, y, w, 18), 0);
    // 两行高的说明（STATIC 默认自动折行）
    let para = |p: usize, t: &str, y: i32| make(Some(p), "STATIC", t, 0, (X0, y, PAGE_W, 36), 0);
    let when = |p: usize, t: &str| para(p, t, Y_WHEN);
    let num = |p: usize, x: i32, y: i32, w: i32, id: usize| {
        let style = WS_BORDER | WS_TABSTOP | ES_NUMBER as u32;
        make(Some(p), "EDIT", "", style, (x, y, w, 22), id)
    };
    // 文本输入框：地址、模型名、目录是字符串，不能带 ES_NUMBER，长内容要能横向滚动
    let text = |p: usize, x: i32, y: i32, w: i32, id: usize| {
        let style = WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32;
        make(Some(p), "EDIT", "", style, (x, y, w, 22), id)
    };
    let check = |p: usize, t: &str, y: i32, id: usize| {
        let style = WS_TABSTOP | BS_AUTOCHECKBOX as u32;
        make(Some(p), "BUTTON", t, style, (X0, y, PAGE_W, 20), id)
    };
    // 只读、无边框的输入框：看着像一行字，但能选中复制（路径、网址）
    let copyable = |p: usize, t: &str, y: i32| {
        let style = (ES_READONLY | ES_AUTOHSCROLL) as u32;
        make(Some(p), "EDIT", t, style, (X0 + 18, y, PAGE_W - 18, 20), 0)
    };
    let button = |page: Option<usize>, t: &str, x: i32, y: i32, w: i32, id: usize| {
        make(page, "BUTTON", t, WS_TABSTOP, (x, y, w, 26), id)
    };

    // ── 页签 ──────────────────────────────────────────────
    // 不用 comctl32 的 TabCtrl（要 InitCommonControlsEx + manifest）：一排
    // BS_PUSHLIKE 的单选按钮就是页签，按下去的那个是当前页。
    let mut tabs = Vec::with_capacity(PAGE_COUNT);
    for (i, t) in PAGE_TITLES.iter().enumerate() {
        // 第一个带 WS_GROUP + WS_TABSTOP：Tab 键进到页签这一组，方向键在组内切页
        let first = if i == 0 { WS_GROUP | WS_TABSTOP } else { 0 };
        let style = first | (BS_AUTORADIOBUTTON | BS_PUSHLIKE) as u32;
        let x = 12 + i as i32 * 96;
        tabs.push(make(
            None,
            "BUTTON",
            t,
            style,
            (x, 12, 90, 28),
            ID_TAB_FIRST + i,
        ));
    }
    // 分隔线（SS_ETCHEDHORZ = 0x10）。带 WS_GROUP：页签那一组到这里为止
    make(
        None,
        "STATIC",
        "",
        WS_GROUP | 0x10,
        (12, 50, CLIENT_W - 24, 2),
        0,
    );
    // ── 1 输入 ────────────────────────────────────────────
    let p = PAGE_INPUT;
    label(p, "每页候选", X0, 72, 70);
    let cols = num(p, X0 + 76, 68, 44, ID_COLS);
    label(p, "列 ×", X0 + 128, 72, 34);
    let rows = num(p, X0 + 166, 68, 44, ID_ROWS);
    label(p, "行", X0 + 218, 72, 24);
    label(
        p,
        "列数 3–9，行数 1–9。收起时只显示 1 行，展开后显示这里设的行数。",
        X0,
        100,
        PAGE_W,
    );
    let hint = check(
        p,
        "联想条（上屏一个词后，在下方给出后面常接的词）",
        136,
        ID_HINT,
    );
    label(
        p,
        "默认关：联想条会挡在光标下方，只想安静打字就不用开。",
        X0 + 18,
        160,
        PAGE_W - 18,
    );
    when(
        p,
        "生效时机：立即。点「保存」后，下一次按键、下一次上屏就按新设置来，不用重启。",
    );

    // ── 2 外观 ────────────────────────────────────────────
    let p = PAGE_LOOK;
    let dark = check(p, "暗色主题（候选窗、浮条、☰ 菜单一起变）", 70, ID_DARK);
    label(
        p,
        "这个设置窗口本身用的是系统控件，不跟着变暗。",
        X0 + 18,
        94,
        PAGE_W - 18,
    );
    when(
        p,
        "生效时机：立即。点「保存」后，候选窗、浮条、菜单下一次重绘时换成新主题。",
    );

    // ── 3 学习 ────────────────────────────────────────────
    let p = PAGE_LEARN;
    let learn = check(
        p,
        "从我的输入里学词（关掉就完全不记，只查不学）",
        70,
        ID_LEARN,
    );
    label(
        p,
        "误选撤回窗口（毫秒；0 = 关掉撤回，最大 10000）",
        X0,
        112,
        PAGE_W,
    );
    let undo = num(p, X0, 134, 80, ID_UNDO);
    para(
        p,
        "上屏之后在这段时间内用退格把刚打出来的字删光，输入法就当作选错了，这次的选择不会被记住。",
        166,
    );
    when(
        p,
        "生效时机：立即。点「保存」后，下一次选词、下一次退格就按新设置来。",
    );

    // ── 4 词库 ────────────────────────────────────────────
    let p = PAGE_CORPUS;
    label(p, "笔记目录（Obsidian 库；留空 = 不启用）", X0, 72, PAGE_W);
    let corpus = text(p, X0, 94, PAGE_W - 116, ID_CORPUS);
    button(Some(p), "立即导入", X0 + PAGE_W - 104, 92, 104, ID_IMPORT);
    para(
        p,
        "导入会把笔记里的词汇整理成语料表（%LOCALAPPDATA%\\inklink\\corpus.tsv，只存在本机），让这些词能出现在候选里。",
        130,
    );
    label(
        p,
        "点「保存」时如果填了目录，也会顺手导入一次。导入结果显示在窗口下方。",
        X0,
        172,
        PAGE_W,
    );
    when(p, CORPUS_WHEN);

    // ── 5 AI ──────────────────────────────────────────────
    let p = PAGE_AI;
    label(
        p,
        "选中一段文字，再点浮条 ☰ 里的「翻译 / 改写 / 续写」。",
        X0,
        70,
        PAGE_W,
    );
    let ai_enable = check(p, "启用 AI（会连到你填的接口地址）", 94, ID_AI_ENABLE);
    label(p, "提供商", X0, 130, 66);
    let ai_provider = make(
        Some(p),
        "COMBOBOX",
        "",
        0x0003 | WS_TABSTOP, // CBS_DROPDOWNLIST
        (X0 + 70, 126, 160, 200),
        ID_AI_PROVIDER,
    );
    for pr in AI_PROVIDERS {
        SendMessageW(
            ai_provider,
            CB_ADDSTRING,
            0,
            wide_static(pr).as_ptr() as isize,
        );
    }
    label(
        p,
        "ollama = 本机的 Ollama；openai = 任意 OpenAI 兼容接口（DeepSeek、智谱、月之暗面、OpenAI 官方）",
        X0,
        158,
        PAGE_W,
    );
    label(p, "接口地址", X0, 192, 66);
    let ai_endpoint = text(p, X0 + 70, 188, PAGE_W - 70, ID_AI_ENDPOINT);
    label(p, "模型名", X0, 224, 66);
    let ai_model = text(p, X0 + 70, 220, PAGE_W - 70, ID_AI_MODEL);
    label(p, "API Key", X0, 256, 66);
    let ai_key = text(p, X0 + 70, 252, PAGE_W - 70, ID_AI_KEY);
    label(
        p,
        "ollama 不用填。Key 保存在本机注册表（HKCU）里。",
        X0 + 70,
        280,
        PAGE_W - 70,
    );
    when(
        p,
        "生效时机：下一次 AI 请求。点「保存」后，下次在浮条里点翻译 / 改写 / 续写就用新设置。",
    );

    // ── 6 关于 ────────────────────────────────────────────
    let p = PAGE_ABOUT;
    label(
        p,
        &format!("inklink {}", env!("CARGO_PKG_VERSION")),
        X0,
        72,
        PAGE_W,
    );
    label(p, "Windows 上的开源中文输入法。", X0, 96, PAGE_W);
    label(
        p,
        "许可证：GPL-3.0（基础词库衍生自 rime-ice，同为 GPL-3.0）",
        X0,
        130,
        PAGE_W,
    );
    para(
        p,
        "诊断工具：出问题时双击它，会在桌面生成 inklink-诊断-<时间>.txt，把这个文件发给开发者。",
        164,
    );
    copyable(p, DIAGNOSE_PATH, 202);
    label(p, "项目主页", X0, 236, PAGE_W);
    copyable(p, PROJECT_URL, 258);

    // ── 底部：所有页共用 ──────────────────────────────────
    make(None, "STATIC", "", 0x10, (12, 398, CLIENT_W - 24, 2), 0);
    let status = make(
        None,
        "STATIC",
        "改完点「保存」。每页底部写着改完什么时候生效。",
        0,
        (X0, 404, PAGE_W, 32),
        0,
    );
    button(None, "保存", X0, 440, 84, ID_SAVE);
    button(None, "恢复默认", X0 + 92, 440, 96, ID_RESET);
    button(None, "关闭", X0 + PAGE_W - 84, 440, 84, ID_CLOSE);

    let ui = Ui {
        cols,
        rows,
        dark,
        learn,
        undo,
        ai_enable,
        ai_endpoint,
        ai_model,
        ai_provider,
        ai_key,
        hint,
        corpus,
        status,
        tabs,
        pages: pages.into_inner(),
    };
    fill_form(&ui, &load_config());
    show_page(&ui, PAGE_INPUT);
    UI.with(|u| *u.borrow_mut() = ui);
}
// ── 导入后重启引擎（任务 C，方案 1）────────────────────────────
//
// 引擎只在启动时读 corpus.tsv / entities.tsv，导入完不重启就等于没导。
// 这里结束旧引擎后**立刻自己拉起新的**，不留给 DLL 的 ensure_engine：那条路是在
// 宿主应用的 UI 线程里拉引擎，最多要等 2 秒，会让用户打下一个字时卡一下。

/// 引擎管道名，和 `tsf/src/ipc.rs` 的 `PIPE_NAME` 一致。
const ENGINE_PIPE: &str = r"\\.\pipe\inklink-engine";
/// 和 `tsf/src/ipc.rs` 的 `engine_exe_path()` 同一个回退位置。
const ENGINE_FALLBACK: &str = r"C:\inklink-engine\rime-server.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 引擎可执行文件：`HKLM\SOFTWARE\inklink\EnginePath` > 默认安装位置（与 DLL 同一规则）。
fn engine_exe_path() -> Option<std::path::PathBuf> {
    let key = wide(r"SOFTWARE\inklink");
    let value = wide("EnginePath");
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: 标准注册表读取，缓冲区大小按字节给出
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
        )
    };
    if rc == 0 {
        let len = (size as usize / 2).saturating_sub(1).min(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        if !s.trim().is_empty() {
            return Some(std::path::PathBuf::from(s.trim()));
        }
    }
    let p = std::path::Path::new(ENGINE_FALLBACK);
    p.exists().then(|| p.to_path_buf())
}

/// 管道还在不在（有实例就算在，哪怕实例都忙）。
fn engine_pipe_exists() -> bool {
    let name = wide(ENGINE_PIPE);
    // SAFETY: 纯查询；1ms 是为了不落到 0 = "用管道的默认超时"
    unsafe { WaitNamedPipeW(name.as_ptr(), 1) != 0 || GetLastError() != ERROR_FILE_NOT_FOUND }
}

fn wait_until(limit: std::time::Duration, cond: impl Fn() -> bool) -> bool {
    let start = std::time::Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// 结束旧引擎、拉起新引擎，返回给状态行的一句话。
fn restart_engine() -> String {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let Some(exe) = engine_exe_path() else {
        return "没找到输入引擎（rime-server.exe），新词要等引擎下次启动才生效。".into();
    };
    // /F：引擎没有"请退出"的报文。它写词库都是先写临时文件再换名，强杀不会留下半个文件
    let _ = Command::new("taskkill")
        .args(["/IM", "rime-server.exe", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    // 等旧进程的管道消失（单例互斥量随进程一起释放），否则新进程会以为"已有实例"直接退出
    if !wait_until(std::time::Duration::from_secs(3), || !engine_pipe_exists()) {
        return "旧的输入引擎没能结束（可能是权限不够），新词要等重启电脑后生效。".into();
    }
    // 两次机会：旧进程刚死、互斥量还没完全释放时，新进程会自己退出，再拉一次就好
    for _ in 0..2 {
        let child = Command::new(&exe)
            .arg("--serve")
            .current_dir(exe.parent().unwrap_or(std::path::Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => return format!("输入引擎没能重启（{e}），下一次按键会自动再拉起。"),
        };
        let up = wait_until(std::time::Duration::from_secs(5), engine_pipe_exists);
        if up {
            return "输入引擎已重启，现在打字就能打出笔记里的词。".into();
        }
        if !matches!(child.try_wait(), Ok(Some(_))) {
            // 进程还活着，只是 librime 初始化慢（首次部署会多花几秒）
            return "输入引擎正在重启，过几秒再打字就能打出笔记里的词。".into();
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    "输入引擎没能重启，下一次按键会自动再拉起。".into()
}

/// 语料相关的三个文件此刻的内容（读不到算空）。用来判断导入有没有真的改变什么。
fn corpus_snapshot() -> Vec<Vec<u8>> {
    let ent = dirs_entities();
    let next = ent.with_file_name("entities-next.tsv");
    [dirs_corpus(), ent, next]
        .iter()
        .map(|p| std::fs::read(p).unwrap_or_default())
        .collect()
}

/// 导入，导入成功且语料确实变了就重启引擎，让新词立刻生效。
///
/// 语料没变（比如只改了列数又点了保存）就不重启：重启会打断正在打的拼音，没必要。
fn import_and_reload(notes: &str, exe_dir: &std::path::Path) -> String {
    let before = corpus_snapshot();
    let msg = import_all(notes, exe_dir);
    if msg.starts_with("导入失败") {
        return msg;
    }
    if corpus_snapshot() == before {
        return format!("{msg}语料没有变化，不用重启引擎。");
    }
    set_status_now(&format!("{msg}正在重启输入引擎…"));
    format!("{msg}{}", restart_engine())
}

/// 改状态行并立刻重绘：导入和重启引擎期间消息循环是停着的，不手动刷就看不到进度。
fn set_status_now(msg: &str) {
    UI.with(|u| unsafe {
        let h = u.borrow().status;
        set_text(h, msg);
        UpdateWindow(h);
    });
}
/// 只导入、不保存设置：给用户一个"我现在就想生成"的按钮。
fn on_import() {
    let ui = UI.with(|u| u.borrow().to_snapshot());
    let notes = text_of(ui.corpus).trim().to_string();
    if notes.is_empty() {
        set_status("先在上面填笔记目录，再点导入。");
        return;
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|x| x.to_path_buf()))
        .unwrap_or_default();
    set_status_now("正在导入笔记…");
    let msg = import_and_reload(&notes, &exe_dir);
    set_status(&msg);
}

/// 生成语料表 + 实体表，返回一句人话（状态行直接显示）。
///
/// 实体是 M3 的东西：抽不到不算失败（笔记里没有 `负责人：` 这类结构很正常），
/// 只在有结果时附在后面。
fn import_all(notes: &str, exe_dir: &std::path::Path) -> String {
    let dir = std::path::Path::new(notes);
    let corpus_out = dirs_corpus();
    let mut msg = match corpus::rebuild(dir, &corpus_out, exe_dir) {
        Ok(r) => format!("导入完成：{} 篇笔记 / {} 条语料", r.files, r.entries),
        Err(e) => return format!("导入失败：{e}"),
    };
    let entities_out = dirs_entities();
    if let Ok(r) = corpus::extract_entities(dir, &entities_out, exe_dir) {
        if r.entries > 0 {
            msg.push_str(&format!(" / {} 个实体", r.entries));
        }
        if r.next_edges > 0 {
            msg.push_str(&format!(" / {} 条关联", r.next_edges));
        }
    }
    msg.push('。');
    msg
}

fn set_status(msg: &str) {
    UI.with(|u| unsafe { set_text(u.borrow().status, msg) });
}

fn on_save(hwnd: HWND) {
    let ui = UI.with(|u| u.borrow().to_snapshot());
    match read_form(&ui) {
        Ok(c) => {
            let notes = c.corpus_path.clone();
            if save_config(c) {
                if notes.trim().is_empty() {
                    UI.with(|u| unsafe {
                        set_text(u.borrow().status, "已保存 —— 下一次按键生效。")
                    });
                } else {
                    // 顺手把笔记目录转成语料表 + 实体表：用户只要填目录，不用再跑脚本
                    let exe_dir = std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|x| x.to_path_buf()))
                        .unwrap_or_default();
                    set_status_now("正在导入笔记…");
                    let msg = import_and_reload(&notes, &exe_dir);
                    UI.with(|u| unsafe { set_text(u.borrow().status, &msg) });
                }
            } else {
                unsafe {
                    let msg = wide("写注册表失败。\n\n可能是权限问题，试试以管理员身份运行。");
                    MessageBoxW(
                        hwnd,
                        msg.as_ptr(),
                        wide("inklink").as_ptr(),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
        }
        Err(e) => unsafe {
            let msg = wide(&e);
            MessageBoxW(
                hwnd,
                msg.as_ptr(),
                wide("inklink").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        },
    }
}

impl Ui {
    fn to_snapshot(&self) -> Ui {
        self.clone()
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            build_ui(hwnd);
            0
        }
        WM_COMMAND => {
            match (wp & 0xFFFF) as usize {
                ID_SAVE => on_save(hwnd),
                ID_RESET => {
                    // 出厂值用 default_config()，和首次安装同一个来源：六页一起还原，
                    // AI 四项跟着回到禁用态（fill_form 末尾会刷 apply_ai_enabled）
                    let ui = UI.with(|u| u.borrow().to_snapshot());
                    fill_form(&ui, &default_config());
                    set_text(ui.status, "已填回默认值 —— 还要点一次“保存”才写进去。");
                }
                ID_CLOSE => {
                    DestroyWindow(hwnd);
                }
                ID_AI_ENABLE => {
                    // 勾上/取消都立刻反映到输入框的可用状态
                    let ui = UI.with(|u| u.borrow().to_snapshot());
                    apply_ai_enabled(
                        ui.ai_enable,
                        ui.ai_provider,
                        ui.ai_endpoint,
                        ui.ai_model,
                        ui.ai_key,
                    );
                }
                ID_IMPORT => on_import(),
                id => {
                    if let Some(page) = page_of_tab(id) {
                        let ui = UI.with(|u| u.borrow().to_snapshot());
                        show_page(&ui, page);
                    }
                }
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// windows-sys 0.61 的 WindowsAndMessaging 里没有 EnableWindow（被裁掉了），
// 自己声明一次：只是 user32 的一个普通导出，签名和 Win32 文档一致。
#[link(name = "user32")]
extern "system" {
    fn EnableWindow(hwnd: HWND, enable: i32) -> i32;
}

fn enable_window(h: HWND, on: i32) {
    unsafe {
        EnableWindow(h, on);
    }
}

/// 按「启用 AI」的勾选状态决定接口/模型/Key 能不能编辑。
///
/// 默认未勾选就全部灰掉：只想当输入法用的人看到的只是一行复选框，
/// 不会以为「必须先填接口才能打字」。
fn apply_ai_enabled(enable: HWND, provider: HWND, endpoint: HWND, model: HWND, key: HWND) {
    let on = checked(enable);
    for h in [provider, endpoint, model, key] {
        unsafe { enable_window(h, if on { 1 } else { 0 }) };
    }
}

fn main() {
    // 命令行模式：`inklink-settings.exe --import <笔记目录>`
    // 给自动化/排错用 —— 不开窗，直接跑一遍抽取并把结果打到 stdout。
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--import") {
        let Some(dir) = args.get(i + 1) else {
            eprintln!("用法：inklink-settings.exe --import <笔记目录>");
            std::process::exit(2);
        };
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|x| x.to_path_buf()))
            .unwrap_or_default();
        let msg = import_all(dir, &exe_dir);
        println!("{msg}");
        std::process::exit(if msg.starts_with("导入失败") {
            1
        } else {
            0
        });
    }
    unsafe {
        let cls = wide("inklink-settings");
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: std::ptr::null_mut(),
            hIcon: std::ptr::null_mut(),
            hCursor: windows_sys::Win32::UI::WindowsAndMessaging::LoadCursorW(
                std::ptr::null_mut(),
                windows_sys::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            ),
            hbrBackground: (windows_sys::Win32::Graphics::Gdi::COLOR_WINDOW + 1)
                as *mut core::ffi::c_void,
            lpszMenuName: std::ptr::null(),
            lpszClassName: cls.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 {
            return;
        }
        // 客户区固定 CLIENT_W × CLIENT_H，外框按标题栏/边框的实际尺寸反推
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: CLIENT_W,
            bottom: CLIENT_H,
        };
        AdjustWindowRect(&mut rc, style, 0);
        let hwnd = CreateWindowExW(
            0,
            cls.as_ptr(),
            wide_static("inklink 设置").as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            rc.right - rc.left,
            rc.bottom - rc.top,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return;
        }
        ShowWindow(hwnd, SW_SHOW);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 数字要在区间内才算数() {
        assert_eq!(parse_clamped("5", COLS_RANGE), Some(5));
        assert_eq!(parse_clamped("  5 ", COLS_RANGE), Some(5));
        assert_eq!(parse_clamped("3", COLS_RANGE), Some(3));
        assert_eq!(parse_clamped("9", COLS_RANGE), Some(9));
        assert_eq!(parse_clamped("2", COLS_RANGE), None);
        assert_eq!(parse_clamped("10", COLS_RANGE), None);
        assert_eq!(parse_clamped("", COLS_RANGE), None);
        assert_eq!(parse_clamped("abc", COLS_RANGE), None);
        assert_eq!(parse_clamped("5.5", COLS_RANGE), None);
        assert_eq!(parse_clamped("-1", COLS_RANGE), None);
    }

    #[test]
    fn 撤回窗口允许_0_表示关掉() {
        assert_eq!(parse_clamped("0", UNDO_RANGE), Some(0));
        assert_eq!(parse_clamped("2000", UNDO_RANGE), Some(2000));
        assert_eq!(parse_clamped("10000", UNDO_RANGE), Some(10000));
        assert_eq!(parse_clamped("10001", UNDO_RANGE), None);
    }

    #[test]
    fn 地址留空就用默认值且去掉尾部斜杠() {
        assert_eq!(endpoint_or_default(""), DEF_ENDPOINT);
        assert_eq!(endpoint_or_default("   "), DEF_ENDPOINT);
        assert_eq!(endpoint_or_default("http://127.0.0.1:11434/"), DEF_ENDPOINT);
        assert_eq!(
            endpoint_or_default(" http://192.168.1.9:8080 "),
            "http://192.168.1.9:8080"
        );
    }

    #[test]
    fn 模型名留空就用默认值() {
        assert_eq!(model_or_default(""), DEF_MODEL);
        assert_eq!(model_or_default("  "), DEF_MODEL);
        assert_eq!(model_or_default(" qwen3:8b "), "qwen3:8b");
    }

    #[test]
    fn 页签_id_对应六页_别的_id_不算() {
        assert_eq!(page_of_tab(2001), Some(0));
        assert_eq!(page_of_tab(2006), Some(5));
        assert_eq!(page_of_tab(2007), None);
        assert_eq!(page_of_tab(2000), None);
        assert_eq!(page_of_tab(ID_SAVE), None);
        assert_eq!(PAGE_TITLES[PAGE_ABOUT], "关于");
    }
}
