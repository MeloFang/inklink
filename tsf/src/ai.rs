//! AI 的界面侧（P4b）：把"选中文字 → 翻译/改写/续写 → 写回选区"这条链串起来。
//!
//! 引擎那边（P4a，`rime-server/rime-server.cc`）已经能发请求、自己起后台线程、
//! 完成后 `PostMessage(hwnd, WM_APP + 30, seq, 0)` 通知。这里只负责薄壳该做的事：
//!
//! ```text
//! ☰ 菜单 → ai::from_menu(action)
//!   → TextService 排一个 EditAction::ReadSelection（编辑会话里读选区，见 lib.rs）
//!   → ai::start(...) 拿 seq、记账、通过管道发协议 6
//!   → 引擎后台发 HTTP，完了 PostMessage(WM_APP+30, seq)
//!   → 浮条的 wndproc 调 ai::on_done(seq)
//!   → ipc::ai_result(seq) 取回文字（此时是内存读，微秒级）
//!   → TextService 排一个 EditAction::AiApply（校验选区没变才写回）
//! ```
//!
//! ## 为什么每一步都要再排一次队
//!
//! 编辑会话是**单槽位**的（`TextServiceInner::pending`），而我们既不能在按键线程上
//! 做管道往返（会把宿主的 UI 线程挂住），也不能在收到结果时直接改文档（必须先拿到
//! 一个 `ec`）。所以读和写各排一次队，中间隔着引擎那次 HTTP。
//!
//! ## 失败必须静默
//!
//! 这是输入法：AI 挂了可以没反应、可以写日志，**不能弹窗、不能吃键、不能卡住**。
//! 所以这里所有错误路径都只 `log_line`，一个返回值都不往上抛。

use std::cell::Cell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Registry::KEY_READ;
use windows_sys::Win32::UI::WindowsAndMessaging::WM_APP;

use crate::config;
use crate::ipc;
use crate::state;

/// 引擎处理完通知浮条的消息（必须和 `rime-server.cc` 里的 `WM_APP + 30` 一致）。
pub const WM_AI_DONE: u32 = WM_APP + 30;

/// 三个动作的编号。**必须和引擎的 `enum AiAction` 一致**（协议里就是一个字节）。
pub const ACTION_TRANSLATE: u8 = 0;
pub const ACTION_REWRITE: u8 = 1;
pub const ACTION_CONTINUE: u8 = 2;

/// 送给模型的最长字数。选中一整章再点翻译，提示词会大到没意义、还慢；
/// 2000 字足够一段/一屏，也远小于协议里的 `u16` 长度上限。
pub const MAX_INPUT_CHARS: usize = 2000;
/// 写回的最长字数。模型偶尔会复读，别让它把整篇文档灌满。
pub const MAX_RESULT_CHARS: usize = 4000;
/// 一次 AI 任务的记账保留多久。超过就该是通知丢了，清掉免得越积越多。
const JOB_TTL: Duration = Duration::from_secs(120);

/// 注册表里 AI 那三项的名字（**引擎侧读的是同一份**，见 `rime-server.cc` 的 `ai_config()`）。
pub const VALUE_AI_ENABLE: &str = "AI_Enable";
pub const VALUE_AI_ENDPOINT: &str = "AI_Endpoint";
pub const VALUE_AI_MODEL: &str = "AI_Model";

/// 一个还在路上的 AI 请求。
pub struct Job {
    pub seq: u32,
    pub action: u8,
    /// 发起时的选中文字，写回前拿它核对选区有没有被用户改掉。
    pub source: String,
    started: Instant,
}

static NEXT_SEQ: AtomicU32 = AtomicU32::new(0); // 0 = 还没播种
static JOBS: Mutex<Vec<Job>> = Mutex::new(Vec::new());

type StartFn = fn(usize, u8);
type ApplyFn = fn(usize, u8, &str, &str);

thread_local! {
    /// 本线程的输入服务：`(发起函数, 上下文指针)`。按线程存，和浮条窗口一致 ——
    /// TSF 按线程激活，一个进程里可能有好几条线程各有自己的服务实例。
    static SERVICE: Cell<(Option<StartFn>, usize)> = const { Cell::new((None, 0)) };
    /// 本线程的写回函数：同上。
    static APPLY: Cell<(Option<ApplyFn>, usize)> = const { Cell::new((None, 0)) };
}

/// 菜单命令 ID → 动作。`None` = 这条命令不是 AI 的。
pub fn action_for_cmd(cmd: u32) -> Option<u8> {
    match cmd {
        crate::menu::CMD_AI_TRANSLATE => Some(ACTION_TRANSLATE),
        crate::menu::CMD_AI_REWRITE => Some(ACTION_REWRITE),
        crate::menu::CMD_AI_CONTINUE => Some(ACTION_CONTINUE),
        _ => None,
    }
}

/// 这个动作是"替换选中文字"还是"接在后面"。
///
/// 译文/改写稿要顶掉原文；续写要接在原文后面（用户选一段是想让模型接着它的语境写）。
pub fn replaces_selection(action: u8) -> bool {
    !matches!(action, ACTION_CONTINUE)
}

