//! inklink 的 TSF 文本服务（薄壳）。
//!
//! 设计约束（见 `docs/tsf-plan.md`）：
//! - DLL 里**不放引擎**：词库、切分、排序都在独立进程，这里只做转发
//! - `DllMain` 什么都不做；不启动线程
//! - `DllCanUnloadNow` **永远返回 S_FALSE**（上一版就死在这条）
//!
//! 当前阶段：T4 —— 组合串显示（拼音直接显示在文档光标处）。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{
    CLASS_E_NOAGGREGATION, E_NOINTERFACE, HINSTANCE, LPARAM, RECT, S_FALSE, S_OK, WPARAM,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::TextServices::{
    IEnumTfDisplayAttributeInfo, ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl,
    ITfContext, ITfContextComposition, ITfDisplayAttributeInfo, ITfDisplayAttributeProvider,
    ITfDisplayAttributeProvider_Impl, ITfEditSession, ITfEditSession_Impl, ITfKeyEventSink,
    ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfRange, ITfRangeACP, ITfTextInputProcessor_Impl,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfThreadMgr, TF_ANCHOR_END,
    TF_ANCHOR_START, TF_DEFAULT_SELECTION, TF_ES_ASYNC, TF_ES_READWRITE, TF_SELECTION,
};
use windows::core::BOOL;
mod ai;
mod ai_panel;
mod bar;
mod candwin;
mod config;
mod display;
mod ipc;
mod menu;
mod pinyin;
mod state;
use windows::core::IUnknownImpl;
use windows::core::{GUID, HRESULT, Interface, Ref, implement};

/// 文本服务的 CLSID（固定值，注册表与代码必须一致）
pub const CLSID_INKLINK_TSF: GUID = GUID::from_u128(0x8f2a6c1e_4b3d_4f7a_9a21_5c6d7e8f9a0b);
/// 语言配置的 GUID
pub const GUID_PROFILE: GUID = GUID::from_u128(0x1b2c3d4e_5f60_4718_9a2b_3c4d5e6f7081);

const DLL_DESC: &str = "inklink 输入法";

/// 候选网格列数（P3 起可配置，默认 5）。
///
/// 每次现读注册表：一个 `RegOpenKeyExW` + 几个 `RegQueryValueExW` 是微秒级的，
/// 换来的是"改配置立刻生效、不用重启宿主"。见 `config.rs` 顶部的理由。
fn grid_cols() -> usize {
    config::snapshot().cols
}

/// 候选窗收起时每页几个（默认 5 = 一行）
/// 展开时每页几个（默认 25 = 5 行 × 5 列）
fn page_size(expanded: bool) -> usize {
    let cfg = config::snapshot();
    if expanded {
        cfg.expanded()
    } else {
        cfg.collapsed()
    }
}

/// 光标在屏幕上的位置：`(x, 顶边 y, 底边 y)`。
///
/// 顶边不能省：候选窗在屏幕底部时要往上翻，翻的基准是顶边（见 `candwin::place`）。
type CaretPos = (i32, i32, i32);

// ─────────────────────────── COM 类工厂 ───────────────────────────

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<windows::core::IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        // SAFETY: 标准 COM 契约；ppv 由调用方保证可写
        unsafe {
            if ppv.is_null() {
                return Err(windows::core::Error::from(
                    windows::Win32::Foundation::E_POINTER,
                ));
            }
            *ppv = core::ptr::null_mut();
            if !outer.is_null() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            let service: ITfTextInputProcessorEx = TextService::new().into();
            service.query(riid, ppv).ok()
        }
    }

    fn LockServer(&self, _lock: BOOL) -> windows::core::Result<()> {
        Ok(())
    }
}

// ─────────────────────────── 文本服务本体 ───────────────────────────

/// 待编辑会话执行的动作
enum EditAction {
    /// 更新组合串文本（拼音）并定位内部光标
    Compose(String, usize),
    /// 只移动组合串内部光标（方向键）
    MoveCaret(usize),
    /// 提交：把组合串替换成最终文字，然后结束组合
    Commit(String),
    /// 只上屏一部分：先把 `picked` 定格成真正的文本，再在它后面开一个新的组合串
    /// 装剩下的拼音 `rest`（光标放在 `rest` 末尾）。这样 `pikeqiu` 选完「皮」
    /// 之后，文章里已经落下「皮」，而候选窗继续给 `keqiu` 的候选。
    PickCompose(String, String),
    /// 取消：结束组合，清空
    Cancel,
    /// 插入一段文字到当前选区（中文标点用，没有组合）
    Insert(String),
    /// 插标点，但先看一眼光标前一个字符：是数字就插半角，否则插全角。
    ///
    /// 参数是 `(全角, 半角)`。数字后面要的是 `3.14`、`10:30`、`1,000` 里的那个
    /// 半角符号（用户实测："输入数字之后打的。会是.而不是。"）；判断必须在编辑
    /// 会话里做，因为按键线程上拿不到文档内容。
    InsertPunct(String, String),
    /// 自己删掉光标前的一个字（误选撤回时用，见 `OnKeyDown` 的退格分支）
    DeleteBack,
    /// 读出当前选中的文字，然后请引擎做 AI（翻译/改写/续写，参数是动作编号）。
    ///
    /// 为什么读选区要占一个编辑会话：`ITfContext::GetSelection` 必须在编辑会话里
    /// 拿着 `ec` 调，按键线程上没有。读完顺手把请求发出去 —— 发请求只是写管道，
    /// 不等模型（引擎那边是后台线程）。
    ReadSelection(u8),
    /// 把 AI 结果写回文档。`expect` 是发起时选中的原文，用来核对"用户有没有改过选区"。
    AiApply {
        action: u8,
        expect: String,
        text: String,
    },
    /// 撤回上一次 AI 写入：删掉插入的那段，必要时把原文写回去。
    ///
    /// 参数是 `(要恢复的原文, 插入的 UTF-16 长度, 写入后光标所在的锚点)`。
    /// 锚点对不上（用户点过别处 / 又打了字）就什么也不做，只写日志。
    AiRevert {
        original: String,
        inserted: i32,
        end_anchor: i32,
    },
}

struct TextServiceInner {
    thread_mgr: Mutex<Option<ITfThreadMgr>>,
    keystroke_mgr: Mutex<Option<ITfKeystrokeMgr>>,
    tid: Mutex<u32>,
    /// 当前拼音缓冲
    buffer: Mutex<String>,
    /// 当前候选
    cands: Mutex<Vec<String>>,
    /// 候选当前页
    page: Mutex<usize>,
    /// 候选窗是否展开成 5×5
    expanded: Mutex<bool>,
    /// 当前页内选中的候选下标
    sel: Mutex<usize>,
    /// 候选命中的缓冲长度（前缀回退时可能小于 buffer 全长）
    matched_len: Mutex<usize>,
    /// 是否已挂载按键接收器（防止重复挂载导致同一按键触发多次回调）
    advised: Mutex<bool>,
    /// 输入上下文（来自 OnKeyDown 的 pic，编辑会话要用）
    context: Mutex<Option<ITfContext>>,
    /// 当前活动的组合
    composition: Mutex<Option<ITfComposition>>,
    /// 组合串内部光标位置（字节索引，拼音全 ASCII）
    cursor: Mutex<usize>,
    /// 待编辑会话执行的动作
    pending: Mutex<Option<EditAction>>,
    /// 是否由我们主动结束组合（提交/取消），用于区分外部终止
    self_terminating: AtomicBool,
    /// 最近一次 GetTextExt 拿到的组合串屏幕位置（左下起 `(x, 顶边, 底边)`）
    last_pos: Mutex<Option<CaretPos>>,
    /// 本次按下 Shift 期间是否打过字（用于区分"单击"和"按住"）
    shift_used: AtomicBool,
    /// 刚上屏的词（用于识别"选错了、马上删掉"）
    commit_mark: Mutex<Option<CommitMark>>,
    /// 正在逐词选择的这一句（用于整句学习）
    sentence: Mutex<Option<SentenceAcc>>,
    /// 刚上屏的那一句（用于整句的撤回）
    sentence_mark: Mutex<Option<CommitMark>>,
    /// 攒着还没执行的退格次数（自己删字用，见 EditAction::DeleteBack）
    pending_backspace: Mutex<usize>,
    /// 刚写回的 AI 结果（用于"两秒内按退格撤回"）
    ai_undo: Mutex<Option<AiUndo>>,
}

#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfEditSession,
    ITfCompositionSink,
    ITfDisplayAttributeProvider
)]
struct TextService {
    inner: TextServiceInner,
}

impl TextService {
    fn new() -> Self {
        Self {
            inner: TextServiceInner {
                thread_mgr: Mutex::new(None),
                keystroke_mgr: Mutex::new(None),
                tid: Mutex::new(0),
                buffer: Mutex::new(String::new()),
                cands: Mutex::new(Vec::new()),
                page: Mutex::new(0),
                expanded: Mutex::new(false),
                sel: Mutex::new(0),
                matched_len: Mutex::new(0),
                advised: Mutex::new(false),
                context: Mutex::new(None),
                composition: Mutex::new(None),
                cursor: Mutex::new(0),
                pending: Mutex::new(None),
                self_terminating: AtomicBool::new(false),
                last_pos: Mutex::new(None),
                shift_used: AtomicBool::new(false),
                commit_mark: Mutex::new(None),
                sentence: Mutex::new(None),
                sentence_mark: Mutex::new(None),
                pending_backspace: Mutex::new(0),
                ai_undo: Mutex::new(None),
            },
        }
    }

    /// 记下"刚上屏了 `word`（拼音是 `code`）"，好判断用户是不是马上把它删了。
    fn mark_commit(&self, code: &str, word: &str) {
        let mut g = self
            .inner
            .commit_mark
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *g = CommitMark::new(code.to_string(), word.to_string());
    }

    /// 记下"刚上屏的整句"，好判断用户是不是把整句删光了。
    ///
    /// 整句的撤回和逐词的是两条独立的账：删掉最后那个词只撤回那个词；
    /// 一直删到整句都没了，整句那条也要撤回（见 `count_backspace`）。
    fn mark_sentence(&self, code: &str, text: &str) {
        let mut g = self
            .inner
            .sentence_mark
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *g = CommitMark::new(code.to_string(), text.to_string());
    }

    /// 忘掉刚才那些上屏记录（用户又打别的字了）。
    fn clear_commit_mark(&self) {
        *self
            .inner
            .commit_mark
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        *self
            .inner
            .sentence_mark
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        // 攒着没执行的退格次数也作废，否则等下删字会多删
        *self
            .inner
            .pending_backspace
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = 0;
    }

    /// 有没有"刚上屏、还可能被撤回"的词或整句。
    fn undo_pending(&self) -> bool {
        let word = self
            .inner
            .commit_mark
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if word.as_ref().map(|m| !m.expired()).unwrap_or(false) {
            return true;
        }
        let sent = self
            .inner
            .sentence_mark
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        sent.as_ref().map(|m| !m.expired()).unwrap_or(false)
    }

    /// 有没有"刚写回、还能撤回"的 AI 结果。
    fn ai_undo_pending(&self) -> bool {
        let g = self.inner.ai_undo.lock().unwrap_or_else(|p| p.into_inner());
        g.as_ref().map(|u| !u.expired()).unwrap_or(false)
    }

    /// 取走 AI 撤回标记（过期就当成没有）。只撤一次。
    fn take_ai_undo(&self) -> Option<(String, i32, i32)> {
        let mut g = self.inner.ai_undo.lock().unwrap_or_else(|p| p.into_inner());
        match g.as_ref() {
            Some(u) if !u.expired() => {
                let out = (u.original.clone(), u.inserted, u.end_anchor);
                *g = None;
                Some(out)
            }
            Some(_) => {
                *g = None;
                None
            }
            None => None,
        }
    }

