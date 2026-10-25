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
use windows::core::BOOL;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfContext, ITfContextComposition,
    ITfEditSession, ITfEditSession_Impl, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr,
    ITfRange, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl,
    ITfThreadMgr, TF_ANCHOR_END, TF_ANCHOR_START, TF_DEFAULT_SELECTION, TF_ES_ASYNC,
    TF_ES_READWRITE, TF_SELECTION,
};
mod ipc;
mod candwin;
use windows::core::{GUID, HRESULT, Interface, Ref, implement};
use windows::core::IUnknownImpl;

/// 文本服务的 CLSID（固定值，注册表与代码必须一致）
pub const CLSID_INKLINK_TSF: GUID = GUID::from_u128(0x8f2a6c1e_4b3d_4f7a_9a21_5c6d7e8f9a0b);
/// 语言配置的 GUID
pub const GUID_PROFILE: GUID = GUID::from_u128(0x1b2c3d4e_5f60_4718_9a2b_3c4d5e6f7081);

const DLL_DESC: &str = "inklink 输入法";

/// 候选网格列数
const GRID_COLS: usize = 5;

/// 候选窗收起时每页 5 个（1 行）
const PAGE_SIZE_COLLAPSED: usize = 5;
/// 展开时每页 25 个（5 行 × 5 列）
const PAGE_SIZE_EXPANDED: usize = 25;

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
                return Err(windows::core::Error::from(windows::Win32::Foundation::E_POINTER));
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
    /// 取消：结束组合，清空
    Cancel,
    /// 插入一段文字到当前选区（中文标点用，没有组合）
    Insert(String),
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
    /// 最近一次 GetTextExt 拿到的组合串屏幕位置
    last_pos: Mutex<Option<(i32, i32)>>,
}