/// 动作的中文名（只用于日志）。
pub fn action_name(action: u8) -> &'static str {
    match action {
        ACTION_REWRITE => "改写",
        ACTION_CONTINUE => "续写",
        _ => "翻译",
    }
}

/// 按字符（不是字节）截断，不会把一个汉字/emoji 切坏。
pub fn clip_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// AI 有没有开。**默认关**（铁律第三条：默认不联网）。
///
/// 只在菜单弹出时读一次（微秒级），不进按键路径，所以不用像 `config` 那样解释"为什么不缓存"。
pub fn enabled() -> bool {
    unsafe {
        let Some(key) = state::open_key_path(config::KEY_PATH, KEY_READ) else {
            return false;
        };
        let on = state::query_dword(key, VALUE_AI_ENABLE).unwrap_or(0) != 0;
        windows_sys::Win32::System::Registry::RegCloseKey(key);
        on
    }
}

/// 序号种子：**跨进程也得独一无二**。
///
/// 引擎的结果表是 `std::map<u32, AiJob>`，只按 seq 索引；而 DLL 在每个宿主进程里各有一份，
/// 记事本、微信、浏览器会同时存在。如果都从 1 开始数，两个进程各发一次 AI 就会撞号 ——
/// 后一个覆盖前一个的条目，取结果时会拿到别人的（甚至被清掉）。所以种子混进 PID 和
/// 当前时间的低 16 位；同进程内仍然只是自增，靠 `fetch_add` 保证不重。
fn seed_from(pid: u32, nanos: u32) -> u32 {
    let v = (pid << 16) ^ (nanos & 0xFFFF);
    if v == 0 { 1 } else { v }
}

fn seed_seq() -> u32 {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    seed_from(pid, nanos)
}

fn next_seq() -> u32 {
    if NEXT_SEQ.load(Ordering::Relaxed) == 0 {
        // 播种可能被两条线程同时撞上：比较交换，只有一份生效
        let _ = NEXT_SEQ.compare_exchange(0, seed_seq(), Ordering::Relaxed, Ordering::Relaxed);
    }
    let s = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
    // 0 留给"还没播种"，回绕到 0 就跳过
    if s == 0 {
        NEXT_SEQ.store(2, Ordering::Relaxed);
        1
    } else {
        s
    }
}

fn prune(now: Instant) {
    if let Ok(mut jobs) = JOBS.lock() {
        jobs.retain(|j| now.duration_since(j.started) < JOB_TTL);
    }
}

fn remember_at(seq: u32, action: u8, source: &str, started: Instant) {
    let job = Job {
        seq,
        action,
        source: source.to_string(),
        started,
    };
    prune(started);
    if let Ok(mut jobs) = JOBS.lock() {
        jobs.push(job);
    }
}

fn take_at(seq: u32) -> Option<Job> {
    let mut jobs = JOBS.lock().ok()?;
    let idx = jobs.iter().position(|j| j.seq == seq)?;
    Some(jobs.remove(idx))
}

/// 登记并发出一个 AI 请求，返回序号。没有浮条窗口时返回 `None`（拿不到通知，发了也白发）。
///
/// `text` 是选区的**全文**：它要留着和写回时的选区比对，别在这里截断。
/// 长度上限由调用方（`lib.rs` 读选区那一处）把关。
pub fn start(action: u8, text: &str) -> Option<u32> {
    let hwnd = crate::bar::hwnd();
    if hwnd == 0 {
        crate::log_line("ai: 没有浮条窗口，放弃");
        return None;
    }
    let seq = next_seq();
    remember_at(seq, action, text, Instant::now());
    crate::log_line(&format!(
        "ai: start seq={seq} action={} chars={}",
        action_name(action),
        text.chars().count()
    ));
    // 浮条上盖一行"翻译中…"：模型慢的时候用户至少知道点的那一下生效了。
    // 15 秒是个上限，正常请求回来会立刻被"完成/失败"覆盖。
    crate::bar::set_status(&format!("{}中…", action_name(action)), 15_000);
    let _ = ipc::ai_request(seq, hwnd as u32, action, text);
    Some(seq)
}

/// 引擎通知"这个序号好了"：取结果、交给输入服务写回。
///
/// 跑在浮条的窗口过程里（宿主 UI 线程、10ms 级）。取结果是一次管道往返，
/// 有超时；拿不到就只写日志。
pub fn on_done(seq: u32) {
    let Some(job) = take_at(seq) else {
        crate::log_line(&format!("ai: 收到未知序号的完成通知 seq={seq}"));
        return;
    };
    let (state, text) = ipc::ai_result(seq);
    match state {
        1 if !text.trim().is_empty() => {
            // 提示"可以撤回"：后面那两秒内按一次退格就能还原（见 lib.rs 的 AiUndo）
            crate::bar::set_status("完成 · 退格可撤回", 2_500);
            let out = clip_chars(&text, MAX_RESULT_CHARS);
            crate::log_line(&format!(
                "ai: done seq={seq} action={} chars={}",
                action_name(job.action),
                out.chars().count()
            ));
            apply(job.action, &job.source, out);
        }
        1 => {
            crate::bar::set_status("AI 没返回内容", 2_500);
            crate::log_line(&format!("ai: seq={seq} 返回空文本，丢掉"));
        }
        2 => {
            crate::bar::set_status("AI 失败（详见日志）", 3_000);
            crate::log_line(&format!("ai: seq={seq} 失败：{text}"));
        }
        other => {
            crate::bar::set_status("AI 没响应", 3_000);
            crate::log_line(&format!("ai: seq={seq} 未知状态 {other}"));
        }
    }
}