    /// 忘掉 AI 撤回标记（用户又按了别的键）。
    fn clear_ai_undo(&self) {
        *self.inner.ai_undo.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// 写回成功后挂上撤回标记：`undo_window` 内按一次退格就能还原。
    ///
    /// 撤回窗口配置成 0（关掉误选撤回）时也不挂 —— 两个超时一个语义，别让用户猜。
    fn arm_ai_undo(&self, original: &str, inserted: i32, end_anchor: i32) {
        let Some(window) = crate::config::snapshot().undo_window() else {
            return;
        };
        if inserted <= 0 {
            return;
        }
        *self.inner.ai_undo.lock().unwrap_or_else(|p| p.into_inner()) = Some(AiUndo {
            original: original.to_string(),
            inserted,
            end_anchor,
            deadline: std::time::Instant::now() + window,
        });
    }

    /// 记一次退格。返回需要撤回学习的 `(码, 词)` 列表 —— 删掉最后一个词时
    /// 一条，接着把整句也删光时两条（逐词的 + 整句的）。
    fn count_backspace(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let word = step_mark(
            &mut self
                .inner
                .commit_mark
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        if let Some(pair) = word {
            out.push(pair);
        }
        let sent = step_mark(
            &mut self
                .inner
                .sentence_mark
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        if let Some(pair) = sent {
            out.push(pair);
        }
        out
    }

    /// 记一次候选选择，供整句学习用。
    ///
    /// 返回 `Some((整句拼音, 整句文字))` 表示这一串拼音打完了 —— 该把整句
    /// 也报给引擎学习：**同一个词你反复用整句打出来**，下次打同样的全拼时
    /// 整句候选才该往上排。逐词的选择已经在 `apply_pick` 里报过了，两边
    /// 记的是不同的码（词码 vs 整串全拼），不会互相覆盖。
    ///
    /// 只选一次的不用报：那一次本身就已经按整串记下了（`picks >= 2` 是门槛）。
    fn note_pick_for_sentence(
        &self,
        code_before: &str,
        action: &EditAction,
    ) -> Option<(String, String)> {
        let (word, finished) = match action {
            EditAction::Commit(w) => (w.clone(), true),
            EditAction::PickCompose(w, _) => (w.clone(), false),
            _ => return None,
        };
        let mut g = self
            .inner
            .sentence
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match g.as_mut() {
            Some(acc) => {
                acc.text.push_str(&word);
                acc.picks += 1;
                acc.code = merge_sentence_code(&acc.code, code_before);
            }
            None => {
                *g = Some(SentenceAcc {
                    code: code_before.to_string(),
                    text: word,
                    picks: 1,
                });
            }
        }
        if !finished {
            return None;
        }
        let acc = g.take()?;
        if should_report_sentence(acc.picks, acc.text.chars().count()) {
            Some((acc.code, acc.text))
        } else {
            None
        }
    }

    /// 丢掉没打完的整句记录（Esc、退格删空、换成打英文、点了别处）。
    fn sentence_clear(&self) {
        let mut g = self
            .inner
            .sentence
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *g = None;
    }
}

/// 刚上屏的一个词，等着看用户是不是马上又把它删掉。
///
/// 为什么用"数退格次数"而不是 `ITfTextEditSink`：后者要注册新的编辑观察者、
/// 还要跟踪 range，出错就是挂死（见 1.0.1 的选词死锁）；数退格不需要任何
/// 新接口，而且退格事件本来就由我们处理。
///
/// 判据：上屏后 2 秒内（P3 可配置）连按 `word.chars().count()` 次退格 —— 中文里
/// 一个汉字一次退格，正好等于把刚上屏的词删光。
struct CommitMark {
    code: String,
    word: String,
    remaining: usize,
    deadline: std::time::Instant,
    /// 这次的窗口多长。每按一次退格都会顺延，所以得记住原值。
    window: std::time::Duration,
}

impl CommitMark {
    /// 建一个"可能选错了"的记账。
    ///
    /// 返回 `None` 有两种原因：词是空的；或者用户把撤回窗口配成了 0（关掉撤回）。
    fn new(code: String, word: String) -> Option<Self> {
        let remaining = word.chars().count();
        if remaining == 0 {
            return None;
        }
        let window = crate::config::snapshot().undo_window()?;
        Some(Self {
            code,
            word,
            remaining,
            deadline: std::time::Instant::now() + window,
            window,
        })
    }

    /// 记一次退格。删光了返回 `(code, word)`，该撤回学习。
    fn on_backspace(&mut self) -> Option<(String, String)> {
        self.remaining = self.remaining.saturating_sub(1);
        self.deadline = std::time::Instant::now() + self.window;
        if self.remaining == 0 {
            Some((self.code.clone(), self.word.clone()))
        } else {
            None
        }
    }

    fn expired(&self) -> bool {
        std::time::Instant::now() > self.deadline
    }
}

/// 刚写回的 AI 结果，等着看用户是不是要撤回。
///
/// 为什么不用应用自己的 Ctrl+Z：TSF 的 `SetText` 有没有进宿主的撤销栈，各家不一样
/// （Electron 系尤其不可靠）。这里只做一件确定的事——**两秒内按一次退格，把我们刚写的
/// 那段换成原文**——判据和"误选撤回"一致，用户也不用记新的快捷键。
struct AiUndo {
    /// 覆盖掉的原文；续写是空串（只需要删掉插入的那段）。
    original: String,
    /// 插入的 UTF-16 长度（ACP 的锚点单位就是 UTF-16）。
    inserted: i32,
    /// 写入完成后光标所在的锚点，撤回前必须还对得上。
    end_anchor: i32,
    deadline: std::time::Instant,
}

impl AiUndo {
    fn expired(&self) -> bool {
        std::time::Instant::now() > self.deadline
    }
}

/// 撤回时要覆盖的区间：从 `end_anchor - inserted` 起、长度 `inserted`。
///
/// 抽成纯函数是因为它决定"会不会删错地方"：锚点不够长、长度为 0 都必须拒绝。
fn revert_extent(end_anchor: i32, inserted: i32) -> Option<(i32, i32)> {
    if inserted <= 0 || end_anchor < inserted {
        return None;
    }
    Some((end_anchor - inserted, inserted))
}

/// 记一次退格到某个槽位上。
///
/// 删光了返回 `(code, word)` 并把槽位清空；过期了也清空（不再算误选）；
/// 否则只是递减。提出来是为了能直接单测，不用造一个 COM 对象。
fn step_mark(slot: &mut Option<CommitMark>) -> Option<(String, String)> {
    let mut done = None;
    let mut drop_it = false;
    if let Some(mark) = slot.as_mut() {
        if mark.expired() {
            drop_it = true;
        } else {
            done = mark.on_backspace();
            if done.is_some() {
                drop_it = true;
            }
        }
    }
    if drop_it {
        *slot = None;
    }
    done
}

/// 正在逐词选择的这一句。
///
/// 打 `jintiantianqi` 选「今天」再选「天气」，逐词学习记的是 `jintian→今天`
/// 和 `tianqi→天气`；但整句 `jintiantianqi→今天天气` 才是用户真正想要的东西，
/// 只记词的话下次打全拼，整句候选不会往上排。
struct SentenceAcc {
    /// 这一句开始时输入的完整拼音（还没被分段啃掉过）
    code: String,
    /// 已经选出来的字
    text: String,
    /// 选了几次
    picks: usize,
}

/// 整句短于这个字数就不算"整句"。
///
/// 原来是 4（`今天天气`），2026-11-04 降到 2：短名字（王佳、李雷）没法靠整句学，
/// 而"分段选两个字"本身就是用户明确表达过的偏好（选王 → 选佳）。
/// 单字仍然不学（`USER_MIN_CHARS = 2`），所以这条只影响"两个字以上的组合"。
const SENTENCE_MIN_CHARS: usize = 2;

/// 要不要把这一句当成整句上报：至少选过两次（选一次的那次本身已经记过了），
/// 且至少 `SENTENCE_MIN_CHARS` 个字。
fn should_report_sentence(picks: usize, chars: usize) -> bool {
    picks >= 2 && chars >= SENTENCE_MIN_CHARS
}

/// 整句的码怎么拼。
///
/// 分段上屏有两种节奏：
/// - 一次打完 `pikeqiu`，选「皮」后剩下的码是 `keqiu` —— 它是 `pikeqiu` 的后缀，
///   所以直接用第一个码，**不能**再拼一次（否则记成 `pikeqiukeqiu`）。
/// - 先打 `wang` 选「王」，再打 `jia` 选「佳」—— `jia` 不是 `wang` 的后缀，
///   这一句真实的码是 `wangjia`，要拼起来。
fn merge_sentence_code(code: &str, next: &str) -> String {
    if next.is_empty() || code.ends_with(next) {
        code.to_string()
    } else {
        format!("{code}{next}")
    }
}

/// 我们自己注入的退格还剩几个没被自己的 sink 吃掉。
///
/// 少数宿主（WPS 正文控件等）既不给 ACP 区间、`ShiftStart` 也不动，只能注入
/// 原生 `VK_BACK` 让宿主自己删字。注入的键会再走一遍 TSF 回到我们这里，
/// 所以注入前记数、收到时放行（返回 FALSE），否则会把注入的键再认领一次。
static INJECTED_BACK: Mutex<Option<(std::time::Instant, usize)>> = Mutex::new(None);

/// 记住"接下来有 n 个退格是我们自己发的"。
fn mark_injected_backspace(n: usize) {
    let mut g = INJECTED_BACK.lock().unwrap_or_else(|p| p.into_inner());
    *g = if n == 0 {
        None
    } else {
        Some((std::time::Instant::now(), n))
    };
}

/// 还有没有自己注入、尚未放行的退格（只读，`OnTestKeyDown` 用）。
fn injected_backspace_pending() -> bool {
    let g = INJECTED_BACK.lock().unwrap_or_else(|p| p.into_inner());
    match *g {
        // 1 秒还没消化掉就当作丢了，免得把用户真正的退格一直放行
        Some((at, n)) => n > 0 && at.elapsed() < std::time::Duration::from_millis(1000),
        None => false,
    }
}

/// 消化掉一个注入的退格（`OnKeyDown` 用）：返回 true 表示这次按键是我们自己发的。
fn take_injected_backspace() -> bool {
    if !injected_backspace_pending() {
        *INJECTED_BACK.lock().unwrap_or_else(|p| p.into_inner()) = None;
        return false;
    }
    let mut g = INJECTED_BACK.lock().unwrap_or_else(|p| p.into_inner());
    match *g {
        Some((at, n)) if n > 1 => {
            *g = Some((at, n - 1));
            true
        }
        _ => {
            *g = None;
            true
        }
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> windows::core::Result<()> {
        let r = self.advise_keys(ptim, tid);
        // 输入法被切到前台 —— 让浮条出现（搜狗那种「用的时候才有」）
        bar::show();
        r
    }

    fn Deactivate(&self) -> windows::core::Result<()> {
        log_line("deactivate");
        bar::hide();
        // 切换输入法/焦点时清空状态，否则缓冲会跨会话累积
        self.inner
            .buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        self.inner
            .cands
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        *self
            .inner
            .matched_len
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = 0;
        *self.inner.context.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self
            .inner
            .composition
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        *self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner()) = 0;
        candwin::hide();
        self.unadvise_keys();
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(
        &self,
        ptim: Ref<ITfThreadMgr>,
        tid: u32,
        _flags: u32,
    ) -> windows::core::Result<()> {
        log_line("ActivateEx");
        let r = self.advise_keys(ptim, tid);
        bar::show();
        r
    }
}

/// 组合串的显示属性（下划线）。
///
/// TSF 会用**我们的 CLSID** 直接 `CoCreateInstance` 再 QI 这个接口，
/// 所以它必须和文本服务实现在同一个类上——具体样式在 `display.rs`。
impl ITfDisplayAttributeProvider_Impl for TextService_Impl {
    fn EnumDisplayAttributeInfo(&self) -> windows::core::Result<IEnumTfDisplayAttributeInfo> {
        display::enum_info()
    }

    fn GetDisplayAttributeInfo(
        &self,
        pguid: *const GUID,
    ) -> windows::core::Result<ITfDisplayAttributeInfo> {
        display::info_for(pguid)
    }
}

/// 候选窗鼠标点击的跳板：`candwin` 的窗口过程在 UI 线程里直接调进来。
///
/// # Safety
/// `ctx` 必须是 `advise_keys` 里存进去的 `TextService_Impl` 指针。候选窗只在
/// 激活期间可见，能点到就说明对象还活着；`unadvise_keys` 会把回调拆掉。
unsafe extern "system" fn cand_click_thunk(ctx: usize, index: usize) {
    if ctx == 0 {
        return;
    }
    unsafe {
        let svc = &*(ctx as *const TextService_Impl);
        let _ = svc.handle_cand_click(index);
    }
}

/// AI 的"从菜单发起"跳板：`ai::from_menu` 在浮条窗口过程里调进来。
///
/// 只是个转发，真正的活（读选区）在编辑会话里做 —— 见 `start_ai`。
///
/// # Safety
/// `ctx` 必须是 `advise_keys` 里存进去的 `TextService_Impl` 指针，生命周期由
/// `unadvise_keys` 的清除保证。纯 Rust ABI，进程内部调用，不跨越 FFI 边界。
fn ai_start_thunk(ctx: usize, action: u8) {
    if ctx == 0 {
        return;
    }
    unsafe {
        let svc = &*(ctx as *const TextService_Impl);
        svc.start_ai(action);
    }
}

/// AI 的"写回结果"跳板：`ai::on_done` 取回文字后调进来。
///
/// # Safety
/// 同 [`ai_start_thunk`]。
fn ai_apply_thunk(ctx: usize, action: u8, expect: &str, text: &str) {
    if ctx == 0 {
        return;
    }
    unsafe {
        let svc = &*(ctx as *const TextService_Impl);
        svc.apply_ai(action, expect, text);
    }
}
impl TextService_Impl {
    /// 挂上按键接收器：TSF 把按键通过 `ITfKeystrokeMgr` 分发给注册的 sink。
    /// 幂等——Activate/ActivateEx 可能被反复调用，重复挂载会让同一按键触发多次回调。
    fn advise_keys(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> windows::core::Result<()> {
        let Some(thread_mgr) = ptim.as_ref() else {
            return Ok(());
        };

        // 已挂载就不再挂：这是「字母重复」的根因
        let mut advised = self.inner.advised.lock().unwrap_or_else(|p| p.into_inner());
        if *advised {
            log_line("activate skipped (already advised)");
            return Ok(());
        }

        *self.inner.tid.lock().unwrap() = tid;

        // 从 thread manager 取 ITfKeystrokeMgr
        let keystroke_mgr: ITfKeystrokeMgr = thread_mgr.cast()?;
        *self.inner.keystroke_mgr.lock().unwrap() = Some(keystroke_mgr.clone());
        // 留着它问"现在焦点在哪个文档"——判断有没有可编辑光标要用
        // （`unadvise_keys` 里会清掉）
        *self.inner.thread_mgr.lock().unwrap() = Some(thread_mgr.clone());

        // 激活时顺手确保引擎在跑——引擎崩了下次切输入法就自动复活
        ipc::ensure_engine();

        // 组合串下划线要的 atom（显示属性）。失败不影响打字，只是拼音没有虚线。
        let atom = display::ensure_atom();
        log_line(&format!("display-attr atom={atom}"));

        // 候选窗点鼠标选词：回调跳板带上自己，窗口过程只要把下标送回来。
        // 只在激活期间有效（`unadvise_keys` 里拆掉）。
        candwin::set_click_handler(cand_click_thunk, self as *const TextService_Impl as usize);
        // AI 的两个跳板（菜单发起 / 结果写回）。按线程存：TSF 按线程激活，
        // 一个进程里可能有好几条线程各有自己的服务实例。
        let me = self as *const TextService_Impl as usize;
        ai::set_service(ai_start_thunk, me, ai_apply_thunk, me);

        // 把自己作为按键 sink 挂上去（传引用，而不是把 owned 接口交给 Param）
        let sink: ITfKeyEventSink = self.to_interface();
        let r = unsafe { keystroke_mgr.AdviseKeyEventSink(tid, &sink, true) };
        let (chinese, punct) = state::snapshot();
        log_line(&format!(
            "activate tid={tid} advise={} version={} chinese={} punct={}",
            if r.is_ok() { "ok" } else { "FAILED" },
            env!("INKLINK_VERSION"),
            chinese,
            punct
        ));
        r?;
        *advised = true;
        Ok(())
    }

    fn unadvise_keys(&self) {
        let mut advised = self.inner.advised.lock().unwrap_or_else(|p| p.into_inner());
        if !*advised {
            return;
        }
        *advised = false;
        let tid = *self.inner.tid.lock().unwrap();
        if tid != 0 {
            if let Some(mgr) = self.inner.keystroke_mgr.lock().unwrap().as_ref() {
                unsafe {
                    let _ = mgr.UnadviseKeyEventSink(tid);
                }
            }
        }
        *self.inner.keystroke_mgr.lock().unwrap() = None;
        *self.inner.thread_mgr.lock().unwrap() = None;
        candwin::clear_click_handler();
        ai::clear_service();
    }

    /// 选完一个候选后的统一收尾：记"可能被撤回"的标记、记整句学习、提交编辑动作。
    fn finish_pick(&self, code_before: &str, action: EditAction) -> BOOL {
        if let EditAction::Commit(word) = &action {
            self.mark_commit(code_before, word);
        }
        if let Some((code, text)) = self.note_pick_for_sentence(code_before, &action) {
            log_line(&format!("learn-sentence code={code} text={text}"));
            if crate::config::snapshot().learn {
                ipc::select(0, &code, &text);
            } else {
                log_line("learn-sentence skipped (learn off)");
            }
            // 整句也记一条"可能被撤回"的账：删掉最后那个词只撤回那个词，
            // 一直删到整句都没了，这一条才触发。
            self.mark_sentence(&code, &text);
        }
        self.request_edit(action);
        BOOL(1)
    }

    /// 鼠标点了候选窗里的某一格：走和空格/数字键完全一样的那条路。
    ///
    /// `index` 是**整页里的下标**（网格第几格），不是"第几行第几个"；
    /// 窗口过程那边的 `hit_index` 已经算好了。
    fn handle_cand_click(&self, index: usize) -> BOOL {
        let mut buffer = self.inner.buffer.lock().unwrap_or_else(|p| p.into_inner());
        if buffer.is_empty() {
            return BOOL(0);
        }
        let mut cands = self.inner.cands.lock().unwrap_or_else(|p| p.into_inner());
        let mut cursor = self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner());
        let mut matched_len = self
            .inner
            .matched_len
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut page = self.inner.page.lock().unwrap_or_else(|p| p.into_inner());
        let mut expanded = self
            .inner
            .expanded
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut sel = self.inner.sel.lock().unwrap_or_else(|p| p.into_inner());
        let pos = *self
            .inner
            .last_pos
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let code_before = buffer.clone();
        let pick_idx = *page * page_size(*expanded) + index;
        match apply_pick(
            pick_idx,
            &mut cands,
            &mut buffer,
            &mut cursor,
            &mut matched_len,
            &mut page,
            &mut expanded,
            &mut sel,
            pos,
        ) {
            Some(action) => {
                log_line(&format!("pick(mouse {index}) buffer={buffer}"));
                self.finish_pick(&code_before, action)
            }
            None => BOOL(0),
        }
    }

    /// 把一个动作交给编辑会话执行（异步，安全，不阻塞按键线程）。
    fn request_edit(&self, action: EditAction) {
        *self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(action);
        self.poke_session();
    }

    /// 同上，但**槽位占着就不排**（返回 false）。
    ///
    /// AI 的两步（读选区、写回）都要用它：编辑会话只有一个槽位，AI 结果晚几秒才到，
    /// 这期间用户很可能正在打字、槽位里躺着一个组合串更新。这时候硬塞会把用户的
    /// 那一次更新挤掉（表现为"打着打着拼音不刷新了"）。宁可丢掉 AI 结果。
    fn request_edit_if_idle(&self, action: EditAction) -> bool {
        {
            let mut slot = self.inner.pending.lock().unwrap_or_else(|p| p.into_inner());
            if slot.is_some() {
                return false;
            }
            *slot = Some(action);
        }
        self.poke_session();
        true
    }

    /// 通知 TSF"有个编辑会话要跑"。槽位已经填好，这里只负责敲一下门。
    fn poke_session(&self) {
        let tid = *self.inner.tid.lock().unwrap();
        if tid == 0 {
            return;
        }
        let Some(ctx) = self.current_context() else {
            return;
        };
        let sink: ITfEditSession = self.to_interface();
        // SAFETY: 标准 TSF 编辑会话请求，sink 生命周期由 COM 引用计数管理
        let _ = unsafe { ctx.RequestEditSession(tid, &sink, TF_ES_ASYNC | TF_ES_READWRITE) };
    }

    /// 拿当前输入上下文：优先用 `OnKeyDown` 存下来的，没有再问线程焦点。
    ///
    /// 为什么要有第二条：AI 是"鼠标选中文字 → 点浮条"，用户可能一次键都没按过，
    /// 这时 `context` 还是空的。以前这条只服务打字路径，缺了也没什么；AI 缺了就是
    /// 点了菜单毫无反应。`ITfThreadMgr::GetFocus` 只是读焦点，不涉及编辑会话，安全。
    fn current_context(&self) -> Option<ITfContext> {
        if let Some(ctx) = self
            .inner
            .context
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
        {
            return Some(ctx);
        }
        let mgr = self
            .inner
            .thread_mgr
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()?;
        // SAFETY: 只读查询焦点文档管理器；拿到的是活着的上下文，随后由调用方排队
        unsafe {
            let doc = mgr.GetFocus().ok()?;
            doc.GetTop().ok()
        }
    }

    /// 现在有没有正在打的拼音（组合串非空）。
    ///
    /// AI 写回前要看一眼：用户等待结果的那几秒里要是已经开始打下一句，
    /// 结果写进去就会插在拼音中间。此时直接丢弃（只记日志）比"改错地方"好。
    fn is_composing(&self) -> bool {
        !self
            .inner
            .buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
    }

    /// 菜单点了翻译/改写/续写：排一次"读选区"。
    fn start_ai(&self, action: u8) {
        if self.is_composing() {
            log_line("ai: 正在打拼音，先不读选区");
            return;
        }
        if !self.request_edit_if_idle(EditAction::ReadSelection(action)) {
            log_line("ai: 编辑会话正忙，跳过这次请求");
        }
    }

    /// 引擎把结果送回来了：排一次"写回"。
    fn apply_ai(&self, action: u8, expect: &str, text: &str) {
        if self.is_composing() {
            log_line("ai: 正在打拼音，结果丢掉（不往组合串中间塞字）");
            return;
        }
        let action_desc = ai::action_name(action);
        if !self.request_edit_if_idle(EditAction::AiApply {
            action,
            expect: expect.to_string(),
            text: text.to_string(),
        }) {
            log_line(&format!("ai: 编辑会话正忙，{action_desc}结果丢掉"));
        }
    }

    /// 把输入上下文存下来（编辑会话要用）
    fn store_context(&self, pic: Ref<ITfContext>) {
        if let Some(ctx) = pic.as_ref() {
            *self.inner.context.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx.clone());
        }
    }

    /// 现在按键真的能往文档里塞字吗？
    ///
    /// 焦点一旦离开可编辑区域（点到工具栏、停在面板空白处、Explorer 列表、
    /// 桌面……），TSF 仍然会把按键送进来（我们是 `fForeground=TRUE` 的接收器），
    /// 但这时候**没有光标可以挂组合串**。不挡住就会在屏幕上飘出一个候选窗。
    ///
    /// 判断依据两条，命中任意一条就当"能打字"：
    /// 1. TSF 给了本进程一个 `ITfContext`（绝大多数正常情况）；
    /// 2. 线程焦点里确实有一个顶层上下文（兜住少数不传 context 的应用）。
    fn can_compose(&self, pic: Option<&ITfContext>) -> bool {
        if pic.is_some() {
            return true;
        }
        let mgr = self
            .inner
            .thread_mgr
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let Some(mgr) = mgr else {
            return false;
        };
        // SAFETY: 只读查询焦点文档管理器，不涉及编辑会话
        unsafe {
            let Ok(doc) = mgr.GetFocus() else {
                return false;
            };
            doc.GetTop().is_ok()
        }
    }

    /// 按当前状态重画候选窗（位置取 last_pos）。
    ///
    /// 每次 lock 都是临时 guard，语句结束即释放——不会跨 TSF 调用持锁。
    fn refresh_cand_window(&self) {
        let pinyin = self
            .inner
            .buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let cands = self
            .inner
            .cands
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let page = *self.inner.page.lock().unwrap_or_else(|p| p.into_inner());
        let expanded = *self
            .inner
            .expanded
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let sel = *self.inner.sel.lock().unwrap_or_else(|p| p.into_inner());
        let pos = *self
            .inner
            .last_pos
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        show_cand_window(&pinyin, &cands, page, expanded, sel, pos);
    }

    /// 把文档选区移到组合串内部的第 `cursor` 个字符处，让光标"看得见"。
    ///
    /// 注意：**必须传入已经取出的 context**，不能在这里加锁——
    /// 锁一旦跨越 TSF 调用，就可能和同步回调（OnCompositionTerminated）互相等待。
    fn place_caret(
        &self,
        ctx: &ITfContext,
        ec: u32,
        comp_range: &windows::Win32::UI::TextServices::ITfRange,
        cursor: usize,
    ) -> windows::core::Result<()> {
        let mut scratch = [TF_SELECTION::default()];
        let mut fetched = 0u32;
        // SAFETY: 标准 TSF range 操作，全部在同一个编辑会话内
        unsafe {
            ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut scratch, &mut fetched)?;
            if let Some(s) = scratch[0].range.as_ref() {
                s.ShiftStartToRange(ec, comp_range, TF_ANCHOR_START)?;
                s.Collapse(ec, TF_ANCHOR_START)?;
                let mut shifted = 0i32;
                s.ShiftEnd(ec, cursor as i32, &mut shifted, std::ptr::null())?;
                s.Collapse(ec, TF_ANCHOR_END)?;
                let caret = [TF_SELECTION {
                    range: std::mem::ManuallyDrop::new(Some(s.clone())),
                    style: Default::default(),
                }];
                ctx.SetSelection(ec, &caret)?;
            }
        }
        Ok(())
    }
}

impl ITfEditSession_Impl for TextService_Impl {
    fn DoEditSession(&self, ec: u32) -> windows::core::Result<()> {
        let action = self
            .inner
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let Some(action) = action else {
            return Ok(());
        };

        // 取出副本后**立刻释放锁**：下面的 TSF 调用可能同步回调我们，
        // 持锁调用 = 死锁（鼠标点到别处导致组合终止时就必现）。
        let comp = self
            .inner
            .composition
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        // 和 poke_session 用同一个取法：AI 可能在一次键都没按过时发起（详见 current_context）
        let Some(ctx) = self.current_context() else {
            return Ok(());
        };

        match action {
            EditAction::Compose(text, cursor) => {
                let utf16: Vec<u16> = text.encode_utf16().collect();
                // 没有组合就新建一个
                let comp = match comp {
                    Some(c) => Some(c),
                    None => {
                        let mut sel = [TF_SELECTION::default()];
                        let mut fetched = 0u32;
                        // SAFETY: 标准 TSF 调用
                        unsafe {
                            ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)?;
                        }
                        match sel[0].range.as_ref() {
                            Some(range) => {
                                let sink: ITfCompositionSink = self.to_interface();
                                let ctx_comp: ITfContextComposition = ctx.cast()?;
                                // SAFETY: 标准 TSF 调用
                                Some(unsafe { ctx_comp.StartComposition(ec, range, &sink)? })
                            }
                            None => None,
                        }
                    }
                };
                if let Some(c) = comp {
                    let range = unsafe { c.GetRange()? };
                    unsafe { range.SetText(ec, 0, &utf16)? };
                    // 给组合串打上"输入中"的样式（应用据此画虚线，见 display.rs）
                    match display::mark(&ctx, ec, &range) {
                        "ok" | "no atom" => {}
                        why => log_line(&format!("display-attr {why}")),
                    }
                    self.place_caret(&ctx, ec, &range, cursor)?;
                    // 用 TSF 的 GetTextExt 问应用"我这段在屏幕哪"（和搜狗同款）
                    let pos = text_ext_pos(&ctx, ec, &range);
                    *self
                        .inner
                        .last_pos
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = pos;
                    self.refresh_cand_window();
                    // 写回（短锁，不跨越 TSF 调用）
                    *self
                        .inner
                        .composition
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(c);
                } else {
                    // 连组合都起不来（没有可写选区/应用不让改）：候选窗也别显示，
                    // 否则就是一个跟光标无关的飘窗
                    candwin::hide();
                }
                Ok(())
            }
            EditAction::MoveCaret(cursor) => {
                if let Some(c) = comp {
                    let range = unsafe { c.GetRange()? };
                    self.place_caret(&ctx, ec, &range, cursor)?;
                }
                Ok(())
            }
            EditAction::Commit(text) => {
                let utf16: Vec<u16> = text.encode_utf16().collect();
                if let Some(c) = comp {
                    let range = unsafe { c.GetRange()? };
                    // 上屏的字不能留虚线：样式要在定格前清掉
                    display::clear(&ctx, ec, &range);
                    unsafe { range.SetText(ec, 0, &utf16)? };
                    self.inner.self_terminating.store(true, Ordering::SeqCst);
                    let end_res = unsafe { c.EndComposition(ec) };
                    self.inner.self_terminating.store(false, Ordering::SeqCst);
                    end_res?;
                    // 结束组合只是"定格"文字，光标仍停在文本开头。
                    unsafe { range.Collapse(ec, TF_ANCHOR_END)? };
                    let sel = [TF_SELECTION {
                        range: std::mem::ManuallyDrop::new(Some(range)),
                        style: Default::default(),
                    }];
                    unsafe { ctx.SetSelection(ec, &sel)? };
                }
                *self
                    .inner
                    .composition
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = None;
                Ok(())
            }
            EditAction::PickCompose(picked, rest) => {
                // 第一步：把已选的字定格成真正的文本（结束组合），光标停到它后面。
                // 和 Commit 一样要打 self_terminating，否则 OnCompositionTerminated
                // 会把刚上屏的字擦掉。
                if let Some(c) = comp {
                    let pick16: Vec<u16> = picked.encode_utf16().collect();
                    let range = unsafe { c.GetRange()? };
                    // 定格的这几个字不要虚线（剩下的拼音会在新组合串里重新打上）
                    display::clear(&ctx, ec, &range);
                    unsafe { range.SetText(ec, 0, &pick16)? };
                    self.inner.self_terminating.store(true, Ordering::SeqCst);
                    let end_res = unsafe { c.EndComposition(ec) };
                    self.inner.self_terminating.store(false, Ordering::SeqCst);
                    end_res?;
                    unsafe { range.Collapse(ec, TF_ANCHOR_END)? };
                    let sel = [TF_SELECTION {
                        range: std::mem::ManuallyDrop::new(Some(range)),
                        style: Default::default(),
                    }];
                    unsafe { ctx.SetSelection(ec, &sel)? };
                }
                *self
                    .inner
                    .composition
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = None;

                // 第二步：在光标处开一个新的组合串装剩下的拼音
                let mut sel = [TF_SELECTION::default()];
                let mut fetched = 0u32;
                // SAFETY: 标准 TSF 调用
                unsafe {
                    ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)?;
                }
                let Some(start_range) = sel[0].range.as_ref() else {
                    return Ok(());
                };
                let sink: ITfCompositionSink = self.to_interface();
                let ctx_comp: ITfContextComposition = ctx.cast()?;
                // SAFETY: 标准 TSF 调用
                let c = unsafe { ctx_comp.StartComposition(ec, start_range, &sink)? };
                let range = unsafe { c.GetRange()? };
                // 剩下的拼音也要按音节显示分隔符（和主组合串一致）
                let (rest_view, rest_cursor) = pinyin::compose_view(&rest, rest.len());
                let rest16: Vec<u16> = rest_view.encode_utf16().collect();
                unsafe { range.SetText(ec, 0, &rest16)? };
                display::mark(&ctx, ec, &range);
                self.place_caret(&ctx, ec, &range, rest_cursor)?;
                let pos = text_ext_pos(&ctx, ec, &range);
                *self
                    .inner
                    .last_pos
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = pos;
                self.refresh_cand_window();
                *self
                    .inner
                    .composition
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(c);
                Ok(())
            }
            EditAction::Cancel => {
                if let Some(c) = comp {
                    let range = unsafe { c.GetRange()? };
                    // 先清空文本再结束组合，否则拼音会被当成普通文字留在文档里
                    unsafe { range.SetText(ec, 0, &[])? };
                    self.inner.self_terminating.store(true, Ordering::SeqCst);
                    let end_res = unsafe { c.EndComposition(ec) };
                    self.inner.self_terminating.store(false, Ordering::SeqCst);
                    end_res?;
                }
                *self
                    .inner
                    .composition
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = None;
                Ok(())
            }
            EditAction::Insert(text) => {
                let why = insert_at_caret(&ctx, ec, &text);
                log_line(&format!("insert '{text}' -> {why}"));
                if why != "ok" {
                    // TSF 这条路不通，退回注入字符
                    ipc::send_text(&text);
                }
                Ok(())
            }
            EditAction::InsertPunct(full, half) => {
                // 数字后面要半角（`3.14` `10:30` `1,000`），其余照旧全角。
                let digit = preceding_char_is_digit(&ctx, ec);
                let text = if digit { half } else { full };
                let why = insert_at_caret(&ctx, ec, &text);
                log_line(&format!(
                    "insert-punct '{text}' digit_before={digit} -> {why}"
                ));
                if why != "ok" {
                    ipc::send_text(&text);
                }
                Ok(())
            }
            EditAction::DeleteBack => {
                // 误选撤回时自己删字。
                //
                // 为什么不让应用删：我们在 `OnTestKeyDown` 里认领了退格之后，
                // 有的应用（微信、记事本）就再也收不到这个键了 ——
                // 用户看到的是"按了两下没反应，第三下才删掉一个字"。
                // 所以这里自己把光标前一个字删掉，`OnKeyDown` 返回 TRUE 把键吃掉。
                let mut sel = [TF_SELECTION::default()];
                let mut fetched = 0u32;
                let mut why = "ok";
                // 连按了好几下退格就一次删完（计数器由 OnKeyDown 累加）
                let want = {
                    let mut n = self
                        .inner
                        .pending_backspace
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    let v = *n;
                    *n = 0;
                    v
                };
                let want = want.max(1) as i32;
                // SAFETY: 标准 TSF range 操作，全部在同一个编辑会话内
                unsafe {
                    match ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched) {
                        Err(_) => why = "GetSelection failed",
                        Ok(()) => match sel[0].range.as_ref() {
                            None => why = "no range",
                            Some(range) => {
                                // 三级级联，每级都要**确认真的删掉了**才收手：
                                //
                                // ① `ITfRangeACP` 精确指定区间（最干净）。
                                // ② `Collapse + ShiftStart`（1.0.5/1.0.6 用的就是它）——
                                //    在某些宿主上返回 S_OK 却一步都没移动，所以必须
                                //    检查 `shifted`，不然 `SetText(&[])` 作用在空区间上，
                                //    日志写 ok、界面一个字都没少。
                                // ③ 注入原生 `VK_BACK`，让宿主编按自己的逻辑删。
                                //    WPS 的正文控件在 ① 里 `GetExtent` 给 (0,0)、
                                //    在 ② 里原地不动，只剩这条路 —— 代价是这个键会
                                //    再回到我们的 `OnKeyDown`，所以要记抑制次数。
                                let mut done = false;
                                if let Ok(acp) = range.cast::<ITfRangeACP>() {
                                    let (mut anchor, mut cch) = (0i32, 0i32);
                                    if acp.GetExtent(&mut anchor, &mut cch).is_err() {
                                        why = "GetExtent failed";
                                    } else if anchor == 0 && cch == 0 {
                                        // 分不清"宿主不认 ACP"和"光标真在文档开头"，
                                        // 但光标在开头的情况我们本来就没什么可删，
                                        // 让后面两级去试，代价可以接受。
                                        log_line("delete-back acp extent=0 (host ignores ACP)");
                                        why = "acp-extent-zero";
                                    } else {
                                        // 有选中内容就删选中；只有一个光标就往前吃 want 个字。
                                        let (from, len) = if cch > 0 {
                                            (anchor, cch)
                                        } else {
                                            let from =
                                                if anchor - want < 0 { 0 } else { anchor - want };
                                            (from, anchor - from)
                                        };
                                        if len <= 0 {
                                            why = "nothing to delete";
                                        } else if acp.SetExtent(from, len).is_err() {
                                            why = "SetExtent failed";
                                        } else {
                                            log_line(&format!(
                                                "delete-back acp anchor={anchor} cch={cch} from={from} len={len} want={want}"
                                            ));
                                            if range.SetText(ec, 0, &[]).is_err() {
                                                why = "SetText failed";
                                            } else {
                                                done = true;
                                                why = "ok";
                                            }
                                        }
                                    }
                                } else {
                                    why = "no acp";
                                }
                                if !done {
                                    // ② ShiftStart，必须真的移动了才算数
                                    let mut shifted = 0i32;
                                    let moved = range.Collapse(ec, TF_ANCHOR_START).is_ok()
                                        && range
                                            .ShiftStart(ec, -want, &mut shifted, std::ptr::null())
                                            .is_ok()
                                        && shifted != 0;
                                    if moved && range.SetText(ec, 0, &[]).is_ok() {
                                        log_line(&format!(
                                            "delete-back shiftstart shifted={shifted}"
                                        ));
                                        done = true;
                                        why = "ok (shiftstart)";
                                    } else {
                                        log_line(&format!(
                                            "delete-back shiftstart failed shifted={shifted}"
                                        ));
                                    }
                                }
                                if !done {
                                    // ③ 交给宿主：注入原生退格
                                    log_line(&format!(
                                        "delete-back inject native want={want} (why={why})"
                                    ));
                                    mark_injected_backspace(want as usize);
                                    crate::ipc::press_backspace(want as usize);
                                    why = "injected native backspace";
                                }
                                if done {
                                    let _ = range.Collapse(ec, TF_ANCHOR_END);
                                    let caret = [TF_SELECTION {
                                        range: std::mem::ManuallyDrop::new(Some(range.clone())),
                                        style: Default::default(),
                                    }];
                                    if ctx.SetSelection(ec, &caret).is_err() {
                                        why = "SetSelection failed";
                                    }
                                }
                            }
                        },
                    }
                }
                log_line(&format!("delete-back -> {why}"));
                Ok(())
            }
            EditAction::ReadSelection(action) => {
                // 读选区必须在编辑会话里做（要 ec）。
                // `ACTION_READ` = 面板打开时只读一次，把文字交给面板显示，不发请求。
                let text = selection_text(&ctx, ec);
                let chars = text.chars().count();
                if action == ai::ACTION_READ {
                    if chars > ai::MAX_INPUT_CHARS {
                        ai::on_selection("");
                        log_line(&format!("ai: 选区太长（{chars} 字），面板不处理"));
                    } else {
                        ai::on_selection(&text);
                    }
                    return Ok(());
                }
                if chars == 0 || text.trim().is_empty() {
                    log_line("ai: 没有选中文字");
                    return Ok(());
                }
                if chars > ai::MAX_INPUT_CHARS {
                    log_line(&format!(
                        "ai: 选区太长（{chars} 字 > {}），不做",
                        ai::MAX_INPUT_CHARS
                    ));
                    return Ok(());
                }
                if ai::start(action, &text).is_none() {
                    log_line("ai: 请求没发出去");
                }
                Ok(())
            }
            EditAction::AiApply {
                action,
                expect,
                text,
            } => {
                let (why, end_anchor) = apply_ai_result(&ctx, ec, action, &expect, &text);
                if why == "ok" {
                    // 续写只需要删掉插入的那段；翻译/改写还要把原文写回去
                    let original = if ai::replaces_selection(action) {
                        expect.as_str()
                    } else {
                        ""
                    };
                    let inserted = text.encode_utf16().count() as i32;
                    if let Some(anchor) = end_anchor {
                        self.arm_ai_undo(original, inserted, anchor);
                    }
                }
                log_line(&format!(
                    "ai: apply action={} -> {why}",
                    ai::action_name(action)
                ));
                Ok(())
            }
            EditAction::AiRevert {
                original,
                inserted,
                end_anchor,
            } => {
                let why = revert_ai_result(&ctx, ec, &original, inserted, end_anchor);
                log_line(&format!("ai: revert -> {why}"));
                Ok(())
            }
        }
    }
}

