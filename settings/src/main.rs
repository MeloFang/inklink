//! inklink 设置 —— 一页原生小窗，只改 `HKCU\Software\inklink\Config` 里那几个值。
//!
//! 为什么是这么小一个程序：现在可配的就八项（暗色、列数、行数、撤回窗口、学习开关，
//! 加上 P4b 的 AI 三项），为八项做六页设置台是过度设计。等 P6 把配置撑到几十项、
//! 需要分组和长列表时，再考虑换 webview 那套（见 PLAN.md 的 P2b）。
//!
//! 为什么用系统控件而不是自绘：浮条和候选窗自绘是因为它们要跟着主题变、还要在
//! 宿主进程里跑（不能引 UI 框架）；设置窗是独立进程、一天用一次，系统控件够用。
//! 代价是它不跟暗色 —— 这一点写进 README 比让用户猜好。
//!
//! 值的读写规则与 `tsf/src/config.rs`、`tsf/src/ai.rs` 必须一致，否则设置台和 DLL
//! 会各说各话：`Dark`/`Learn`/`AI_Enable` 是 DWORD（0/1），列数 3–9、行数 1–9、
//! 撤回窗口 0–10000，`AI_Endpoint`/`AI_Model` 是 SZ（引擎自己读那两个）。

use std::cell::RefCell;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT, HGDIOBJ};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_WRITE, REG_DWORD, REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowTextW,
    IsDialogMessageW, MessageBoxW, PostQuitMessage, RegisterClassW, SendMessageW, SetWindowTextW,
    ShowWindow, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, CW_USEDEFAULT,
    ES_AUTOHSCROLL, ES_NUMBER, MB_ICONERROR, MB_OK, MSG, SW_SHOW, WM_COMMAND, WM_CREATE,
    WM_DESTROY, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
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

#[derive(Default)]
struct Ui {
    cols: HWND,
    rows: HWND,
    dark: HWND,
    learn: HWND,
    undo: HWND,
    ai_enable: HWND,
    ai_endpoint: HWND,
    ai_model: HWND,
    corpus: HWND,
    status: HWND,
}

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