#[implement(ITfTextInputProcessorEx, ITfKeyEventSink, ITfEditSession, ITfCompositionSink)]
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
            },
        }
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> windows::core::Result<()> {
        self.advise_keys(ptim, tid)
    }

    fn Deactivate(&self) -> windows::core::Result<()> {
        log_line("deactivate");
        // 切换输入法/焦点时清空状态，否则缓冲会跨会话累积
        self.inner.buffer.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.inner.cands.lock().unwrap_or_else(|p| p.into_inner()).clear();
        *self.inner.matched_len.lock().unwrap_or_else(|p| p.into_inner()) = 0;
        *self.inner.context.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.inner.composition.lock().unwrap_or_else(|p| p.into_inner()) = None;
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
        self.advise_keys(ptim, tid)
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

        // 激活时顺手确保引擎在跑——引擎崩了下次切输入法就自动复活
        ipc::ensure_engine();

        // 把自己作为按键 sink 挂上去（传引用，而不是把 owned 接口交给 Param）
        let sink: ITfKeyEventSink = self.to_interface();
        let r = unsafe { keystroke_mgr.AdviseKeyEventSink(tid, &sink, true) };
        log_line(&format!("activate tid={tid} advise={}", if r.is_ok() { "ok" } else { "FAILED" }));
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
    }

    /// 把一个动作交给编辑会话执行（异步，安全，不阻塞按键线程）。
    fn request_edit(&self, action: EditAction) {
        *self.inner.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(action);
        let tid = *self.inner.tid.lock().unwrap();
        if tid == 0 {
            return;
        }
        if let Some(ctx) = self.inner.context.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            let sink: ITfEditSession = self.to_interface();
            // SAFETY: 标准 TSF 编辑会话请求，sink 生命周期由 COM 引用计数管理
            let _ = unsafe { ctx.RequestEditSession(tid, &sink, TF_ES_ASYNC | TF_ES_READWRITE) };
        }
    }

    /// 把输入上下文存下来（编辑会话要用）
    fn store_context(&self, pic: Ref<ITfContext>) {
        if let Some(ctx) = pic.as_ref() {
            *self.inner.context.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx.clone());
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
        let cands = self.inner.cands.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let page = *self.inner.page.lock().unwrap_or_else(|p| p.into_inner());
        let expanded = *self.inner.expanded.lock().unwrap_or_else(|p| p.into_inner());
        let sel = *self.inner.sel.lock().unwrap_or_else(|p| p.into_inner());
        let pos = *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner());
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
        let ctx = self
            .inner
            .context
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let Some(ctx) = ctx else {
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
                    self.place_caret(&ctx, ec, &range, cursor)?;
                    // 用 TSF 的 GetTextExt 问应用"我这段在屏幕哪"（和搜狗同款）
                    let pos = text_ext_pos(&ctx, ec, &range);
                    *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()) = pos;
                    self.refresh_cand_window();
                    // 写回（短锁，不跨越 TSF 调用）
                    *self
                        .inner
                        .composition
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(c);
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
                // 中文标点：直接替换当前选区，不需要组合
                let utf16: Vec<u16> = text.encode_utf16().collect();
                let mut sel = [TF_SELECTION::default()];
                let mut fetched = 0u32;
                // SAFETY: 标准 TSF 调用
                unsafe {
                    ctx.GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)?;
                    if let Some(range) = sel[0].range.as_ref() {
                        range.SetText(ec, 0, &utf16)?;
                        range.Collapse(ec, TF_ANCHOR_END)?;
                        let caret = [TF_SELECTION {
                            range: std::mem::ManuallyDrop::new(Some(range.clone())),
                            style: Default::default(),
                        }];
                        ctx.SetSelection(ec, &caret)?;
                    }
                }
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
            *self.inner.matched_len.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.sel.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.page.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.expanded.lock().unwrap_or_else(|p| p.into_inner()) = false;
            candwin::hide();
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
            *self.inner.matched_len.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.sel.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.page.lock().unwrap_or_else(|p| p.into_inner()) = 0;
            *self.inner.expanded.lock().unwrap_or_else(|p| p.into_inner()) = false;
            candwin::hide();
        }
        Ok(())
    }

    fn OnTestKeyDown(
        &self,
        _pic: Ref<ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> windows::core::Result<BOOL> {
        let vk = wparam.0 as u32;
        let composing = !self.inner.buffer.lock().unwrap_or_else(|p| p.into_inner()).is_empty();
        let claim = should_claim(vk, composing);
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

        let vk = wparam.0 as u32;

        // 存下输入上下文，编辑会话要用
        self.store_context(pic);

        // Ctrl/Alt/Win 组合键：整体放行给应用（Ctrl+A/C/V/X/Z 等快捷键）。
        // 注意必须放在 OnKeyDown 里——fForeground=TRUE 的接收器，TSF 会跳过
        // OnTestKeyDown 直接调 OnKeyDown，所以修饰键检查放 OnTestKeyDown 是死代码。
        if ctrl_alt_win_held() {
            return Ok(BOOL(0));
        }

        // 没在拼写、也不在英文模式时：把英文标点键换成中文标点（`\` → `、` 等）。
        // Shift 按住 / CapsLock 开着 = 英文输入，标点必须原样透传。
        let composing_now = !self
            .inner
            .buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty();
        if !composing_now && !shift_or_caps() {
            if let Some(ch) = cn_punct(vk) {
                self.request_edit(EditAction::Insert(ch.to_string()));
                return Ok(BOOL(1));
            }
        }

        let mut buffer = self.inner.buffer.lock().unwrap_or_else(|p| p.into_inner());
        let mut cands = self.inner.cands.lock().unwrap_or_else(|p| p.into_inner());
        let mut matched_len = self.inner.matched_len.lock().unwrap_or_else(|p| p.into_inner());
        let mut cursor = self.inner.cursor.lock().unwrap_or_else(|p| p.into_inner());
        let mut page = self.inner.page.lock().unwrap_or_else(|p| p.into_inner());
        let mut expanded = self.inner.expanded.lock().unwrap_or_else(|p| p.into_inner());
        let mut sel = self.inner.sel.lock().unwrap_or_else(|p| p.into_inner());
        let composing = !buffer.is_empty();

        match vk {
            // 字母：插入到内部光标处（拼音全 ASCII，字节索引即字符索引）
            VK_A..=VK_Z => {
                // 空闲 + Shift/CapsLock：是要打英文大写，放行给应用
                if !composing && shift_or_caps() {
                    return Ok(BOOL(0));
                }
                let at = (*cursor).min(buffer.len());
                buffer.insert(at, (vk as u8 as char).to_ascii_lowercase());
                *cursor = at + 1;
                (*cands, *matched_len) = query_with_fallback(&buffer);
                *page = 0;
                *expanded = false;
                *sel = 0;
                log_line(&format!(
                    "compose buffer={} cursor={} cands={} matched={} top={:?}",
                    *buffer,
                    *cursor,
                    cands.len(),
                    *matched_len,
                    cands.first()
                ));
                // 显示候选窗
                show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
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
                        self.request_edit(EditAction::Cancel);
                    } else {
                        show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
                        self.request_edit(EditAction::Compose(buffer.clone(), *cursor));
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
                        self.request_edit(EditAction::Cancel);
                    } else {
                        show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
                        self.request_edit(EditAction::Compose(buffer.clone(), *cursor));
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
                        if *sel % GRID_COLS > 0 {
                            *sel -= 1;
                        } else if *page > 0 {
                            *page -= 1;
                            let page_start = *page * size;
                            let page_len = cands.len().saturating_sub(page_start).min(size);
                            *sel = page_len.saturating_sub(1);
                        }
                        show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
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
                        if *sel % GRID_COLS < GRID_COLS - 1 && *sel + 1 < page_len {
                            *sel += 1;
                        } else if *page + 1 < total_pages {
                            *page += 1;
                            *sel = 0;
                        }
                        show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
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
                        if *sel >= GRID_COLS {
                            *sel -= GRID_COLS;
                        } else if *page > 0 {
                            *page -= 1;
                            let page_start = *page * size;
                            let page_len = cands.len().saturating_sub(page_start).min(size);
                            *sel = page_len.saturating_sub(1);
                        }
                        show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
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
                        if *sel + GRID_COLS < page_len {
                            *sel += GRID_COLS;
                        } else if *page + 1 < total_pages {
                            *page += 1;
                            *sel = 0;
                        }
                    }
                    show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // - / , ：上一页（展开时首页则收起）
            VK_OEM_MINUS | VK_OEM_COMMA => {
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
                    show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // = / . ：下一页（收起时先展开）
            VK_OEM_PLUS | VK_OEM_PERIOD => {
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
                    show_cand_window(&buffer, &cands, *page, *expanded, *sel, *self.inner.last_pos.lock().unwrap_or_else(|p| p.into_inner()));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // 空格：上屏当前选中的候选
            VK_SPACE => {
                let pick_idx = *page * page_size(*expanded) + *sel;
                if let Some(top) = cands.get(pick_idx).cloned() {
                    // 上报选词，让引擎写进用户词库（失败不影响上屏）
                    ipc::select(pick_idx);
                    // 上屏后清空整个缓冲——多余/打错的字母一并删掉（用户要求）
                    buffer.clear();
                    cands.clear();
                    *matched_len = 0;
                    *cursor = 0;
                    candwin::hide();
                    log_line(&format!("commit {top}"));
                    self.request_edit(EditAction::Commit(top));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
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
                    log_line(&format!("commit-raw {raw}"));
                    self.request_edit(EditAction::Commit(raw));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // Shift / CapsLock：拼音还在组合时按下，直接把拼音当英文上屏（对齐搜狗）。
            // 空闲时放行（交给应用，或用于打英文大写）。
            VK_SHIFT | VK_CAPITAL => {
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
                    log_line(&format!("commit-raw(shift) {raw}"));
                    self.request_edit(EditAction::Commit(raw));
                    Ok(BOOL(1))
                } else {
                    Ok(BOOL(0))
                }
            }
            // 数字键：选当前页第 N 个候选
            VK_0..=VK_9 => {
                if buffer.is_empty() {
                    return Ok(BOOL(0));
                }
                let idx = (vk - VK_0) as usize;
                if idx >= 1 {
                    // 页内序号 → 全局下标
                    *sel = idx - 1;
                    let global = *page * page_size(*expanded) + (idx - 1);
                    if let Some(pick) = cands.get(global).cloned() {
                        ipc::select(global);
                        buffer.clear();
                        cands.clear();
                        *matched_len = 0;
                        *cursor = 0;
                        candwin::hide();
                        log_line(&format!("commit {pick}"));
                        self.request_edit(EditAction::Commit(pick));
                        return Ok(BOOL(1));
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
        Ok(BOOL(0))
    }

    fn OnKeyUp(
        &self,
        _pic: Ref<ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> windows::core::Result<BOOL> {
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

/// 把按键记到临时目录的日志（按进程分文件，便于诊断按键流是否到达）。
///
/// 这个键要不要由输入法认领。
///
/// 原则：**只认领真正要处理的**。方向键、Tab、F 键一律放行；
/// 这个键该不该由输入法认领（TSF 按键状态机）。
///
/// 规则（对照主流输入法）：
/// - **Ctrl/Alt/Win 按下**：永远放行（是快捷键，如 Ctrl+A/Ctrl+C）
/// - **正在拼写**：认领 A-Z、数字、退格、空格、Esc（候选相关操作）
/// - **空闲**：只认领"无 Shift/CapsLock 的纯字母"来启动拼写；
///   Tab、CapsLock、Shift、Enter、空格、方向键、退格、符号 → 全部放行给应用
fn should_claim(vk: u32, composing: bool) -> bool {
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

    // 快捷键永远放行
    if ctrl_alt_win_held() {
        return false;
    }

    if composing {
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
                | VK_OEM_COMMA
                | VK_OEM_PERIOD
        );
    }

    // 空闲：只有"无 Shift/CapsLock 的纯字母"才启动拼写
    if (VK_A..=VK_Z).contains(&vk) && !shift_or_caps() {
        return true;
    }
    false
}

/// 当前每页候选数：收起 5，展开 25。
fn page_size(expanded: bool) -> usize {
    if expanded {
        PAGE_SIZE_EXPANDED
    } else {
        PAGE_SIZE_COLLAPSED
    }
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
    pos: Option<(i32, i32)>,
) {
    let size = page_size(expanded);
    let total_pages = (cands.len() + size - 1) / size;
    let start = page * size;
    if start >= cands.len() {
        candwin::hide();
        return;
    }
    let end = (start + size).min(cands.len());
    let slice = &cands[start..end];
    let (x, y) = pos
        .or_else(ipc::caret_screen_pos)
        .unwrap_or((200, 200));
    candwin::show(pinyin, slice, sel, page + 1, total_pages, x + 4, y + 8);
}

/// 用 TSF 的 `ITfContextView::GetTextExt` 问应用"这段组合串在屏幕的哪个矩形里"。
///
/// 这是主流输入法的做法：坐标由应用自己算，所以 VSCode / WPS / 浏览器都精准。
/// 失败返回 `None`（老式应用或应用没实现时），调用方再退到光标/鼠标兜底。
fn text_ext_pos(ctx: &ITfContext, ec: u32, range: &ITfRange) -> Option<(i32, i32)> {
    // SAFETY: 标准 TSF 查询，处于编辑会话内
    unsafe {
        let view = ctx.GetActiveView().ok()?;
        let mut rect: RECT = std::mem::zeroed();
        let mut clipped = BOOL(0);
        view.GetTextExt(ec, range, &mut rect, &mut clipped).ok()?;
        if rect.left == 0 && rect.top == 0 && rect.right == 0 && rect.bottom == 0 {
            return None;
        }
        Some((rect.left, rect.bottom))
    }
}

/// 查候选；整串查不到就逐步缩短，返回 (候选, 命中的前缀长度)。
///
/// 这样 `xiangmm` 会退到 `xiang` 出候选，而不是整串查不到直接卡死。
fn query_with_fallback(code: &str) -> (Vec<String>, usize) {
    if code.is_empty() {
        return (Vec::new(), 0);
    }
    let mut probe = code.to_string();
    while probe.len() >= 1 {
        let r = ipc::query(&probe);
        if !r.is_empty() {
            return (r, probe.len());
        }
        probe.pop();
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
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_SHIFT: i32 = 0x10;
    const VK_CAPITAL: i32 = 0x14;
    // SAFETY: 纯查询；Shift 看高位（按下），CapsLock 看低位（开关状态）
    unsafe {
        (GetAsyncKeyState(VK_SHIFT) as u16 & 0x8000) != 0
            || (GetAsyncKeyState(VK_CAPITAL) as u16 & 0x0001) != 0
    }
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
/// 只在"中文模式且没按 Shift"时调用；`\` → `、` 就是顿号。
/// 返回 `None` 表示不是标点键，交给应用。
fn cn_punct(vk: u32) -> Option<&'static str> {
    Some(match vk {
        VK_OEM_COMMA => "，",
        VK_OEM_PERIOD => "。",
        VK_OEM_1 => "；",
        VK_OEM_2 => "／",
        VK_OEM_3 => "·",
        VK_OEM_4 => "【",
        VK_OEM_5 => "、",
        VK_OEM_6 => "】",
        VK_OEM_7 => "‘",
        VK_OEM_MINUS => "－",
        VK_OEM_PLUS => "＝",
        _ => return None,
    })
}

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
    let path = format!(
        "C:\\Users\\Public\\inklink-tsf-{}.log",
        std::process::id()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
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

#[unsafe(no_mangle)]
pub extern "system" fn DllGetClassObject(
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
        factory.query(riid, ppv).ok().map_or_else(
            |_| E_NOINTERFACE,
            |_| S_OK,
        )
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
pub extern "system" fn DllMain(_inst: HINSTANCE, _reason: u32, _reserved: *mut core::ffi::c_void) -> BOOL {
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
        HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE,
        REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
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
        let bytes = unsafe {
            core::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2)
        };
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
        if rc == ERROR_SUCCESS { Ok(hkey) } else { Err(rc) }
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
    fn dll_path() -> String {
        use windows::Win32::System::LibraryLoader::{
            GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
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
            CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles, GUID_TFCAT_TIP_KEYBOARD,
            ITfCategoryMgr, ITfInputProcessorProfiles,
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
                drop(catmgr);

                let desc: Vec<u16> = DLL_DESC.encode_utf16().collect();
                let icon: Vec<u16> = dll_path.encode_utf16().collect();
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
                let _ = profiles.EnableLanguageProfile(&CLSID_INKLINK_TSF, LANG_ZH_CN, &GUID_PROFILE, false);
                let _ = profiles.RemoveLanguageProfile(&CLSID_INKLINK_TSF, LANG_ZH_CN, &GUID_PROFILE);
                let _ = profiles.Unregister(&CLSID_INKLINK_TSF);
                drop(profiles);
            }
            if init.is_ok() {
                CoUninitialize();
            }
        }
    }
}