impl ITfCompositionSink_Impl for TextService_Impl {
    fn OnCompositionTerminated(
        &self,
        ecwrite: u32,
        pcomposition: Ref<ITfComposition>,
    ) -> windows::core::Result<()> {
        // 外部终止（鼠标点到别处 / 切窗口）：把残留的拼音文本清掉，等于"关掉这次输入"。
        // 我们自己 EndComposition（提交/取消）时不能清——那会把上屏的中文一起擦掉。
        if !self.inner.self_terminating.load(Ordering::SeqCst) {
            if let Some(c) = pcomposition.as_ref() {
                // SAFETY: 当前处于写会话，允许改文本
                unsafe {
                    if let Ok(range) = c.GetRange() {
                        let _ = range.SetText(ecwrite, 0, &[]);
                    }
                }
            }
            self.inner
                .buffer
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            self.inner
                .cands
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            *self
                .inner
                .matched_len
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.sel.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.page.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self
                .inner
                .expanded
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = false;
            candwin::hide();
            self.sentence_clear();
        }
        *self
            .inner
            .composition
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, fforeground: BOOL) -> windows::core::Result<()> {
        if !fforeground.as_bool() {
            // 失焦（切窗口 / 鼠标点到别处）：清掉组合状态，隐藏候选窗。
            // 组合本身由 TSF 结束，会走 OnCompositionTerminated。
            self.inner
                .buffer
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            self.inner
                .cands
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
            *self
                .inner
                .matched_len
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.sel.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.page.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self
                .inner
                .expanded
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = false;
            candwin::hide();
            self.sentence_clear();
        }
        Ok(())
    }