unsafe fn build_ui(hwnd: HWND) {
    let font = GetStockObject(DEFAULT_GUI_FONT) as HGDIOBJ;
    let add = |h: HWND| {
        SendMessageW(
            h,
            0x0030, // WM_SETFONT
            font as usize,
            1,
        );
    };

    let label = |t: &str, x: i32, y: i32, w: i32| {
        let h = child("STATIC", t, 0, x, y, w, 18, hwnd, 0);
        add(h);
        h
    };
    let edit = |t: &str, x: i32, y: i32, w: i32, id: usize| {
        let h = child(
            "EDIT",
            t,
            WS_BORDER | WS_TABSTOP | ES_NUMBER as u32,
            x,
            y,
            w,
            22,
            hwnd,
            id,
        );
        SendMessageW(h, 0x0030, font as usize, 1);
        h
    };
    // 文本输入框：地址和模型名是字符串，不能带 ES_NUMBER，长地址要能横向滚动
    let edit_text = |t: &str, x: i32, y: i32, w: i32, id: usize| {
        let h = child(
            "EDIT",
            t,
            WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            x,
            y,
            w,
            22,
            hwnd,
            id,
        );
        SendMessageW(h, 0x0030, font as usize, 1);
        h
    };
    let check = |t: &str, x: i32, y: i32, w: i32, id: usize| {
        let h = child(
            "BUTTON",
            t,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            x,
            y,
            w,
            20,
            hwnd,
            id,
        );
        add(h);
        h
    };
    let button = |t: &str, x: i32, y: i32, w: i32, id: usize| {
        let h = child("BUTTON", t, WS_TABSTOP, x, y, w, 26, hwnd, id);
        add(h);
        h
    };

    let c = load_config();

    label("每页候选", 16, 18, 80);
    let cols = edit(&c.cols.to_string(), 100, 14, 44, ID_COLS);
    label("列 ×", 152, 18, 34);
    let rows = edit(&c.rows.to_string(), 186, 14, 44, ID_ROWS);
    label("行", 238, 18, 24);

    let dark = check("暗色主题（候选窗、浮条、菜单一起变）", 16, 50, 320, ID_DARK);
    if c.dark {
        SendMessageW(dark, BM_SETCHECK as u32, 1, 0);
    }

    let learn = check("从我的输入里学词（关掉就完全不记）", 16, 78, 320, ID_LEARN);
    if c.learn {
        SendMessageW(learn, BM_SETCHECK as u32, 1, 0);
    }

    label("撤回窗口（毫秒，0 = 关掉撤回）", 16, 112, 240);
    let undo = edit(&c.undo_ms.to_string(), 16, 134, 80, ID_UNDO);

    // ── AI（P4b）────────────────────────────────────────────
    label(
        "AI —— 选中一段文字，再点浮条 ☰ 里的「翻译/改写/续写」",
        16,
        168,
        380,
    );
    let ai_enable = check(
        "启用 AI（会连到你填的接口地址）",
        16,
        190,
        360,
        ID_AI_ENABLE,
    );
    if c.ai_enabled {
        SendMessageW(ai_enable, BM_SETCHECK as u32, 1, 0);
    }

    label("接口地址（Ollama 兼容端点）", 16, 220, 300);
    let ai_endpoint = edit_text(&c.ai_endpoint, 16, 240, 380, ID_AI_ENDPOINT);

    label("模型名", 16, 270, 100);
    let ai_model = edit_text(&c.ai_model, 16, 290, 380, ID_AI_MODEL);

    // ── 个人语料（M2）─────────────────────────────────────
    label("笔记目录（Obsidian 库；留空 = 不启用）", 16, 320, 380);
    let corpus = edit_text(&c.corpus_path, 16, 342, 380, ID_CORPUS);

    let status = child(
        "STATIC",
        "改完点保存，下一次按键就生效，不用重启。",
        0,
        16,
        386,
        380,
        18,
        hwnd,
        0,
    );
    add(status);

    button("保存", 16, 412, 76, ID_SAVE);
    button("恢复默认", 100, 412, 90, ID_RESET);
    button("关闭", 198, 412, 76, ID_CLOSE);
    button("立即导入", 286, 412, 100, ID_IMPORT);

    UI.with(|u| {
        let mut u = u.borrow_mut();
        u.cols = cols;
        u.rows = rows;
        u.dark = dark;
        u.learn = learn;
        u.undo = undo;
        u.ai_enable = ai_enable;
        u.ai_endpoint = ai_endpoint;
        u.ai_model = ai_model;
        u.corpus = corpus;
        u.status = status;
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
    let msg = import_all(&notes, &exe_dir);
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
                    let msg = import_all(&notes, &exe_dir);
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
        Ui {
            cols: self.cols,
            rows: self.rows,
            dark: self.dark,
            learn: self.learn,
            undo: self.undo,
            ai_enable: self.ai_enable,
            ai_endpoint: self.ai_endpoint,
            ai_model: self.ai_model,
            corpus: self.corpus,
            status: self.status,
        }
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
                    let ui = UI.with(|u| u.borrow().to_snapshot());
                    set_text(ui.cols, &DEF_COLS.to_string());
                    set_text(ui.rows, &DEF_ROWS.to_string());
                    set_text(ui.undo, &DEF_UNDO.to_string());
                    SendMessageW(ui.dark, BM_SETCHECK as u32, 0, 0);
                    SendMessageW(ui.learn, BM_SETCHECK as u32, 1, 0);
                    SendMessageW(ui.ai_enable, BM_SETCHECK as u32, 0, 0);
                    set_text(ui.ai_endpoint, DEF_ENDPOINT);
                    set_text(ui.ai_model, DEF_MODEL);
                    set_text(ui.corpus, "");
                    set_text(ui.status, "已填回默认值 —— 还要点一次“保存”才写进去。");
                }
                ID_CLOSE => {
                    DestroyWindow(hwnd);
                }
                ID_IMPORT => on_import(),
                _ => {}
                _ => {}
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
        let hwnd = CreateWindowExW(
            0,
            cls.as_ptr(),
            wide_static("inklink 设置").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            424,
            478,
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
}