fn apply(action: u8, expect: &str, text: &str) {
    APPLY.with(|cell| match cell.get() {
        (Some(f), ctx) if ctx != 0 => f(ctx, action, expect, text),
        _ => crate::log_line("ai: 本线程没有活动的输入服务，结果丢掉"),
    });
}

/// 挂上本线程的输入服务（`Activate` 时调）。零指针 = 清掉。
pub fn set_service(start: StartFn, start_ctx: usize, apply: ApplyFn, apply_ctx: usize) {
    SERVICE.with(|c| c.set((Some(start), start_ctx)));
    APPLY.with(|c| c.set((Some(apply), apply_ctx)));
}

/// 摘掉本线程的输入服务（`Deactivate` 时调）。
pub fn clear_service() {
    SERVICE.with(|c| c.set((None, 0)));
    APPLY.with(|c| c.set((None, 0)));
}

/// 菜单点了某个 AI 动作：让输入服务去读选区（读和发请求都在编辑会话里做）。
pub fn from_menu(action: u8) {
    if !enabled() {
        crate::log_line("ai: 设置里没启用 AI，忽略");
        return;
    }
    SERVICE.with(|cell| match cell.get() {
        (Some(f), ctx) if ctx != 0 => f(ctx, action),
        _ => crate::log_line("ai: 本线程没有活动的输入服务，忽略"),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 菜单命令和动作一一对应() {
        use crate::menu::{CMD_AI_CONTINUE, CMD_AI_REWRITE, CMD_AI_TRANSLATE};
        assert_eq!(action_for_cmd(CMD_AI_TRANSLATE), Some(ACTION_TRANSLATE));
        assert_eq!(action_for_cmd(CMD_AI_REWRITE), Some(ACTION_REWRITE));
        assert_eq!(action_for_cmd(CMD_AI_CONTINUE), Some(ACTION_CONTINUE));
        assert_eq!(action_for_cmd(0), None, "0 不是 AI 命令");
        assert_eq!(action_for_cmd(crate::menu::CMD_SETTINGS), None);
    }

    #[test]
    fn 只有续写不替换选区() {
        assert!(replaces_selection(ACTION_TRANSLATE));
        assert!(replaces_selection(ACTION_REWRITE));
        assert!(!replaces_selection(ACTION_CONTINUE));
    }

    #[test]
    fn 截断按字符不切坏汉字() {
        assert_eq!(clip_chars("你好世界", 2), "你好");
        assert_eq!(clip_chars("你好", 9), "你好");
        assert_eq!(clip_chars("", 3), "");
        // ASCII 与 emoji（4 字节）都要整块留
        assert_eq!(clip_chars("hello", 3), "hel");
        assert_eq!(clip_chars("a😀b", 2), "a😀");
    }

    #[test]
    fn 序号不重复且不为零() {
        let a = next_seq();
        let b = next_seq();
        assert!(a >= 1 && b >= 1);
        assert_ne!(a, b);
    }

    #[test]
    fn 种子混进进程号避免跨进程撞号() {
        // 两个进程的 PID 不同 → 种子不同（同 nanos 也不撞）
        let a = seed_from(0x1234, 0x5678);
        let b = seed_from(0x1235, 0x5678);
        assert_ne!(a, b);
        assert_ne!(a, 0);
        assert_ne!(seed_from(0, 0), 0, "全零也要退化成非零");
        // nanos 不同 → 种子不同
        assert_ne!(seed_from(0x1234, 1), seed_from(0x1234, 2));
    }

    #[test]
    fn 记账取走一次就没了() {
        let now = Instant::now();
        remember_at(4242, ACTION_TRANSLATE, "hello", now);
        let job = take_at(4242).expect("应该记得");
        assert_eq!(job.action, ACTION_TRANSLATE);
        assert_eq!(job.source, "hello");
        assert!(take_at(4242).is_none(), "取走就该清掉");
    }

    #[test]
    fn 老任务会被清掉() {
        let now = Instant::now();
        let old = now - JOB_TTL - Duration::from_secs(1);
        remember_at(1, ACTION_REWRITE, "旧", old);
        remember_at(2, ACTION_REWRITE, "新", now);
        prune(now);
        assert!(take_at(1).is_none(), "过期的任务不该留着");
        assert!(take_at(2).is_some(), "新任务要留着");
    }

    #[test]
    fn 没有输入服务时点菜单不崩() {
        clear_service();
        from_menu(ACTION_TRANSLATE); // 没有服务 → 只写日志
    }
}