    fn OnTestKeyDown(
        &self,
        pic: Ref<ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> windows::core::Result<BOOL> {
        let vk = wparam.0 as u32;
        const VK_BACK: u32 = 0x08;
        // 这个退格是我们自己注入的（见 EditAction::DeleteBack 的第三级）：
        // 不认领，让应用收下它去删字。
        if vk == VK_BACK && injected_backspace_pending() {
            log_line("test vk=8 claim=false (injected)");
            return Ok(BOOL(0));
        }
        // 「这次按键是 Shift 组合键」这件事必须在这里就记下：
        // 下面可能一个键都不认领（`Shift+=` → `+` 交给应用、英文模式下所有键都放行），
        // 那种情况下 TSF 不会再调 OnKeyDown，只在 OnKeyDown 里记就会漏掉，
        // 结果松开 Shift 被当成"单击"切换中英文（打个 `+` 就变英文）。
        if is_shift_chord(vk, shift_held(), ctrl_alt_win_held()) {
            self.inner.shift_used.store(true, Ordering::SeqCst);
        }
        let composing = !self
            .inner
            .buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty();
        // 刚上屏、可能被撤回的词，要先把退格抢过来数一下
        let undo = !composing && (self.undo_pending() || self.ai_undo_pending());
        // 全局状态（注册表）：中英模式 + 中文标点开关，所有应用一致。
        let (chinese_mode, punct_mode) = state::snapshot();
        // 标点开关关掉时，空闲态干脆不认领标点键：应用自己出半角标点。
        // 这比"认领下来再转回半角"干净得多 —— 后者要猜键盘布局，还要自己 SetText。
        let punct_passthrough = !punct_mode && !composing;
        let claim = self.can_compose(pic.as_ref())
            && ((should_claim(vk, composing, shift_held(), caps_on(), !chinese_mode)
                && !(punct_passthrough && is_punct_key(vk, shift_held())))
                || (undo && vk == VK_BACK));
        log_line(&format!("test vk={vk} claim={claim} composing={composing}"));
        Ok(BOOL(if claim { 1 } else { 0 }))
    }

    fn OnKeyDown(
        &self,
        pic: Ref<ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> windows::core::Result<BOOL> {
        const VK_BACK: u32 = 0x08;
        const VK_RETURN: u32 = 0x0D;
        const VK_ESCAPE: u32 = 0x1B;
        const VK_SPACE: u32 = 0x20;
        const VK_SHIFT: u32 = 0x10;
        const VK_CAPITAL: u32 = 0x14;
        const VK_DELETE: u32 = 0x2E;
        const VK_0: u32 = 0x30;
        const VK_9: u32 = 0x39;
        const VK_A: u32 = 0x41;
        const VK_Z: u32 = 0x5A;
        const VK_LEFT: u32 = 0x25;
        const VK_RIGHT: u32 = 0x27;
        const VK_HOME: u32 = 0x24;
        const VK_END: u32 = 0x23;
        const VK_UP: u32 = 0x26;
        const VK_DOWN: u32 = 0x28;
        const VK_OEM_MINUS: u32 = 0xBD;
        const VK_OEM_PLUS: u32 = 0xBB;
        const VK_OEM_COMMA: u32 = 0xBC;
        const VK_OEM_PERIOD: u32 = 0xBE;
        const VK_PRIOR: u32 = 0x21;
        const VK_NEXT: u32 = 0x22;

        let vk = wparam.0 as u32;

        // 自己注入的退格：放行给应用（见 EditAction::DeleteBack 的第三级）
        if vk == VK_BACK && take_injected_backspace() {
            log_line("keydown vk=8 pass-through (injected)");
            return Ok(BOOL(0));
        }

        // 光标不在可编辑区域时一个键都不认领，否则会在没有光标的地方冒出候选窗
        let composable = self.can_compose(pic.as_ref());

        // 存下输入上下文，编辑会话要用
        self.store_context(pic);

        if !composable {
            return Ok(BOOL(0));
        }

        // ── 误选撤回 ──
        // 上屏后 2 秒内按退格，把刚上屏的几个字删光就说明那一下选错了：
        // 把刚才记的学习减掉。
        //
        // 这段窗口里退格由**我们**执行（`EditAction::DeleteBack` 自己删一个字），
        // 因为 `OnTestKeyDown` 认领了之后有的应用就收不到这个键了 ——
        // 之前的表现是"按两下没反应，第三下才删掉"。窗口之外的退格不认领、
        // 也不经这里，应用照常自己删。
        if vk == VK_BACK {
            let composing = !self
                .inner
                .buffer
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty();
            if !composing {
                // AI 结果优先还账：刚写回两秒内按退格 = 把那段换成原文。
                if let Some((original, inserted, end_anchor)) = self.take_ai_undo() {
                    self.clear_commit_mark();
                    log_line(&format!("ai-undo requested inserted={inserted}"));
                    self.request_edit(EditAction::AiRevert {
                        original,
                        inserted,
                        end_anchor,
                    });
                    return Ok(BOOL(1));
                }
                let had_mark = self.undo_pending();
                for (code, word) in self.count_backspace() {
                    log_line(&format!("unlearn code={code} word={word}"));
                    ipc::unlearn(&code, &word);
                }
                if had_mark {
                    // 连按两下时异步编辑会话可能还没跑起来，pending 只有一个槽，
                    // 所以把次数攒在计数器里，会话一次按次数删完。
                    {
                        let mut n = self
                            .inner
                            .pending_backspace
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        *n += 1;
                    }
                    self.request_edit(EditAction::DeleteBack);
                    return Ok(BOOL(1));
                }
                return Ok(BOOL(0));
            }
        } else {
            self.clear_commit_mark();
            self.clear_ai_undo();
        }

        // Ctrl/Alt/Win 组合键：整体放行给应用（Ctrl+A/C/V/X/Z 等快捷键）。
        // 注意必须放在 OnKeyDown 里——fForeground=TRUE 的接收器，TSF 会跳过
        // OnTestKeyDown 直接调 OnKeyDown，所以修饰键检查放 OnTestKeyDown 是死代码。
        if ctrl_alt_win_held() {
            return Ok(BOOL(0));
        }

        // Shift 期间按过任何别的键（包括 ? ! : " 这类要 Shift 的标点，
        // 也包括我们根本不认领的 + - _ =），就说明这不是"单击切换中英文"，
        // 只是用 Shift 打字符。
        // 少了这一条，打 `?`（Shift+/）或 `+`（Shift+=）会顺便把输入法切到英文。
        if is_shift_chord(vk, shift_held(), ctrl_alt_win_held()) {
            self.inner.shift_used.store(true, Ordering::SeqCst);
        }

        // 英文模式：所有键原样交给应用（Shift 除外，用 Shift 单击切回中文）。
        // 状态是全局的（注册表），所有应用看到的是同一个中英模式 —— 见 state.rs。
        if !state::chinese() {
            const VK_SHIFT_KEY: u32 = 0x10;
            if vk == VK_SHIFT_KEY {
                // 让 OnKeyUp 去处理切换
                self.inner.shift_used.store(false, Ordering::SeqCst);
            }
            return Ok(BOOL(0));
        }

        // 标点键：中文模式下转成中文标点。
        // - 空闲时：直接插入标点
        // - 拼写中：先上屏当前候选，再插入标点（主流输入法行为）
        //   例外：`-` / `=` 不按 Shift 时仍是翻页键
        let composing_now = !self
            .inner
            .buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty();

        // 拼写中打 `-` / `_`：这是拼音里的分词符（写标识符用），
        // 直接进组合串，像字母一样参与拼写。
        //   xiang-mu → 输入法里显示 xiang-mu，候选照常出，回车原样上屏 xiang-mu
        // 不在这里拦，就会被当成中文标点，把「想」提交进文章（v20 的错误）。
        // 注意：分隔符只影响显示与最终原样上屏，查询引擎前会去掉（见 query_with_fallback）。
        if composing_now {
            // 展开成 5×5 时，`-` / `=` 是翻页键（用户要求"加减键切换行数"）；
            // 收起时 `-` 才是拼音分词符。展开态是临时的导航态（打字会收起），
            // 所以按 `-` 只可能是在翻候选。
            let is_expanded = {
                let e = self
                    .inner
                    .expanded
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                *e
            };
            let sep = if is_expanded {
                None
            } else {
                match (vk, shift_held()) {
                    (VK_OEM_MINUS, true) => Some('_'),
                    (VK_OEM_MINUS, false) => Some('-'),
                    _ => None,
                }
            };
            if let Some(sep) = sep {
                let mut buffer = self.inner.buffer.lock().unwrap_or_else(|p| p.into_inner());
                let mut cands = self.inner.cands.lock().unwrap_or_else(|p| p.into_inner());
                let mut matched_len = self
                    .inner
                    .matched_len
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let mut cursor = self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner());
                let mut page = self.inner.page.lock().unwrap_or_else(|p| p.into_inner());
                let mut expanded = self
                    .inner
                    .expanded
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let mut sel = self.inner.sel.lock().unwrap_or_else(|p| p.into_inner());
                let at = (*cursor).min(buffer.len());
                buffer.insert(at, sep);
                *cursor = at + 1;
                (*cands, *matched_len) = query_with_fallback(&buffer);
                *page = 0;
                *expanded = false;
                *sel = 0;
                log_line(&format!(
                    "compose(sep {sep}) buffer={} cursor={} cands={}",
                    *buffer,
                    *cursor,
                    cands.len()
                ));
                show_cand_window(
                    &buffer,
                    &cands,
                    *page,
                    *expanded,
                    *sel,
                    *self
                        .inner
                        .last_pos
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()),
                );
                self.request_edit(EditAction::Compose(buffer.clone(), *cursor));
                return Ok(BOOL(1));
            }
        }

        if !caps_on() {
            let shift = shift_held();
            let is_paging = (vk == VK_OEM_MINUS || vk == VK_OEM_PLUS) && !shift;
            if !is_paging {
                // `-` `=` `_` `+` 四个键**永远出半角**（Excel 公式、代码、算式要原样字符，
                // 全角的 `－＝＋` 会让 `=A1-B1` 这类公式直接失效），其余走中文标点表。
                // 两者共用下面"先上屏候选再插符号"的流程。
                let punct: Option<String> = match halfwidth_symbol(vk, shift) {
                    Some(half) => Some(half.to_string()),
                    None => cn_punct(vk, shift).map(resolve_punct),
                };
                if let Some(ch) = punct {
                    // 中文标点关掉、而且当前没有在拼写：放行给应用自己出半角标点。
                    // 拼写中仍然照旧（那一刻你在选中文词，标点跟着中文走才连贯）。
                    if !composing_now && !state::punctuation() {
                        log_line(&format!("punct vk={vk:#x} passthrough (punct off)"));
                        return Ok(BOOL(0));
                    }
                    if composing_now {
                        // 先上屏候选，再跟上标点
                        let mut buffer =
                            self.inner.buffer.lock().unwrap_or_else(|p| p.into_inner());
                        let mut cands = self.inner.cands.lock().unwrap_or_else(|p| p.into_inner());
                        let mut matched_len = self
                            .inner
                            .matched_len
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        let mut cursor =
                            self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner());
                        let mut page = self.inner.page.lock().unwrap_or_else(|p| p.into_inner());
                        let mut expanded = self
                            .inner
                            .expanded
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        let mut sel = self.inner.sel.lock().unwrap_or_else(|p| p.into_inner());
                        let idx = *page * page_size(*expanded) + *sel;
                        let head = cands.get(idx).cloned().unwrap_or_default();
                        let text = format!("{head}{ch}");
                        let code = buffer.clone();
                        buffer.clear();
                        cands.clear();
                        *matched_len = 0;
                        *cursor = 0;
                        *page = 0;
                        *expanded = false;
                        *sel = 0;
                        candwin::hide();
                        log_line(&format!("punct-in-compose vk={vk:#x} -> {text}"));
                        if !head.is_empty() && crate::config::snapshot().learn {
                            ipc::select(idx, &code, &head);
                        }
                        self.request_edit(EditAction::Commit(text));
                    } else {
                        // 数字后面要半角（`3.14`、`10:30`、`1,000`）：
                        // 光标前一个字符得在编辑会话里才看得到，这里只把两种写法都传下去。
                        match digit_pair(vk, shift) {
                            Some((full, half)) => {
                                log_line(&format!("punct vk={vk:#x} -> digit-aware"));
                                self.request_edit(EditAction::InsertPunct(
                                    full.to_string(),
                                    half.to_string(),
                                ));
                            }
                            None => {
                                log_line(&format!("punct vk={vk:#x} -> {ch}"));
                                self.request_edit(EditAction::Insert(ch));
                            }
                        }
                    }
                    return Ok(BOOL(1));
                }
            }
        }

        let mut buffer = self.inner.buffer.lock().unwrap_or_else(|p| p.into_inner());
        let mut cands = self.inner.cands.lock().unwrap_or_else(|p| p.into_inner());
        let mut matched_len = self
            .inner
            .matched_len
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut cursor = self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner());
        let mut page = self.inner.page.lock().unwrap_or_else(|p| p.into_inner());
        let mut expanded = self
            .inner
            .expanded
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let mut sel = self.inner.sel.lock().unwrap_or_else(|p| p.into_inner());
        let composing = !buffer.is_empty();

        match vk {
            // 字母：插入到内部光标处（拼音全 ASCII，字节索引即字符索引）
            VK_A..=VK_Z => {
                // 空闲 + Shift/CapsLock：是要打英文大写，放行给应用
                if !composing && shift_or_caps() {
                    log_line(&format!(
                        "letter passthrough vk={vk:#x} shift={} caps={}",
                        shift_held(),
                        caps_on()
                    ));
                    if shift_held() {
                        self.inner.shift_used.store(true, Ordering::SeqCst);
                    }
                    return Ok(BOOL(0));
                }
                let at = (*cursor).min(buffer.len());
                if !composing_now {
                    // 新起一句：上一条没打完的整句记录作废
                    self.sentence_clear();
                }
                buffer.insert(at, (vk as u8 as char).to_ascii_lowercase());
                *cursor = at + 1;
                (*cands, *matched_len) = query_with_fallback(&buffer);
                *page = 0;
                *expanded = false;
                *sel = 0;
                log_line(&format!(
                    "compose buffer={} cursor={} cands={} matched={} top={:?} caps={} shift={}",
                    *buffer,
                    *cursor,
                    cands.len(),
                    *matched_len,
                    cands.first(),
                    caps_on(),
                    shift_held()
                ));
                // 显示候选窗
                show_cand_window(
                    &buffer,
                    &cands,
                    *page,
                    *expanded,
                    *sel,
                    *self
                        .inner
                        .last_pos
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()),
                );
                // 把拼音写到文档里（组合串）
                self.request_edit(EditAction::Compose(buffer.clone(), *cursor));
                Ok(BOOL(1))
            }
            // 退格：删内部光标前一个字母；缓冲空时放行给应用
            VK_BACK => {
                if !buffer.is_empty() && *cursor > 0 {
                    let at = (*cursor).min(buffer.len());
                    buffer.remove(at - 1);
                    *cursor = at - 1;
                    (*cands, *matched_len) = query_with_fallback(&buffer);
                    *page = 0;
                    *expanded = false;
                    *sel = 0;
                    if buffer.is_empty() {
                        candwin::hide();
                        self.sentence_clear();
                        self.request_edit(EditAction::Cancel);
                    } else {
                        show_cand_window(
                            &buffer,
                            &cands,
                            *page,
                            *expanded,
                            *sel,
                            *self
                                .inner
                                .last_pos
                                .lock()
                                .unwrap_or_else(|p| p.into_inner()),
                        );
                        self.request_edit(compose_action(&buffer, *cursor));
                    }
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // Delete：删内部光标处那个字母（向前删）
            VK_DELETE => {
                if !buffer.is_empty() && *cursor < buffer.len() {
                    let at = *cursor;
                    buffer.remove(at);
                    (*cands, *matched_len) = query_with_fallback(&buffer);
                    *page = 0;
                    *expanded = false;
                    *sel = 0;
                    if buffer.is_empty() {
                        candwin::hide();
                        self.sentence_clear();
                        self.request_edit(EditAction::Cancel);
                    } else {
                        show_cand_window(
                            &buffer,
                            &cands,
                            *page,
                            *expanded,
                            *sel,
                            *self
                                .inner
                                .last_pos
                                .lock()
                                .unwrap_or_else(|p| p.into_inner()),
                        );
                        self.request_edit(compose_action(&buffer, *cursor));
                    }
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // 左键：展开时在网格里左移一列（行首则上一页）；收起时移动拼音光标
            VK_LEFT => {
                if composing {
                    if *expanded {
                        let size = page_size(true);
                        if *sel % grid_cols() > 0 {
                            *sel -= 1;
                        } else if *page > 0 {
                            *page -= 1;
                            let page_start = *page * size;
                            let page_len = cands.len().saturating_sub(page_start).min(size);
                            *sel = page_len.saturating_sub(1);
                        }
                        show_cand_window(
                            &buffer,
                            &cands,
                            *page,
                            *expanded,
                            *sel,
                            *self
                                .inner
                                .last_pos
                                .lock()
                                .unwrap_or_else(|p| p.into_inner()),
                        );
                    } else if *cursor > 0 {
                        *cursor -= 1;
                        self.request_edit(EditAction::MoveCaret(*cursor));
                    }
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // 右键：展开时右移一列（行末则下一页）；收起时移动拼音光标
            VK_RIGHT => {
                if composing {
                    if *expanded {
                        let size = page_size(true);
                        let page_start = *page * size;
                        let page_len = cands.len().saturating_sub(page_start).min(size);
                        let total_pages = (cands.len() + size - 1) / size;
                        if *sel % grid_cols() < grid_cols() - 1 && *sel + 1 < page_len {
                            *sel += 1;
                        } else if *page + 1 < total_pages {
                            *page += 1;
                            *sel = 0;
                        }
                        show_cand_window(
                            &buffer,
                            &cands,
                            *page,
                            *expanded,
                            *sel,
                            *self
                                .inner
                                .last_pos
                                .lock()
                                .unwrap_or_else(|p| p.into_inner()),
                        );
                    } else {
                        let len = buffer.len();
                        if *cursor < len {
                            *cursor += 1;
                            self.request_edit(EditAction::MoveCaret(*cursor));
                        }
                    }
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // Home/End：跳到组合串首/尾
            VK_HOME => {
                if composing {
                    *cursor = 0;
                    self.request_edit(EditAction::MoveCaret(*cursor));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            VK_END => {
                if composing {
                    *cursor = buffer.len();
                    self.request_edit(EditAction::MoveCaret(*cursor));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // 上键：展开时上移一行（首行则上一页）；收起时不动
            VK_UP => {
                if composing {
                    if *expanded {
                        let size = page_size(true);
                        if *sel >= grid_cols() {
                            *sel -= grid_cols();
                        } else if *page > 0 {
                            *page -= 1;
                            let page_start = *page * size;
                            let page_len = cands.len().saturating_sub(page_start).min(size);
                            *sel = page_len.saturating_sub(1);
                        }
                        show_cand_window(
                            &buffer,
                            &cands,
                            *page,
                            *expanded,
                            *sel,
                            *self
                                .inner
                                .last_pos
                                .lock()
                                .unwrap_or_else(|p| p.into_inner()),
                        );
                    }
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // 下键：展开时下移一行（末行则下一页）；收起时展开成 5×5
            VK_DOWN => {
                if composing {
                    if !*expanded {
                        *expanded = true;
                        *page = 0;
                        *sel = 0;
                    } else {
                        let size = page_size(true);
                        let page_start = *page * size;
                        let page_len = cands.len().saturating_sub(page_start).min(size);
                        let total_pages = (cands.len() + size - 1) / size;
                        if *sel + grid_cols() < page_len {
                            *sel += grid_cols();
                        } else if *page + 1 < total_pages {
                            *page += 1;
                            *sel = 0;
                        }
                    }
                    show_cand_window(
                        &buffer,
                        &cands,
                        *page,
                        *expanded,
                        *sel,
                        *self
                            .inner
                            .last_pos
                            .lock()
                            .unwrap_or_else(|p| p.into_inner()),
                    );
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // PageUp / `-`：上一页（展开时首页则收起）
            // `-` 拼写中已改为分词符，这里只剩 PageUp 实际可达，留着不碍事。
            VK_OEM_MINUS | VK_PRIOR => {
                if composing {
                    if *expanded {
                        if *page > 0 {
                            *page -= 1;
                        } else {
                            *expanded = false;
                        }
                    } else if *page > 0 {
                        *page -= 1;
                    }
                    *sel = 0;
                    show_cand_window(
                        &buffer,
                        &cands,
                        *page,
                        *expanded,
                        *sel,
                        *self
                            .inner
                            .last_pos
                            .lock()
                            .unwrap_or_else(|p| p.into_inner()),
                    );
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // PageDown / `=`：下一页（收起时先展开）
            VK_OEM_PLUS | VK_NEXT => {
                if composing {
                    if !*expanded {
                        *expanded = true;
                        *page = 0;
                    } else {
                        let size = page_size(true);
                        let total_pages = (cands.len() + size - 1) / size;
                        if *page + 1 < total_pages {
                            *page += 1;
                        }
                    }
                    *sel = 0;
                    show_cand_window(
                        &buffer,
                        &cands,
                        *page,
                        *expanded,
                        *sel,
                        *self
                            .inner
                            .last_pos
                            .lock()
                            .unwrap_or_else(|p| p.into_inner()),
                    );
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // 空格：上屏当前选中的候选（选完可能还剩拼音，留给 apply_pick 判断）
            VK_SPACE => {
                let pick_idx = *page * page_size(*expanded) + *sel;
                let pos = *self
                    .inner
                    .last_pos
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                let code_before = buffer.clone();
                match apply_pick(
                    pick_idx,
                    &mut cands,
                    &mut buffer,
                    &mut cursor,
                    &mut matched_len,
                    &mut page,
                    &mut expanded,
                    &mut sel,
                    pos,
                ) {
                    Some(action) => {
                        log_line(&format!("pick {pick_idx} buffer={buffer}"));
                        Ok(self.finish_pick(&code_before, action))
                    }
                    None => Ok(BOOL(0)),
                }
            }
            // 回车：把当前拼音原样上屏（等于直接打英文），是打错后的恢复通道
            VK_RETURN => {
                if !buffer.is_empty() {
                    let raw = buffer.clone();
                    buffer.clear();
                    cands.clear();
                    *matched_len = 0;
                    *cursor = 0;
                    candwin::hide();
                    self.sentence_clear();
                    log_line(&format!("commit-raw {raw}"));
                    self.request_edit(EditAction::Commit(raw));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // Shift：**按下时什么也不做**，交给 OnKeyUp 判断是不是"单击"。
            // 原因：按 Shift 常常是为了接着按组合键（`_` 就是 Shift+-），
            // 按下就提交拼音会把组合键的第二个键（-）的上下文弄丢。
            // CapsLock 不是修饰键，仍按原行为处理。
            VK_SHIFT => Ok(BOOL(0)),
            VK_CAPITAL => {
                if !buffer.is_empty() {
                    let raw = buffer.clone();
                    buffer.clear();
                    cands.clear();
                    *matched_len = 0;
                    *cursor = 0;
                    *sel = 0;
                    *page = 0;
                    *expanded = false;
                    candwin::hide();
                    self.sentence_clear();
                    log_line(&format!("commit-raw(caps) {raw}"));
                    self.request_edit(EditAction::Commit(raw));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // 数字键：选**当前这一行**的第 N 个候选
            //
            // 展开成 5×5 后不再从 1 标到 25 —— 只给选中那一行标 1~5，
            // 上下键换行，左右键/数字键在行内选（用户要求）。
            VK_0..=VK_9 => {
                if buffer.is_empty() {
                    return Ok(BOOL(0));
                }
                let idx = (vk - VK_0) as usize;
                if idx >= 1 && idx <= grid_cols() {
                    let row = *sel / grid_cols();
                    let local = row * grid_cols() + (idx - 1);
                    let page_len = cands
                        .len()
                        .saturating_sub(*page * page_size(*expanded))
                        .min(page_size(*expanded));
                    if local < page_len {
                        *sel = local;
                        let global = *page * page_size(*expanded) + local;
                        let pos = *self
                            .inner
                            .last_pos
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        let code_before = buffer.clone();
                        if let Some(action) = apply_pick(
                            global,
                            &mut cands,
                            &mut buffer,
                            &mut cursor,
                            &mut matched_len,
                            &mut page,
                            &mut expanded,
                            &mut sel,
                            pos,
                        ) {
                            log_line(&format!("pick(number {idx}) buffer={buffer}"));
                            return Ok(self.finish_pick(&code_before, action));
                        }
                    }
                }
                Ok(BOOL(1))
            }
            // Esc：取消这次输入
            VK_ESCAPE => {
                let had = !buffer.is_empty();
                buffer.clear();
                cands.clear();
                *matched_len = 0;
                *cursor = 0;
                candwin::hide();
                self.sentence_clear();
                if had {
                    self.request_edit(EditAction::Cancel);
                }
                Ok(BOOL(if had { 1 } else { 0 }))
            }
            _ => Ok(BOOL(0)),
        }
    }

    fn OnTestKeyUp(
        &self,
        _pic: Ref<ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> windows::core::Result<BOOL> {
        // 必须认领 key-up：TSF 只在 OnTestKeyUp 返回 TRUE 时才调 OnKeyUp，
        // 而 Shift 单击切换中英文就发生在 OnKeyUp 里。
        // （微信里不认领 = Shift 永远切不了英文，DSH 里碰巧会直接调所以没暴露）
        Ok(BOOL(1))
    }

    fn OnKeyUp(
        &self,
        _pic: Ref<ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> windows::core::Result<BOOL> {
        const VK_SHIFT: u32 = 0x10;
        let vk = wparam.0 as u32;
        if vk == VK_SHIFT {
            // Shift 单击（按下期间没打字）→ 切换中英文模式
            if !self.inner.shift_used.load(Ordering::SeqCst) {
                // Shift 单击（按下期间没按别的键）：
                // - 正在拼写 → 拼音原样上屏 + 进英文模式（对齐搜狗）
                // - 空闲     → 切换中英文
                let composing = !self
                    .inner
                    .buffer
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_empty();
                if composing {
                    let raw = self
                        .inner
                        .buffer
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone();
                    self.inner
                        .buffer
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clear();
                    self.inner
                        .cands
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clear();
                    *self
                        .inner
                        .matched_len
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = 0;
                    *self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner()) = 0;
                    *self.inner.sel.lock().unwrap_or_else(|p| p.into_inner()) = 0;
                    *self.inner.page.lock().unwrap_or_else(|p| p.into_inner()) = 0;
                    *self
                        .inner
                        .expanded
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = false;
                    candwin::hide();
                    // 打了一串不像拼音的东西、用 Shift 把它原样上屏：这时候意图就是打英文，
                    // 顺手把全局模式切成英文（搜狗也是这个行为）。
                    state::set_chinese(false);
                    self.sentence_clear();
                    log_line(&format!("commit-raw(shift-up) {raw}"));
                    self.request_edit(EditAction::Commit(raw));
                } else {
                    let cur = state::chinese();
                    state::set_chinese(!cur);
                    log_line(&format!("chinese -> {}", !cur));
                }
                // Shift 单击可能切了中英，让浮条跟着重绘（浮条不缓存，paint 现读注册表）
                bar::refresh();
            }
            self.inner.shift_used.store(false, Ordering::SeqCst);
        }
        Ok(BOOL(0))
    }

    fn OnPreservedKey(
        &self,
        _pic: Ref<ITfContext>,
        _rguid: *const GUID,
    ) -> windows::core::Result<BOOL> {
        Ok(BOOL(0))
    }
}

/// 这个键是不是"中文标点转换"覆盖的键（关掉中文标点后这些键就该放行给应用）。
///
/// 只用来做放行判断，不关心转成什么 —— 所以 `-` `=`（永远是半角）不算在内。
fn is_punct_key(vk: u32, shift: bool) -> bool {
    cn_punct(vk, shift).is_some() || digit_pair(vk, shift).is_some()
}

/// 这个键该不该由输入法认领（TSF 按键状态机）。
///
/// 规则（对照主流输入法）：
/// - **Ctrl/Alt/Win 按下**：永远放行（是快捷键，如 Ctrl+A/Ctrl+C）
/// - **正在拼写**：认领 A-Z、数字、退格、空格、Esc（候选相关操作）
/// - **空闲**：只认领"无 Shift/CapsLock 的纯字母"来启动拼写；
///   Tab、CapsLock、Shift、Enter、空格、方向键、退格、符号 → 全部放行给应用
///
/// `shift` / `caps` 显式传进来（而不是内部查键盘），这样纯逻辑可单测。
fn should_claim(vk: u32, composing: bool, shift: bool, caps: bool, english: bool) -> bool {
    const VK_BACK: u32 = 0x08;
    const VK_RETURN: u32 = 0x0D;
    const VK_ESCAPE: u32 = 0x1B;
    const VK_SPACE: u32 = 0x20;
    const VK_DELETE: u32 = 0x2E;
    const VK_END: u32 = 0x23;
    const VK_HOME: u32 = 0x24;
    const VK_LEFT: u32 = 0x25;
    const VK_RIGHT: u32 = 0x27;
    const VK_UP: u32 = 0x26;
    const VK_DOWN: u32 = 0x28;
    const VK_0: u32 = 0x30;
    const VK_9: u32 = 0x39;
    const VK_A: u32 = 0x41;
    const VK_Z: u32 = 0x5A;
    const VK_OEM_MINUS: u32 = 0xBD;
    const VK_OEM_PLUS: u32 = 0xBB;
    const VK_OEM_COMMA: u32 = 0xBC;
    const VK_OEM_PERIOD: u32 = 0xBE;
    const VK_PRIOR: u32 = 0x21;
    const VK_NEXT: u32 = 0x22;

    // 快捷键永远放行
    if ctrl_alt_win_held() {
        return false;
    }

    // 英文模式：一律不认领。
    // 注意不能"认领后再在 OnKeyDown 里返回 FALSE"——WPS 这类应用认领了就不给回，
    // 表现为一个字母都打不出来。
    if english {
        return false;
    }

    if composing {
        // 拼写中按标点：要认领，才能"先上屏候选再插标点"
        if !caps && cn_punct(vk, shift).is_some() {
            return true;
        }
        // 正在拼写：字母/数字/候选操作键 + 组合串内移动光标的键 + 翻页键
        return matches!(
            vk,
            VK_A..=VK_Z
                | VK_0..=VK_9
                | VK_BACK
                | VK_DELETE
                | VK_SPACE
                | VK_RETURN
                | VK_ESCAPE
                | VK_LEFT
                | VK_RIGHT
                | VK_HOME
                | VK_END
                | VK_UP
                | VK_DOWN
                | VK_OEM_MINUS
                | VK_OEM_PLUS
                | VK_PRIOR
                | VK_NEXT
                | VK_OEM_COMMA
                | VK_OEM_PERIOD
        );
    }

    // 空闲：只有"无 Shift/CapsLock 的纯字母"才启动拼写
    // 纯字母（无 Shift/CapsLock）→ 认领，开始拼写
    if (VK_A..=VK_Z).contains(&vk) && !shift && !caps {
        return true;
    }
    // 中文标点键 → 也要认领。
    // 关键：微信这类应用在 OnTestKeyDown 返回 false 时**不会调用 OnKeyDown**，
    // 不认领就等于标点转换代码永远不执行（表现为标点保持英文）。
    if !caps && cn_punct(vk, shift).is_some() {
        return true;
    }
    false
}

/// 这一次按键算不算"在用 Shift 打字符"（而不是 Shift 单击切换中英文）。
///
/// 必须在 `OnTestKeyDown` 里就记下来，不能只在 `OnKeyDown` 里记：
/// 有些键我们压根不认领（`Shift+=`、`Shift+-` 就是），不认领就不会有 `OnKeyDown`，
/// 于是 `shift_used` 一直是 false，松开 Shift 就成了"单击切换"——表现为打个 `+` 就切到英文。
fn is_shift_chord(vk: u32, shift: bool, ctrl_alt_win: bool) -> bool {
    const VK_SHIFT_ONLY: u32 = 0x10;
    vk != VK_SHIFT_ONLY && shift && !ctrl_alt_win
}

/// 显示候选窗。
///
/// 位置优先级：`pos`（TSF GetTextExt 拿到的精确坐标，和搜狗同款）→ 光标/鼠标兜底。
fn show_cand_window(
    pinyin: &str,
    cands: &[String],
    page: usize,
    expanded: bool,
    sel: usize,
    pos: Option<CaretPos>,
) {
    let size = page_size(expanded);
    let total_pages = (cands.len() + size - 1) / size;
    // 候选窗上的拼音行和文档里的组合串用同一种显示（带音节分隔符）
    let (pinyin_view, _) = pinyin::compose_view(pinyin, 0);
    let start = page * size;
    if start >= cands.len() {
        candwin::hide();
        return;
    }
    let end = (start + size).min(cands.len());
    let slice = &cands[start..end];
    // 没有可信的光标位置就干脆不显示：宁可看不到候选窗，也不要在屏幕某个
    // 角落飘一个和光标无关的框（以前的 (200, 200) 兜底就是这个毛病）。
    let Some(caret) = pos.or_else(ipc::caret_screen_rect) else {
        candwin::hide();
        return;
    };
    // 具体放哪儿（下方还是上方、左右怎么贴边）由 candwin 决定，它知道窗口尺寸
    candwin::show(&pinyin_view, slice, sel, page + 1, total_pages, caret);
}

/// 用 TSF 的 `ITfContextView::GetTextExt` 问应用"这段组合串在屏幕的哪个矩形里"。
///
/// 这是主流输入法的做法：坐标由应用自己算，所以 VSCode / WPS / 浏览器都精准。
/// 失败返回 `None`（老式应用或应用没实现时），调用方再退到光标/鼠标兜底。
fn text_ext_pos(ctx: &ITfContext, ec: u32, range: &ITfRange) -> Option<CaretPos> {
    // SAFETY: 标准 TSF 查询，处于编辑会话内
    unsafe {
        let view = ctx.GetActiveView().ok()?;
        let mut rect: RECT = std::mem::zeroed();
        let mut clipped = BOOL(0);
        view.GetTextExt(ec, range, &mut rect, &mut clipped).ok()?;
        if rect.left == 0 && rect.top == 0 && rect.right == 0 && rect.bottom == 0 {
            return None;
        }
        // 顶边和底边都给出去：候选窗在屏幕底部时要往上翻
        Some((rect.left, rect.top, rect.bottom))
    }
}

/// 读出当前选区的文字（只在编辑会话里调）。
///
/// 没有选区、读不到、宿主不支持，一律返回空串 —— 调用方按"没有选中文字"处理。
/// 缓冲区给 8192 个 UTF-16 单元：AI 只处理 2000 字以内的选区，这个上限远大于它，
/// 真遇到超长选区也只是被截断（读和写回用的是同一个上限，比对仍然成立）。
fn selection_text(ctx: &ITfContext, ec: u32) -> String {
    let mut sel = [TF_SELECTION::default()];
    let mut fetched = 0u32;
    // SAFETY: 标准 TSF 查询，处于编辑会话内
    unsafe {
        if ctx
            .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)
            .is_err()
        {
            return String::new();
        }
        match sel[0].range.as_ref() {
            Some(range) => range_text(ec, range),
            None => String::new(),
        }
    }
}

/// 读一个 range 里的文字。
fn range_text(ec: u32, range: &ITfRange) -> String {
    let mut buf = vec![0u16; 8192];
    let mut got = 0u32;
    // SAFETY: 缓冲区是我们自己的，长度按元素个数传给 TSF
    let ok = unsafe { range.GetText(ec, 0, &mut buf, &mut got).is_ok() };
    if !ok {
        return String::new();
    }
    let n = (got as usize).min(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// 把 AI 结果写回当前选区。
///
/// 两条安全线：
/// 1. **选区必须还是原来那段文字**。结果回来要几秒，这期间用户可能已经点了别处、
///    重选了别的段落。拿旧结果去改新选区就是改错地方 —— 直接丢掉（只写日志）。
/// 2. `continue` 是"接在后面"：先把选区塌到末尾，再在光标处插入；
///    翻译/改写则是替换整个选区。
fn apply_ai_result(
    ctx: &ITfContext,
    ec: u32,
    action: u8,
    expect: &str,
    text: &str,
) -> (&'static str, Option<i32>) {
    let current = selection_text(ctx, ec);
    if current.trim() != expect.trim() {
        return ("selection changed", None);
    }
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let mut sel = [TF_SELECTION::default()];
    let mut fetched = 0u32;
    // SAFETY: 标准 TSF 编辑操作，全部在同一个编辑会话内
    unsafe {
        if ctx
            .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)
            .is_err()
        {
            return ("GetSelection failed", None);
        }
        let Some(range) = sel[0].range.as_ref() else {
            return ("no range", None);
        };
        if !ai::replaces_selection(action) && range.Collapse(ec, TF_ANCHOR_END).is_err() {
            return ("collapse failed", None);
        }
        if range.SetText(ec, 0, &utf16).is_err() {
            return ("SetText failed", None);
        }
        let _ = range.Collapse(ec, TF_ANCHOR_END);
        let caret = [TF_SELECTION {
            range: std::mem::ManuallyDrop::new(Some(range.clone())),
            style: Default::default(),
        }];
        if ctx.SetSelection(ec, &caret).is_err() {
            return ("SetSelection failed", None);
        }
        // 记下写入后的锚点，撤回时要靠它确认"光标还在我们放的地方"
        let mut anchor = None;
        if let Ok(acp) = range.cast::<ITfRangeACP>() {
            let (mut a, mut c) = (0i32, 0i32);
            if acp.GetExtent(&mut a, &mut c).is_ok() && c == 0 {
                anchor = Some(a);
            }
        }
        ("ok", anchor)
    }
}

/// 撤回一次 AI 写入：删掉插入的那段，把原文写回去。
fn revert_ai_result(
    ctx: &ITfContext,
    ec: u32,
    original: &str,
    inserted: i32,
    end_anchor: i32,
) -> &'static str {
    let Some((start, len)) = revert_extent(end_anchor, inserted) else {
        return "nothing to revert";
    };
    let mut sel = [TF_SELECTION::default()];
    let mut fetched = 0u32;
    let utf16: Vec<u16> = original.encode_utf16().collect();
    // SAFETY: 标准 TSF 编辑操作，全部在同一个编辑会话内
    unsafe {
        if ctx
            .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)
            .is_err()
        {
            return "GetSelection failed";
        }
        let Some(range) = sel[0].range.as_ref() else {
            return "no range";
        };
        let Ok(acp) = range.cast::<ITfRangeACP>() else {
            return "no acp";
        };
        let (mut anchor, mut cch) = (0i32, 0i32);
        if acp.GetExtent(&mut anchor, &mut cch).is_err() {
            return "GetExtent failed";
        }
        if cch != 0 || anchor != end_anchor {
            return "caret moved";
        }
        if acp.SetExtent(start, len).is_err() {
            return "SetExtent failed";
        }
        if range.SetText(ec, 0, &utf16).is_err() {
            return "SetText failed";
        }
        let _ = range.Collapse(ec, TF_ANCHOR_END);
        let caret = [TF_SELECTION {
            range: std::mem::ManuallyDrop::new(Some(range.clone())),
            style: Default::default(),
        }];
        if ctx.SetSelection(ec, &caret).is_err() {
            return "SetSelection failed";
        }
    }
    "ok"
}

/// 把一段文字插到光标处（没有选区就是纯插入，有选区就是替换）。
///
/// 某些应用（Electron 系）对裸选区 `SetText` 不认，所以返回 `why` 让调用方
/// 决定要不要退回 `SendInput`。
fn insert_at_caret(ctx: &ITfContext, ec: u32, text: &str) -> &'static str {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let mut sel = [TF_SELECTION::default()];
    let mut fetched = 0u32;
    let mut why = "ok";
    // SAFETY: 标准 TSF 调用，处于编辑会话内
    unsafe {
        match ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched) {
            Err(_) => why = "GetSelection failed",
            Ok(()) => match sel[0].range.as_ref() {
                None => why = "no range",
                Some(range) => {
                    if range.SetText(ec, 0, &utf16).is_err() {
                        why = "SetText failed";
                    } else {
                        let _ = range.Collapse(ec, TF_ANCHOR_END);
                        let caret = [TF_SELECTION {
                            range: std::mem::ManuallyDrop::new(Some(range.clone())),
                            style: Default::default(),
                        }];
                        if ctx.SetSelection(ec, &caret).is_err() {
                            why = "SetSelection failed";
                        }
                    }
                }
            },
        }
    }
    why
}

/// 光标前一个字符是不是阿拉伯数字？
///
/// 探针只在**光标是塌缩状态**时做（有选区就直接答"不是"，反正接下来是替换）。
/// NBSP 之类不影响：只认 `0-9`。
/// 读完把光标放回原处，调用方拿到的还是同一个光标位置。
fn preceding_char_is_digit(ctx: &ITfContext, ec: u32) -> bool {
    // SAFETY: 标准 TSF 查询，处于编辑会话内
    unsafe {
        let mut sel = [TF_SELECTION::default()];
        let mut fetched = 0u32;
        if ctx
            .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)
            .is_err()
        {
            return false;
        }
        let Some(range) = sel[0].range.as_ref() else {
            return false;
        };
        let mut anchor = 0i32;
        let mut cch = 0i32;
        let collapsed = range
            .cast::<ITfRangeACP>()
            .map(|acp| acp.GetExtent(&mut anchor, &mut cch).is_ok() && cch == 0)
            .unwrap_or(false);
        if !collapsed {
            return false;
        }
        let mut shifted = 0i32;
        if range
            .ShiftStart(ec, -1, &mut shifted, std::ptr::null())
            .is_err()
            || shifted == 0
        {
            return false;
        }
        let mut buf = [0u16; 2];
        // pcch 是入参（缓冲区长度）也是出参（实际写入长度）
        let mut got = buf.len() as u32;
        let digit = match range.GetText(ec, 0, &mut buf, &mut got) {
            Ok(()) if got >= 1 => char::from_u32(buf[got as usize - 1] as u32)
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false),
            _ => false,
        };
        // 放回原处：区间现在是 [前一个字符, 光标]，塌缩到末尾就回到光标
        let _ = range.Collapse(ec, TF_ANCHOR_END);
        digit
    }
}

/// 查候选；整串查不到就逐步缩短，返回 (候选, 命中的前缀长度)。
///
/// 这样 `xiangmm` 会退到 `xiang` 出候选，而不是整串查不到直接卡死。
/// 去掉拼音缓冲里的分词符（`-` / `_`）。
///
/// 分隔符只做显示与最终原样上屏，不参与音节匹配：
/// `xiang-mu` 送进引擎前变成 `xiangmu`，才能查到「项目」。
fn strip_separators(code: &str) -> String {
    code.chars()
        .filter(|c| *c != '-' && *c != '_' && *c != '\'')
        .collect()
}

/// 组出一个"把拼音写进文档"的编辑动作：文本带音节分隔符，光标换算到显示串里。
///
/// 缓冲里存的是用户原样敲的串（`xiangmu` / `xi-an`），文档里显示的是带分隔符的形式
/// （`xiang'mu` / `xi'an`）——和搜狗、微信一致。显示串只含 ASCII，
/// 所以 `compose_view` 返回的下标可以直接给 `SetText` / `ShiftEnd` 用。
fn compose_action(raw: &str, cursor: usize) -> EditAction {
    let (text, cursor) = pinyin::compose_view(raw, cursor);
    EditAction::Compose(text, cursor)
}

/// 把缓冲里的分词符统一成音节边界 `'`（`xi-an` → `xi'an`）。
///
/// 边界是**用户表达的意图**：`xian` 是一个音节（先），`xi'an` 是两个（西安）。
/// 所以送进引擎的码要带着边界（引擎按带边界的码记账，见 `learn_key`），
/// 只有真正送去 librime 的那一步才还原成整串。
fn normalize_boundaries(code: &str) -> String {
    let mut out = String::with_capacity(code.len());
    for ch in code.chars() {
        if ch == '-' || ch == '_' || ch == '\'' || ch == ' ' {
            if !out.is_empty() && !out.ends_with('\'') {
                out.push('\'');
            }
            continue;
        }
        out.push(ch);
    }
    while out.ends_with('\'') {
        out.pop();
    }
    out
}

/// 缓冲里消耗掉 `letters` 个字母后，该剪到的字节下标。
///
/// 缓冲里可能有分词符（`xiang-mu`），所以字母数不能直接当字节下标用。
fn consumed_index(raw: &str, letters: usize) -> usize {
    let mut seen = 0usize;
    for (i, ch) in raw.char_indices() {
        if seen >= letters {
            return i;
        }
        if ch != '-' && ch != '_' {
            seen += 1;
        }
    }
    raw.len()
}

/// 这串拼音里还有没有元音？没有（`x`、`ng`）就是打错的辅音，可以直接丢掉。
fn has_vowel(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'v'))
}

/// 选中第 `pick_idx` 个候选之后怎么收场，返回要执行的编辑动作。
///
/// 规则（用户要求，见对话）：选掉几个字，就只从拼音里剪掉这几个字对应的音节，
/// 剩下的拼音**留在组合串里继续选**；而不是像以前那样整串清空。
///
/// `pikeqiu` → 选「皮」→ 文章里落下「皮」，候选窗接着显示 `keqiu` 的候选
/// → 选「克」→ 接着 `qiu` → 选「球」→ 没有剩余，正常提交。
///
/// 剩余部分为空、没有元音、或者查不到候选时收尾：把选中的字上屏，
/// 多打/打错的字母一起丢掉。
#[allow(clippy::too_many_arguments)]
fn apply_pick(
    pick_idx: usize,
    cands: &mut Vec<String>,
    buffer: &mut String,
    cursor: &mut usize,
    matched_len: &mut usize,
    page: &mut usize,
    expanded: &mut bool,
    sel: &mut usize,
    pos: Option<CaretPos>,
) -> Option<EditAction> {
    let pick = cands.get(pick_idx)?.clone();
    // 上报选词，让引擎记进个人词库（失败不影响上屏）。
    // 带上 buffer（当时的完整拼音）和 pick（选中的词）：引擎按选中的字数
    // 反推该记在哪个拼音串下，分段上屏时才不会把 `皮` 记成 `pikeqiu`。
    // P3 起可以整体关掉学习（`Config\Learn=0`）：那就不发这条消息，纯查询。
    if crate::config::snapshot().learn {
        ipc::select(pick_idx, buffer, &pick);
    }

    let stripped = strip_separators(buffer);
    let chars = pick.chars().count();
    // 这次选掉的字消耗多少个拼音字母；整串都切不出音节（英文候选）就按整串算
    let mut used = pinyin::consumed_letters(&stripped, chars);
    if used == 0 || used >= stripped.len() {
        used = stripped.len();
    }
    let cut = consumed_index(buffer, used);
    let rest: String = buffer[cut..].trim_start_matches(['-', '_']).to_string();

    // 还剩有效拼音吗？
    let mut cont = None;
    if !rest.is_empty() && has_vowel(&rest) {
        let (rc, rm) = query_with_fallback(&rest);
        if !rc.is_empty() {
            cont = Some((rc, rm));
        }
    }

    let Some((rc, rm)) = cont else {
        // 收尾
        buffer.clear();
        cands.clear();
        *matched_len = 0;
        *cursor = 0;
        *page = 0;
        *expanded = false;
        *sel = 0;
        candwin::hide();
        return Some(EditAction::Commit(pick));
    };

    // 继续拼剩下的拼音
    *buffer = rest;
    *cursor = buffer.len();
    *cands = rc;
    *matched_len = rm;
    *page = 0;
    *expanded = false;
    *sel = 0;
    show_cand_window(buffer, cands, *page, *expanded, *sel, pos);
    Some(EditAction::PickCompose(pick, buffer.clone()))
}

fn query_with_fallback(code: &str) -> (Vec<String>, usize) {
    // 带着边界发出去（`xi'an`）：引擎用它查 librime（还原成 `xian`），
    // 同时用它当用户词库的键 —— 这样"在 xi'an 下选过西安"不会污染 `xian`。
    let code = normalize_boundaries(code);
    if strip_separators(&code).is_empty() {
        return (Vec::new(), 0);
    }
    let mut probe = code;
    loop {
        let flat_len = strip_separators(&probe).len();
        if flat_len == 0 {
            break;
        }
        let r = ipc::query(&probe);
        if !r.is_empty() {
            return (r, flat_len);
        }
        // 往前退一个字母再试（末尾打错的字母丢掉），退到分隔符也一起退掉
        probe.pop();
        while probe.ends_with('\'') {
            probe.pop();
        }
        if probe.is_empty() {
            break;
        }
    }
    (Vec::new(), 0)
}

/// Ctrl / Alt / Win 是否按下（快捷键，应交给应用）。
fn ctrl_alt_win_held() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12;
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;
    // SAFETY: 纯查询
    unsafe {
        [VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .any(|vk| (GetAsyncKeyState(*vk) as u16 & 0x8000) != 0)
    }
}

/// Shift 按下 或 CapsLock 开着（用户想打英文大写，应交给应用）。
fn shift_or_caps() -> bool {
    shift_held() || caps_on()
}

/// CapsLock 是否开着。
///
/// 用 `GetKeyState` 而不是 `GetAsyncKeyState`：后者的低字节（开关状态）
/// 在 MSDN 里明确标注"不可靠"，实测开了 CapsLock 仍返回 0，导致大写失效。
/// `GetKeyState` 返回的是"当前线程处理最后一条消息时"的状态，正是我们要的。
fn caps_on() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
    const VK_CAPITAL: i32 = 0x14;
    // SAFETY: 纯查询
    unsafe { (GetKeyState(VK_CAPITAL) as u16 & 0x0001) != 0 }
}

/// 只看 Shift 是否按下（中文标点用，CapsLock 不参与）。
fn shift_held() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_SHIFT: i32 = 0x10;
    // SAFETY: 纯查询
    unsafe { (GetAsyncKeyState(VK_SHIFT) as u16 & 0x8000) != 0 }
}

/// 中文标点映射：英文符号键 → 中文标点（对齐微软拼音/搜狗）。
///
/// 只在"中文模式且 CapsLock 没开"时调用；`\` → `、` 就是顿号。
/// 引号这里只给**左引号**，开合交替在 `resolve_punct` 里做。
/// 返回 `None` 表示不是标点键，交给应用。
/// `-` `=` `_` `+` 四个键永远出**半角**原字符，不做中文标点转换。
///
/// 理由：这四个是公式/代码/算式里的运算符，全角版本（`－＝＋`）和半角在应用眼里
/// 是两个完全不同的字符 —— Excel 里打 `=` 变小写全角 `＝`，整个公式就不会被识别
/// （用户实测："在 excel 里面 - = 这两个打不出来"）。
///
/// 拼写中仍然要经过这里（`先上屏候选再插符号`），所以这个函数既服务于
/// 空闲时的直通，也服务于拼写中的收尾。
fn halfwidth_symbol(vk: u32, shift: bool) -> Option<&'static str> {
    Some(match (vk, shift) {
        (VK_OEM_MINUS, false) => "-",
        (VK_OEM_PLUS, false) => "=",
        (VK_OEM_MINUS, true) => "_",
        (VK_OEM_PLUS, true) => "+",
        _ => return None,
    })
}

/// 这几个标点在数字后面应该是半角：`3.14` / `1,000` / `10:30`。
/// 返回 `(全角, 半角)`，由编辑会话看光标前一个字符决定用哪个。
fn digit_pair(vk: u32, shift: bool) -> Option<(&'static str, &'static str)> {
    Some(match (vk, shift) {
        (VK_OEM_PERIOD, false) => ("。", "."),
        (VK_OEM_COMMA, false) => ("，", ","),
        (VK_OEM_1, true) => ("：", ":"),
        _ => return None,
    })
}

fn cn_punct(vk: u32, shift: bool) -> Option<&'static str> {
    Some(match (vk, shift) {
        // 不按 Shift
        (VK_OEM_COMMA, false) => "，",
        (VK_OEM_PERIOD, false) => "。",
        (VK_OEM_1, false) => "；",
        // `/` 和 `\` 都出顿号：主流输入法里 `/` 就是顿号。
        // 之前 `/` 映射成全角 `／`，看起来跟 `/` 一样，用户以为没转换。
        (VK_OEM_2, false) => "、",
        (VK_OEM_3, false) => "·",
        (VK_OEM_4, false) => "【",
        (VK_OEM_5, false) => "、",
        (VK_OEM_6, false) => "】",
        // 单引号/双引号给的是**左引号**，实际输出由 `resolve_quote` 按开合状态交替。
        (VK_OEM_7, false) => "‘",
        // 注意：`-` `=` `_` `+` 不在这张表里，它们由 `halfwidth_symbol()` 处理成半角。
        // 这里返回 None 还有第二个作用：空闲时 `should_claim` 不认领这两个键，
        // Excel 里打 `=A1-B1` 才能原样进单元格（认领了就会被转成全角）。
        // 按住 Shift：问号、叹号、冒号这些都要 Shift 才打得出来，
        // 所以 Shift 只影响字母大小写，不影响标点转换。
        (VK_OEM_COMMA, true) => "《",
        (VK_OEM_PERIOD, true) => "》",
        (VK_OEM_1, true) => "：",
        (VK_OEM_2, true) => "？",
        (VK_OEM_3, true) => "～",
        (VK_OEM_4, true) => "「",
        (VK_OEM_5, true) => "｜",
        (VK_OEM_6, true) => "」",
        (VK_OEM_7, true) => "“", // 左双引号，实际输出见 `resolve_quote`
        // Shift + 数字：全角符号（对齐搜狗/微软拼音的习惯）。
        // 括号是用户反馈最直接的缺口——原来 `(` `)` 直接漏给应用，打出来是半角。
        (VK_1, true) => "！",
        (VK_2, true) => "＠",
        (VK_3, true) => "＃",
        (VK_4, true) => "￥",
        (VK_5, true) => "％",
        (VK_6, true) => "……",
        (VK_7, true) => "＆",
        (VK_8, true) => "＊",
        (VK_9, true) => "（",
        (VK_0, true) => "）",
        _ => return None,
    })
}

/// 中文引号的开合状态：`true` = 下一个该打左引号。
///
/// 主流输入法的引号是「按一下开、再按一下合」的交替行为，没有别的上下文可依据。
static DQUOTE_OPEN: AtomicBool = AtomicBool::new(true);
static SQUOTE_OPEN: AtomicBool = AtomicBool::new(true);

/// 纯逻辑：根据"是否该打左引号"把左引号换成对应的一半。
/// 拆出来是为了能单测，不依赖全局状态。
fn pick_quote(left: &'static str, open: bool) -> &'static str {
    match (left, open) {
        ("“", true) => "“",
        ("“", false) => "”",
        ("‘", true) => "‘",
        ("‘", false) => "’",
        _ => left,
    }
}

/// 读并**翻转**引号状态：返回"本次该打左引号吗"。
///
/// 必须用 `fetch_xor(true)` 而不是 `swap(false)`——后者每次都把状态写成 false，
/// 于是第一次左、之后永远是右，引号再也开不回来（0.1.27 的 bug）。
fn flip_quote(flag: &AtomicBool) -> bool {
    flag.fetch_xor(true, Ordering::SeqCst)
}

/// 把 `cn_punct` 给出的标点解析成最终要上屏的字符串（引号会交替成对）。
fn resolve_punct(ch: &'static str) -> String {
    match ch {
        "“" => pick_quote("“", flip_quote(&DQUOTE_OPEN)).to_string(),
        "‘" => pick_quote("‘", flip_quote(&SQUOTE_OPEN)).to_string(),
        other => other.to_string(),
    }
}

const VK_0: u32 = 0x30;
const VK_1: u32 = 0x31;
const VK_2: u32 = 0x32;
const VK_3: u32 = 0x33;
const VK_4: u32 = 0x34;
const VK_5: u32 = 0x35;
const VK_6: u32 = 0x36;
const VK_7: u32 = 0x37;
const VK_8: u32 = 0x38;
const VK_9: u32 = 0x39;

const VK_OEM_1: u32 = 0xBA;
const VK_OEM_2: u32 = 0xBF;
const VK_OEM_3: u32 = 0xC0;
const VK_OEM_4: u32 = 0xDB;
const VK_OEM_5: u32 = 0xDC;
const VK_OEM_6: u32 = 0xDD;
const VK_OEM_7: u32 = 0xDE;
const VK_OEM_MINUS: u32 = 0xBD;
const VK_OEM_PLUS: u32 = 0xBB;
const VK_OEM_COMMA: u32 = 0xBC;
const VK_OEM_PERIOD: u32 = 0xBE;

/// 注册类接口在"已存在"时返回 S_FALSE(HRESULT 1)，windows-rs 会当成 Err。
/// 这里把 S_FALSE 视为成功，让重复注册变成幂等操作。
fn ignore_s_false(r: windows::core::Result<()>) -> windows::core::Result<()> {
    match r {
        Err(e) if e.code() == windows::core::HRESULT(1) => Ok(()),
        other => other,
    }
}

fn log_line(msg: &str) {
    use std::io::Write;
    static LOG_LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOG_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // 固定写到 Public：所有用户可写，且比 %TEMP% 更容易找到。
    let path = format!("C:\\Users\\Public\\inklink-tsf-{}.log", std::process::id());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{msg}");
    }
}

// ─────────────────────────── DLL 导出 ───────────────────────────

/// 永远返回 S_FALSE：即使引用计数归零也不卸载。
///
/// 上一版 inklink 就是在这里返回了真实状态，导致 TSF 在 COM 计数归零时
/// `FreeLibrary`，而静态引擎和后台线程还活着 → 在已释放代码上析构 → 0xC0000409。
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

/// COM 入口：指针全部由调用方（TSF / `regsvr32`）保证有效，用错就是未定义行为，
/// 所以整个函数标 `unsafe`（clippy 的 `not_unsafe_ptr_arg_deref` 也要求这样）。
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    // SAFETY: 标准 COM 入口，指针由调用方保证有效
    unsafe {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return windows::Win32::Foundation::E_POINTER;
        }
        log_line("DllGetClassObject");
        if *rclsid != CLSID_INKLINK_TSF {
            return CLASS_E_NOAGGREGATION;
        }
        let factory: IClassFactory = ClassFactory.into();
        factory
            .query(riid, ppv)
            .ok()
            .map_or_else(|_| E_NOINTERFACE, |_| S_OK)
    }
}

/// `regsvr32` 会调这个：注册 CLSID 与 TSF 语言配置。
#[unsafe(no_mangle)]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    match registry::register() {
        Ok(()) => S_OK,
        Err(_) => windows::Win32::Foundation::E_FAIL,
    }
}

/// `regsvr32 /u` 调这个：清理注册表。
#[unsafe(no_mangle)]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    match registry::unregister() {
        Ok(()) => S_OK,
        Err(_) => windows::Win32::Foundation::E_FAIL,
    }
}

/// 供 `regsvr32` 使用的 DLL 句柄占位（部分注册工具会查询）。
#[unsafe(no_mangle)]
pub extern "system" fn DllMain(
    _inst: HINSTANCE,
    _reason: u32,
    _reserved: *mut core::ffi::c_void,
) -> BOOL {
    // 什么都不做：DllMain 里做事是死锁与崩溃的常见来源
    BOOL(1)
}

// ─────────────────────────── 注册表 ───────────────────────────

mod registry {
    use super::{CLSID_INKLINK_TSF, DLL_DESC, GUID_PROFILE};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
        RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
    };

    type Result<T> = core::result::Result<T, WIN32_ERROR>;

    fn wide(s: &str) -> Vec<u16> {
        OsString::from(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn set_sz(hkey: HKEY, name: &str, value: &str) -> Result<()> {
        let data = wide(value);
        let bytes =
            unsafe { core::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2) };
        let name_w = wide(name);
        let rc = unsafe {
            RegSetValueExW(
                hkey,
                windows::core::PCWSTR(name_w.as_ptr()),
                None,
                REG_SZ,
                Some(bytes),
            )
        };
        if rc == ERROR_SUCCESS { Ok(()) } else { Err(rc) }
    }

    fn create(root: HKEY, path: &str) -> Result<HKEY> {
        let path_w = wide(path);
        let mut hkey = HKEY::default();
        let rc = unsafe {
            RegCreateKeyExW(
                root,
                windows::core::PCWSTR(path_w.as_ptr()),
                None,
                windows::core::PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            )
        };
        if rc == ERROR_SUCCESS {
            Ok(hkey)
        } else {
            Err(rc)
        }
    }

    fn close(hkey: HKEY) {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
    }

    fn clsid_str() -> String {
        format!("{{{:?}}}", CLSID_INKLINK_TSF)
    }

    /// 当前 DLL 路径
    pub(crate) fn dll_path() -> String {
        use windows::Win32::System::LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GetModuleFileNameW, GetModuleHandleExW,
        };
        let mut buf = [0u16; 1024];
        unsafe {
            let mut module = windows::Win32::Foundation::HMODULE::default();
            let _ = GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                windows::core::PCWSTR(crate::DllRegisterServer as *const u16),
                &mut module,
            );
            let len = GetModuleFileNameW(Some(module), &mut buf);
            String::from_utf16_lossy(&buf[..len as usize])
        }
    }

    pub fn register() -> core::result::Result<(), windows::core::HRESULT> {
        let clsid = clsid_str();
        let path = dll_path();

        // 1. CLSID → InprocServer32（COM 激活必需）
        let inproc = format!("SOFTWARE\\Classes\\CLSID\\{clsid}\\InprocServer32");
        let h = create(HKEY_LOCAL_MACHINE, &inproc)?;
        set_sz(h, "", &path)?;
        set_sz(h, "ThreadingModel", "Apartment")?;
        close(h);

        // 2. CTF\TIP 描述
        let tip = format!("SOFTWARE\\Microsoft\\CTF\\TIP\\{clsid}");
        let h = create(HKEY_LOCAL_MACHINE, &tip)?;
        set_sz(h, "", DLL_DESC)?;
        close(h);

        // 3. 语言配置：**完全交给 AddLanguageProfile 写**。
        //    手动写会出错（IconIndex 类型、Description 位置），还会和 TSF 自己写的值打架。
        if let Err(e) = register_with_tsf(&path) {
            eprintln!("ITfInputProcessorProfiles 登记失败: {e:?}");
            return Err(windows::core::HRESULT::from_win32(0x8000_4005));
        }

        Ok(())
    }

    /// 语言列表里我们那一行用哪个文件当图标。
    ///
    /// 优先用安装目录里的 `inklink.ico`（安装器会拷过去），没有就退回 DLL 自身
    /// （DLL 里也编了一份图标资源，索引 0）。开发机上没跑安装器时走的就是退回分支。
    pub(super) fn icon_path_for(dll_path: &str) -> String {
        let path = std::path::Path::new(dll_path);
        if let Some(dir) = path.parent() {
            let ico = dir.join("inklink.ico");
            if ico.exists() {
                return ico.to_string_lossy().into_owned();
            }
        }
        dll_path.to_string()
    }

    /// 走 TSF 官方接口登记：`Register` + `AddLanguageProfile` + `EnableLanguageProfile`。
    ///
    /// 注意两点，都是踩过的坑：
    /// - 图标参数不能传空切片（会解引用空指针直接崩），要传真实路径
    /// - COM 对象必须在 `CoUninitialize` **之前**释放
    fn register_with_tsf(dll_path: &str) -> windows::core::Result<()> {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        };
        use windows::Win32::UI::TextServices::{
            CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles,
            GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, GUID_TFCAT_TIP_KEYBOARD, ITfCategoryMgr,
            ITfInputProcessorProfiles,
        };

        const LANG_ZH_CN: u16 = 0x0804;

        // SAFETY: 注册期间成对初始化/反初始化 COM，对象先于反初始化释放
        unsafe {
            let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let result = (|| -> windows::core::Result<()> {
                let profiles: ITfInputProcessorProfiles =
                    CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;

                // Register / RegisterCategory / AddLanguageProfile 在"已存在"时返回
                // S_FALSE(1)，windows-rs 会当成 Err——这里视为成功（幂等注册）。
                crate::ignore_s_false(profiles.Register(&CLSID_INKLINK_TSF))?;

                // 关键：注册到「键盘输入法」分类。没有这一步，Set-WinUserLanguageList
                // 不会把它当作可选键盘，输入法列表里永远不出现。
                let catmgr: ITfCategoryMgr =
                    CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
                crate::ignore_s_false(catmgr.RegisterCategory(
                    &CLSID_INKLINK_TSF,
                    &GUID_TFCAT_TIP_KEYBOARD,
                    &CLSID_INKLINK_TSF,
                ))?;

                // 组合串下划线：把自己登记成「显示属性提供者」。
                // 应用渲染组合串时就会用我们的 CLSID 去要样式（见 display.rs）。
                // 这是纯加性的分类注册，不影响键盘 TIP 那条。
                crate::ignore_s_false(catmgr.RegisterCategory(
                    &CLSID_INKLINK_TSF,
                    &GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
                    &CLSID_INKLINK_TSF,
                ))?;
                drop(catmgr);

                let mut desc: Vec<u16> = DLL_DESC.encode_utf16().collect();
                // 必须补 NUL：AddLanguageProfile 按 C 字符串读，不补会读到缓冲区后面的垃圾
                // （表现是语言列表里我们的图标路径变成一串乱码、名字也显示不出来）。
                desc.push(0);

                let mut icon: Vec<u16> = icon_path_for(dll_path).encode_utf16().collect();
                icon.push(0);
                crate::ignore_s_false(profiles.AddLanguageProfile(
                    &CLSID_INKLINK_TSF,
                    LANG_ZH_CN,
                    &GUID_PROFILE,
                    &desc,
                    &icon,
                    0,
                ))?;

                // EnableLanguageProfile 失败时把错误透传出去——否则注册"看起来成功"，
                // 但输入法列表里永远不会出现，问题会被藏起来。
                profiles.EnableLanguageProfile(
                    &CLSID_INKLINK_TSF,
                    LANG_ZH_CN,
                    &GUID_PROFILE,
                    true,
                )?;

                drop(profiles);
                Ok(())
            })();
            if init.is_ok() {
                CoUninitialize();
            }
            result
        }
    }

    pub fn unregister() -> core::result::Result<(), windows::core::HRESULT> {
        unregister_with_tsf();
        let clsid = clsid_str();
        let keys = [
            format!("SOFTWARE\\Classes\\CLSID\\{clsid}"),
            format!("SOFTWARE\\Microsoft\\CTF\\TIP\\{clsid}"),
        ];
        for key in keys {
            let key_w = wide(&key);
            unsafe {
                let _ = RegDeleteTreeW(HKEY_LOCAL_MACHINE, windows::core::PCWSTR(key_w.as_ptr()));
            }
        }
        let _ = HKEY_CLASSES_ROOT;
        Ok(())
    }
    /// 反注册：先让 TSF 忘掉，再删注册表。
    fn unregister_with_tsf() {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        };
        use windows::Win32::UI::TextServices::{
            CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfiles,
        };
        const LANG_ZH_CN: u16 = 0x0804;

        // SAFETY: 成对初始化/反初始化，对象先释放
        unsafe {
            let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if let Ok(profiles) = CoCreateInstance::<_, ITfInputProcessorProfiles>(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            ) {
                let _ = profiles.EnableLanguageProfile(
                    &CLSID_INKLINK_TSF,
                    LANG_ZH_CN,
                    &GUID_PROFILE,
                    false,
                );
                let _ =
                    profiles.RemoveLanguageProfile(&CLSID_INKLINK_TSF, LANG_ZH_CN, &GUID_PROFILE);
                let _ = profiles.Unregister(&CLSID_INKLINK_TSF);
                drop(profiles);
            }
            if init.is_ok() {
                CoUninitialize();
            }
        }
    }
}

// ─────────────────────────── 单元测试 ───────────────────────────
//
// 这些是"按键该不该认领"和"标点怎么映射"的纯逻辑，
// 不依赖 GUI，能直接 cargo test 跑——之前几个 bug 都藏在这里。
#[cfg(test)]
mod tests {
    use super::*;

    const VK_A_KEY: u32 = 0x41; // A
    const VK_N_KEY: u32 = 0x4E; // N
    const VK_SPACE_KEY: u32 = 0x20;
    const VK_RETURN_KEY: u32 = 0x0D;
    const VK_BACK_KEY: u32 = 0x08;
    const VK_ESCAPE_KEY: u32 = 0x1B;
    const VK_TAB_KEY: u32 = 0x09;
    const VK_LEFT_KEY: u32 = 0x25;

    #[test]
    fn 空闲时普通字母要认领() {
        assert!(should_claim(VK_A_KEY, false, false, false, false));
        assert!(should_claim(VK_N_KEY, false, false, false, false));
    }

    #[test]
    fn 上屏后连按退格删光才算误选() {
        let mut mark =
            CommitMark::new("xiangmu".to_string(), "项目".to_string()).expect("两个字应该被跟踪");
        assert!(
            mark.on_backspace().is_none(),
            "只删一个字还不够，可能是在改后面的内容"
        );
        assert_eq!(
            mark.on_backspace(),
            Some(("xiangmu".to_string(), "项目".to_string())),
            "删光两个字才是选错了"
        );
    }

    #[test]
    fn 空词和单个标点不做误选判断() {
        assert!(
            CommitMark::new("x".to_string(), String::new()).is_none(),
            "空词不该跟踪"
        );
        let mut one = CommitMark::new("de".to_string(), "的".to_string()).expect("一个字也能跟踪");
        assert_eq!(
            one.on_backspace(),
            Some(("de".to_string(), "的".to_string())),
            "一个字删一次就删光了"
        );
    }

    #[test]
    fn 刚上屏的词在时间窗内有效() {
        let mark = CommitMark::new("nihao".to_string(), "你好".to_string()).expect("应有标记");
        assert!(!mark.expired(), "刚创建不该过期");
    }

    #[test]
    fn 整句要选过两次才算() {
        // 一次选完整句（rime 给的整句候选）时，那一次本身已经按整串记下了
        assert!(!should_report_sentence(1, 7), "只选一次不报整句");
        // 分段上屏：今天 + 天气
        assert!(should_report_sentence(2, 4), "选两次、四个字，就是整句");
        // 2026-11-04 起两个字也算：名字类输入只能靠这个（王佳、李雷）
        assert!(
            should_report_sentence(2, 2),
            "两个字、两次选择 = 用户明确要的组合"
        );
        assert!(should_report_sentence(3, 3));
        assert!(!should_report_sentence(2, 1), "单字仍然不学");
        assert!(should_report_sentence(3, 5));
    }

    #[test]
    fn 删掉最后一个词只撤回那个词() {
        // 打 jintiantianqi 选「今天」再选「天气」：整句标记 4 个字，词的标记 2 个字。
        let mut word = CommitMark::new("tianqi".to_string(), "天气".to_string());
        let mut sent = CommitMark::new("jintiantianqi".to_string(), "今天天气".to_string());
        // 连按两次退格：词删光了，整句还剩两个字。
        assert!(step_mark(&mut word).is_none());
        assert_eq!(
            step_mark(&mut word),
            Some(("tianqi".to_string(), "天气".to_string())),
            "最后一个词被删光，撤回这个词"
        );
        assert!(step_mark(&mut sent).is_none(), "整句还没删光");
        assert!(step_mark(&mut sent).is_none(), "整句还剩一个字");
        assert!(word.is_none(), "词已经撤回过，不该再来一次");
    }

    #[test]
    fn 一直删到整句没了整句也撤回() {
        let mut sent = CommitMark::new("jintiantianqi".to_string(), "今天天气".to_string());
        assert!(step_mark(&mut sent).is_none());
        assert!(step_mark(&mut sent).is_none());
        assert!(step_mark(&mut sent).is_none());
        assert_eq!(
            step_mark(&mut sent),
            Some(("jintiantianqi".to_string(), "今天天气".to_string())),
            "第四下把整句删光，撤回整句那条学习"
        );
        assert!(sent.is_none(), "撤回过就清空，不会重复撤回");
    }

    #[test]
    fn 过期的标记不再算误选() {
        let mut mark = CommitMark::new("nihao".to_string(), "你好".to_string());
        // 手动把截止时间推到过去，模拟用户 2 秒后才按退格。
        if let Some(m) = mark.as_mut() {
            m.deadline = std::time::Instant::now() - std::time::Duration::from_millis(1);
        }
        assert!(step_mark(&mut mark).is_none(), "过期就不算误选");
        assert!(mark.is_none(), "过期后槽位要清空");
    }

    #[test]
    fn 空闲时带_shift_或_capslock_的字母不认领() {
        // 这是大写/英文的入口：必须放行给应用
        assert!(
            !should_claim(VK_A_KEY, false, true, false, false),
            "Shift 按住应放行"
        );
        assert!(
            !should_claim(VK_A_KEY, false, false, true, false),
            "CapsLock 开着应放行"
        );
    }

    #[test]
    fn 空闲时标点和功能键不认领() {
        assert!(!should_claim(VK_SPACE_KEY, false, false, false, false));
        assert!(!should_claim(VK_RETURN_KEY, false, false, false, false));
        assert!(!should_claim(VK_TAB_KEY, false, false, false, false));
        assert!(!should_claim(VK_LEFT_KEY, false, false, false, false));
        assert!(
            should_claim(VK_OEM_5, false, false, false, false),
            "标点键必须认领，否则不调 OnKeyDown"
        );
    }

    #[test]
    fn 拼写中认领候选操作键() {
        assert!(should_claim(VK_A_KEY, true, false, false, false));
        assert!(should_claim(VK_SPACE_KEY, true, false, false, false));
        assert!(should_claim(VK_RETURN_KEY, true, false, false, false));
        assert!(should_claim(VK_BACK_KEY, true, false, false, false));
        assert!(should_claim(VK_ESCAPE_KEY, true, false, false, false));
        assert!(should_claim(VK_LEFT_KEY, true, false, false, false));
    }

    #[test]
    fn 拼写中认领翻页键() {
        // `-` 现在是拼音分词符，翻页改由 PageUp/PageDown 承担
        assert!(should_claim(VK_OEM_MINUS, true, false, false, false), "-");
        assert!(should_claim(VK_OEM_MINUS, true, true, false, false), "_");
        assert!(should_claim(0x21, true, false, false, false), "PageUp");
        assert!(should_claim(0x22, true, false, false, false), "PageDown");
        // 空闲时不认领 PageUp/PageDown，让应用自己滚文档
        assert!(!should_claim(0x21, false, false, false, false));
        assert!(!should_claim(0x22, false, false, false, false));
    }

    #[test]
    fn 分词符不参与音节查询() {
        assert_eq!(strip_separators("xiang-mu"), "xiangmu");
        assert_eq!(strip_separators("xiang_mu"), "xiangmu");
        assert_eq!(strip_separators("xiang-mu-"), "xiangmu");
        assert_eq!(strip_separators("xiangmu"), "xiangmu");
        assert_eq!(strip_separators("xi'an"), "xian");
        assert_eq!(strip_separators("-"), "");
        assert_eq!(strip_separators(""), "");
    }

    #[test]
    fn 分词符统一成音节边界() {
        // 打 `xi-an` / `xi_an` 都是「要 xi 和 an 两个音节」
        assert_eq!(normalize_boundaries("xi-an"), "xi'an");
        assert_eq!(normalize_boundaries("xi_an"), "xi'an");
        assert_eq!(normalize_boundaries("xi--an"), "xi'an");
        assert_eq!(normalize_boundaries("xi'an"), "xi'an");
        // 没打边界的不要凭空造边界
        assert_eq!(normalize_boundaries("xian"), "xian");
        // 首尾的边界没有意义，去掉
        assert_eq!(normalize_boundaries("-xian-"), "xian");
        assert_eq!(normalize_boundaries(""), "");
        // 边界位置原样保留（`xiang-mu` 是 xiang | mu）
        assert_eq!(normalize_boundaries("xiang-mu"), "xiang'mu");
    }

    #[test]
    fn 拼写中不认领_tab() {
        assert!(!should_claim(VK_TAB_KEY, true, false, false, false));
    }

    #[test]
    fn 数字后面的标点是半角() {
        // `3.14`、`1,000`、`10:30` 里的符号
        assert_eq!(digit_pair(VK_OEM_PERIOD, false), Some(("。", ".")));
        assert_eq!(digit_pair(VK_OEM_COMMA, false), Some(("，", ",")));
        assert_eq!(digit_pair(VK_OEM_1, true), Some(("：", ":")));
        // 不是这几个键的一律走普通标点
        assert_eq!(digit_pair(VK_OEM_1, false), None, "分号不受影响");
        assert_eq!(digit_pair(VK_OEM_PERIOD, true), None, "Shift+. 是》");
        assert_eq!(digit_pair(VK_OEM_2, false), None);
    }

    #[test]
    fn 顿号是反斜杠() {
        assert_eq!(cn_punct(VK_OEM_5, false), Some("、"));
    }

    #[test]
    fn 中文标点不按_shift() {
        assert_eq!(cn_punct(VK_OEM_COMMA, false), Some("，"));
        assert_eq!(cn_punct(VK_OEM_PERIOD, false), Some("。"));
        assert_eq!(cn_punct(VK_OEM_1, false), Some("；"));
        assert_eq!(cn_punct(VK_OEM_2, false), Some("、"), "斜杠也出顿号");
        assert_eq!(cn_punct(VK_OEM_4, false), Some("【"));
        assert_eq!(cn_punct(VK_OEM_6, false), Some("】"));
        // 减号/等号不在标点表里，见 `减号等号永远是半角`
        assert_eq!(cn_punct(VK_OEM_MINUS, false), None);
        assert_eq!(cn_punct(VK_OEM_PLUS, false), None);
    }

    #[test]
    fn 减号等号永远是半角() {
        // Excel 里 `=A1-B1` 必须是半角，全角 `＝－` 公式就不成立了（用户实测打不出来）
        assert_eq!(halfwidth_symbol(VK_OEM_MINUS, false), Some("-"));
        assert_eq!(halfwidth_symbol(VK_OEM_PLUS, false), Some("="));
        assert_eq!(halfwidth_symbol(VK_OEM_MINUS, true), Some("_"));
        assert_eq!(halfwidth_symbol(VK_OEM_PLUS, true), Some("+"));
        assert_eq!(halfwidth_symbol(VK_OEM_COMMA, false), None);
    }

    #[test]
    fn 空闲时不认领减号等号() {
        // 不认领 → 应用自己处理 → Excel 的公式、代码里的 `-` `=` 原样进去
        assert!(!should_claim(VK_OEM_MINUS, false, false, false, false));
        assert!(!should_claim(VK_OEM_PLUS, false, false, false, false));
        assert!(!should_claim(VK_OEM_MINUS, false, true, false, false));
        // 但拼写中还是要认领（`-` 是分词符，展开态是翻页）
        assert!(should_claim(VK_OEM_MINUS, true, false, false, false));
        assert!(should_claim(VK_OEM_PLUS, true, false, false, false));
    }

    #[test]
    fn shift_加号算组合键不算单击切换() {
        // 用户实测：打 Shift+= / Shift+- 之后掉进英文模式。
        // 原因是我们不认领这两个键 → 没有 OnKeyDown → shift_used 没置位 → 松开 Shift 当成单击。
        const VK_SHIFT: u32 = 0x10;
        assert!(is_shift_chord(VK_OEM_PLUS, true, false));
        assert!(is_shift_chord(VK_OEM_MINUS, true, false));
        assert!(is_shift_chord(VK_OEM_2, true, false)); // Shift+/ → ？
        assert!(is_shift_chord(0x41, true, false)); // Shift+A
        // Shift 本身不算（它就是"单击切换"的那个键）
        assert!(!is_shift_chord(VK_SHIFT, true, false));
        // 没按 Shift 不算
        assert!(!is_shift_chord(VK_OEM_PLUS, false, false));
        // Ctrl+Shift 是快捷键，也不算
        assert!(!is_shift_chord(VK_OEM_PLUS, true, true));
    }

    #[test]
    fn 中文标点按_shift_也能出() {
        // ? ! : 这些本来就要按 Shift，不能因为 Shift 就不转换
        assert_eq!(cn_punct(VK_OEM_2, true), Some("？"));
        assert_eq!(cn_punct(VK_OEM_1, true), Some("："));
        assert_eq!(cn_punct(VK_OEM_COMMA, true), Some("《"));
        assert_eq!(cn_punct(VK_OEM_PERIOD, true), Some("》"));
        assert_eq!(cn_punct(VK_OEM_4, true), Some("「"));
        assert_eq!(cn_punct(VK_OEM_6, true), Some("」"));
    }

    #[test]
    fn 字母不是标点() {
        assert_eq!(cn_punct(VK_A_KEY, false), None);
        assert_eq!(cn_punct(VK_SPACE_KEY, false), None);
    }

    #[test]
    fn 括号和叹号走全角() {
        // 用户反馈：`(` `)` 原来漏给应用，打出来是半角
        assert_eq!(cn_punct(VK_9, true), Some("（"));
        assert_eq!(cn_punct(VK_0, true), Some("）"));
        assert_eq!(cn_punct(VK_1, true), Some("！"));
        assert_eq!(cn_punct(VK_4, true), Some("￥"));
        // 不按 Shift 的数字是选候选用的，不能当标点
        assert_eq!(cn_punct(VK_9, false), None);
        assert_eq!(cn_punct(VK_0, false), None);
    }

    #[test]
    fn 引号成对交替() {
        // 第一次给左引号，第二次给右引号 —— 对齐主流输入法
        assert_eq!(pick_quote("“", true), "“");
        assert_eq!(pick_quote("“", false), "”");
        assert_eq!(pick_quote("‘", true), "‘");
        assert_eq!(pick_quote("‘", false), "’");
        // 不是引号的字符原样返回
        assert_eq!(pick_quote("，", false), "，");

        // 全局状态：连续按两次 `"` 得到 “”
        let a = resolve_punct("“");
        let b = resolve_punct("“");
        assert_ne!(a, b, "两次双引号必须一开一合");
        assert_eq!(format!("{a}{b}"), "“”");

        // 单引号是独立的一套状态
        let c = resolve_punct("‘");
        let d = resolve_punct("‘");
        assert_eq!(format!("{c}{d}"), "‘’");

        // 非引号不受状态影响
        assert_eq!(resolve_punct("，"), "，");
    }

    #[test]
    fn 引号按三下是开合开() {
        // 0.1.27 的 bug：`swap(false)` 每次都把状态写成 false，
        // 于是第一下左、之后永远右。这里用独立状态连按三次验证真能翻回来。
        let flag = AtomicBool::new(true);
        assert!(flip_quote(&flag), "第 1 下该是左引号");
        assert!(!flip_quote(&flag), "第 2 下该是右引号");
        assert!(flip_quote(&flag), "第 3 下必须翻回左引号");
        assert!(!flip_quote(&flag), "第 4 下又该是右引号");
    }
}
#[cfg(test)]
mod punct_claim_tests {
    use super::*;

    const VK_SLASH: u32 = 0xBF;

    #[test]
    fn 标点键必须认领否则微信里不转换() {
        // 微信这类应用：OnTestKeyDown 返回 false 就不调 OnKeyDown
        assert!(should_claim(VK_OEM_5, false, false, false, false), "反斜杠");
        assert!(should_claim(VK_SLASH, false, false, false, false), "斜杠");
        assert!(
            should_claim(VK_OEM_COMMA, false, false, false, false),
            "逗号"
        );
        assert!(
            should_claim(VK_OEM_PERIOD, false, false, false, false),
            "句号"
        );
    }

    #[test]
    fn 标点带_shift_也要认领() {
        // Shift+/ 要出 ？，必须认领后才能转换
        assert!(should_claim(VK_SLASH, false, true, false, false));
        assert!(
            should_claim(VK_OEM_1, false, true, false, false),
            "Shift+; 要出 ："
        );
    }

    #[test]
    fn 括号和引号必须认领() {
        // 不认领就等于漏给应用，用户看到的还是半角 ( ) 和英文引号
        assert!(
            should_claim(VK_9, false, true, false, false),
            "Shift+9 要出 （"
        );
        assert!(
            should_claim(VK_0, false, true, false, false),
            "Shift+0 要出 ）"
        );
        assert!(
            should_claim(VK_OEM_7, false, true, false, false),
            "Shift+' 要出 “"
        );
        assert!(
            should_claim(VK_OEM_7, false, false, false, false),
            "' 要出 ‘"
        );
        // 不按 Shift 的数字仍然给应用（不抢数字键）
        assert!(!should_claim(VK_9, false, false, false, false));
    }

    #[test]
    fn capslock_开着时不认领标点() {
        // CapsLock = 英文模式，标点保持英文
        assert!(!should_claim(VK_OEM_5, false, false, true, false));
        assert!(!should_claim(VK_SLASH, false, false, true, false));
    }

    #[test]
    fn 英文模式下一律不认领() {
        // 关键：WPS/Excel 这类应用，认领后 OnKeyDown 返回 FALSE 也不给回，
        // 表现为一个字母都打不出来。所以英文模式必须完全不认领。
        assert!(!should_claim(VK_A_KEY, false, false, false, true), "字母");
        assert!(!should_claim(VK_OEM_5, false, false, false, true), "标点");
        assert!(!should_claim(VK_SLASH, false, false, false, true), "斜杠");
        assert!(
            !should_claim(VK_SPACE_KEY, false, false, false, true),
            "空格"
        );
    }

    #[test]
    fn 空闲时功能键仍不认领() {
        assert!(!should_claim(VK_TAB_KEY, false, false, false, false));
        assert!(!should_claim(VK_SHIFT_KEY, false, false, false, false));
        assert!(!should_claim(VK_SPACE_KEY, false, false, false, false));
    }

    const VK_A_KEY: u32 = 0x41;
    const VK_TAB_KEY: u32 = 0x09;
    const VK_SHIFT_KEY: u32 = 0x10;
    const VK_SPACE_KEY: u32 = 0x20;

    #[test]
    fn 注入的退格按次数放行() {
        // 有些宿主删不掉字，我们注入原生退格让宿主自己删；
        // 注入的键会再回到自己这里，必须按次数逐一放行，不能永久放行。
        mark_injected_backspace(0);
        assert!(!injected_backspace_pending());
        assert!(!take_injected_backspace());

        mark_injected_backspace(2);
        assert!(injected_backspace_pending());
        assert!(take_injected_backspace(), "第一个注入键");
        assert!(injected_backspace_pending(), "还剩一个");
        assert!(take_injected_backspace(), "第二个注入键");
        assert!(!injected_backspace_pending(), "放行完了");
        assert!(!take_injected_backspace(), "之后不再放行");
    }

    #[test]
    fn 注入标记过期后不再放行用户自己的退格() {
        // 万一注入的键没被送到（宿主吞了），不能让标记一直挂着把用户真正的
        // 退格当成注入键放行 —— 那样误选就永远撤不回来了。
        *INJECTED_BACK.lock().unwrap_or_else(|p| p.into_inner()) = Some((
            std::time::Instant::now() - std::time::Duration::from_secs(5),
            3,
        ));
        assert!(!injected_backspace_pending());
        assert!(!take_injected_backspace());
    }

    #[test]
    fn 图标缺失时退回_dll_自身() {
        // 安装目录里有 inklink.ico 就用它；没有（开发机、或安装器还没跑）就退回 DLL，
        // 保证 AddLanguageProfile 拿到的永远是一个真实存在的路径 —— 传空指针会直接崩。
        let missing = r"C:\no-such-dir-inklink\inklink_tsf.dll";
        assert_eq!(super::registry::icon_path_for(missing), missing);

        let dll = r"C:\dsh\work\inklink\tsf\target-static\release\inklink_tsf.dll";
        let expected = std::path::Path::new(dll)
            .parent()
            .map(|d| d.join("inklink.ico"))
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| dll.to_string());
        assert_eq!(super::registry::icon_path_for(dll), expected);
    }
}

#[cfg(test)]
mod ai_undo_tests {
    use super::*;

    #[test]
    fn 撤回区间要落在合法位置上() {
        assert_eq!(revert_extent(10, 3), Some((7, 3)));
        assert_eq!(revert_extent(3, 3), Some((0, 3)), "正好在文档开头也行");
        assert_eq!(revert_extent(2, 3), None, "锚点不够长，必须拒绝");
        assert_eq!(revert_extent(10, 0), None, "没插入东西，不能删");
        assert_eq!(revert_extent(0, 1), None);
    }
}

#[cfg(test)]
mod sentence_code_tests {
    use super::*;

    #[test]
    fn 整句的码只在不是后缀时才拼接() {
        // 一次打完、剩下的码是后缀：保持原码
        assert_eq!(merge_sentence_code("pikeqiu", "keqiu"), "pikeqiu");
        assert_eq!(merge_sentence_code("xiangmu", "mu"), "xiangmu");
        // 先打 short、再打新的一段：拼起来
        assert_eq!(merge_sentence_code("wang", "jia"), "wangjia");
        assert_eq!(merge_sentence_code("zhang", "san"), "zhangsan");
        // 空串不动
        assert_eq!(merge_sentence_code("wang", ""), "wang");
    }

    #[test]
    fn 两个字也能算整句() {
        assert!(should_report_sentence(2, 2), "王佳：两个字、两次选择");
        assert!(!should_report_sentence(1, 2), "只选一次不算");
        assert!(!should_report_sentence(2, 1), "单字不学");
    }
}
