//! TSF 显示属性：给组合串（拼音）加下划线。
//!
//! TSF 里没有"给某段文字画下划线"这种直接调用，要走三样东西：
//!
//! 1. **`ITfDisplayAttributeProvider`**：告诉 TSF「这个 CLSID 能提供显示样式」。
//!    应用渲染时拿一个 `GUID` 来问我们，我们返回 `ITfDisplayAttributeInfo`，
//!    从中读到 `TF_DISPLAYATTRIBUTE`（虚线 + 线条颜色）。
//! 2. **`ITfCategoryMgr::RegisterGUID`**：把我们的属性 GUID 换成 TSF 的 atom。
//! 3. **`ITfProperty(GUID_PROP_ATTRIBUTE)`**：在组合串的 range 上写 `VT_I4(atom)`，
//!    应用就按这个样式画那段文字。
//!
//! 三个接口都挂在**同一个 CLSID** 上（`CLSID_INKLINK_TSF`）：TSF 会用
//! `CoCreateInstance(我们的 CLSID)` 再 `QueryInterface(ITfDisplayAttributeProvider)`，
//! 所以 `TextService` 一并实现它即可，不需要第二个 COM 类。注册也只是**加一个
//! category**（`GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER`），不动已有的键盘 TIP 注册。
//!
//! 不支持 `GUID_PROP_ATTRIBUTE` 的应用（Electron 系，比如 VS Code）会忽略它，
//! 拼音就还是普通文字——这不是 bug，是那些应用自己不画。

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use windows::Win32::Foundation::{COLORREF, E_INVALIDARG, E_NOTIMPL, E_POINTER};
use windows::Win32::System::Variant::{
    VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_EMPTY, VT_I4,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_CategoryMgr, GUID_PROP_ATTRIBUTE, IEnumTfDisplayAttributeInfo,
    IEnumTfDisplayAttributeInfo_Impl, ITfCategoryMgr, ITfContext, ITfDisplayAttributeInfo,
    ITfDisplayAttributeInfo_Impl, ITfDisplayAttributeProvider, ITfDisplayAttributeProvider_Impl,
    ITfRange, TF_ATTR_INPUT, TF_CT_COLORREF, TF_CT_NONE, TF_DA_COLOR, TF_DA_COLOR_0,
    TF_DISPLAYATTRIBUTE, TF_LS_DOT,
};
use windows::core::{BSTR, GUID, Result, implement};

/// 组合串的显示属性 GUID（固定值；应用缓存的就是它，改了等于换了套样式）
pub const GUID_INKLINK_ATTRIBUTE: GUID = GUID::from_u128(0x3a7c1d92_5b64_4f18_8e73_2c9a4d6b1e50);

/// 虚线的颜色：和候选窗高亮同一个橙（`candwin::SEL_COLOR`，注意 `COLORREF` 是 0x00BBGGRR）
const LINE_COLOR: COLORREF = COLORREF(0x0000_66FF);

/// TSF 的 atom（`ITfCategoryMgr::RegisterGUID` 的结果）。0 表示还没登记成功。
static ATTR_ATOM: AtomicU32 = AtomicU32::new(0);

/// 组合串的样式：只有下划线，不改文字颜色和背景。
fn attribute() -> TF_DISPLAYATTRIBUTE {
    TF_DISPLAYATTRIBUTE {
        crText: TF_DA_COLOR {
            r#type: TF_CT_NONE,
            Anonymous: TF_DA_COLOR_0 { nIndex: 0 },
        },
        crBk: TF_DA_COLOR {
            r#type: TF_CT_NONE,
            Anonymous: TF_DA_COLOR_0 { nIndex: 0 },
        },
        lsStyle: TF_LS_DOT,
        fBoldLine: windows::core::BOOL(0),
        crLine: TF_DA_COLOR {
            r#type: TF_CT_COLORREF,
            Anonymous: TF_DA_COLOR_0 { cr: LINE_COLOR },
        },
        bAttr: TF_ATTR_INPUT,
    }
}

// ─────────────────────────── 属性描述 ───────────────────────────

/// 描述一种属性：`GUID` + 样式。
#[implement(ITfDisplayAttributeInfo)]
struct AttrInfo;

impl ITfDisplayAttributeInfo_Impl for AttrInfo_Impl {
    fn GetGUID(&self) -> Result<GUID> {
        Ok(GUID_INKLINK_ATTRIBUTE)
    }

    fn GetDescription(&self) -> Result<BSTR> {
        // 这个名字会出现在应用的"显示属性"调试列表里，用英文更省事
        Ok(BSTR::from("inklink composition"))
    }

    fn GetAttributeInfo(&self, pda: *mut TF_DISPLAYATTRIBUTE) -> Result<()> {
        if pda.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: 上面判过非空，调用方保证可写
        unsafe { *pda = attribute() };
        Ok(())
    }

    fn SetAttributeInfo(&self, _pda: *const TF_DISPLAYATTRIBUTE) -> Result<()> {
        // 只读属性：样式写死在代码里
        Err(E_NOTIMPL.into())
    }

    fn Reset(&self) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

// ─────────────────────────── 枚举 ───────────────────────────

/// 我们只有一种属性，枚举器就是"一个元素的列表"。
///
/// 索引放原子变量里：接口方法的签名都是 `&self`，而 `Next`/`Reset`/`Skip`
/// 要改状态——换成 `Mutex` 也行，但这里只有一次比较，原子更省事。
#[implement(IEnumTfDisplayAttributeInfo)]
struct AttrEnum {
    index: AtomicUsize,
}

impl AttrEnum {
    fn new() -> Self {
        Self {
            index: AtomicUsize::new(0),
        }
    }

    fn boxed() -> IEnumTfDisplayAttributeInfo {
        AttrEnum::new().into()
    }

    /// 取下一批：我们只有一个元素，所以要么给 1 个，要么给 0 个。
    ///
    /// 逻辑放在固有方法里（而不是只写在 trait impl 里），测试不用造 COM 对象。
    fn fetch_next(
        &self,
        ulcount: u32,
        rginfo: *mut Option<ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        if rginfo.is_null() {
            return Err(E_POINTER.into());
        }
        let mut got = 0u32;
        if ulcount > 0 && self.index.load(Ordering::SeqCst) == 0 {
            // SAFETY: rginfo 非空，且调用方按 ulcount 给了数组；ulcount > 0 保证第一格可写
            unsafe { *rginfo = Some(AttrInfo.into()) };
            self.index.store(1, Ordering::SeqCst);
            got = 1;
        }
        if !pcfetched.is_null() {
            // SAFETY: 上面判过非空
            unsafe { *pcfetched = got };
        }
        Ok(())
    }

    fn reset_index(&self) {
        self.index.store(0, Ordering::SeqCst);
    }

    fn skip_items(&self, ulcount: u32) {
        let next = self
            .index
            .load(Ordering::SeqCst)
            .saturating_add(ulcount as usize);
        self.index.store(next.min(1), Ordering::SeqCst);
    }
}

impl IEnumTfDisplayAttributeInfo_Impl for AttrEnum_Impl {
    fn Clone(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        Ok(AttrEnum {
            index: AtomicUsize::new(self.index.load(Ordering::SeqCst)),
        }
        .into())
    }

    fn Next(
        &self,
        ulcount: u32,
        rginfo: *mut Option<ITfDisplayAttributeInfo>,
        pcfetched: *mut u32,
    ) -> Result<()> {
        self.fetch_next(ulcount, rginfo, pcfetched)
    }

    fn Reset(&self) -> Result<()> {
        self.reset_index();
        Ok(())
    }

    fn Skip(&self, ulcount: u32) -> Result<()> {
        self.skip_items(ulcount);
        Ok(())
    }
}

// ─────────────────────────── 提供者 ───────────────────────────

/// 给 `TextService` 用的两个实现（TSF 用我们的 CLSID 直接 CoCreate 出 provider）。
pub fn enum_info() -> Result<IEnumTfDisplayAttributeInfo> {
    Ok(AttrEnum::boxed())
}

pub fn info_for(pguid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
    if pguid.is_null() {
        return Err(E_POINTER.into());
    }
    // SAFETY: 上面判过非空
    unsafe {
        if *pguid == GUID_INKLINK_ATTRIBUTE {
            Ok(AttrInfo.into())
        } else {
            Err(E_INVALIDARG.into())
        }
    }
}

/// 让 `ITfDisplayAttributeProvider` 的两个方法在非 COM 上下文里也能直接用
/// （测试、以及万一以后要把 provider 拆成独立类）。
#[implement(ITfDisplayAttributeProvider)]
pub struct AttributeProvider;

impl ITfDisplayAttributeProvider_Impl for AttributeProvider_Impl {
    fn EnumDisplayAttributeInfo(&self) -> Result<IEnumTfDisplayAttributeInfo> {
        enum_info()
    }

    fn GetDisplayAttributeInfo(&self, pguid: *const GUID) -> Result<ITfDisplayAttributeInfo> {
        info_for(pguid)
    }
}

// ─────────────────────────── 登记 atom ───────────────────────────

/// 把属性 GUID 登记成 TSF atom（幂等）。
///
/// 必须在 `Activate` 之后调：那时线程里的 COM 已经初始化好了。
/// 失败不算错（应用只是不会给拼音画线），所以返回值只用来写日志。
pub fn ensure_atom() -> u32 {
    let cached = ATTR_ATOM.load(Ordering::SeqCst);
    if cached != 0 {
        return cached;
    }
    // SAFETY: 只读调用；CategoryMgr 是系统提供的 in-proc 单例
    let atom = unsafe {
        use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
        match CoCreateInstance::<_, ITfCategoryMgr>(
            &CLSID_TF_CategoryMgr,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            Ok(mgr) => mgr.RegisterGUID(&GUID_INKLINK_ATTRIBUTE).unwrap_or(0),
            Err(_) => 0,
        }
    };
    if atom != 0 {
        ATTR_ATOM.store(atom, Ordering::SeqCst);
    }
    atom
}

/// 已经登记过的 atom（0 = 没有）。
pub fn attr_atom() -> u32 {
    ATTR_ATOM.load(Ordering::SeqCst)
}

fn variant_i4(v: i32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: v },
            }),
        },
    }
}

fn variant_empty() -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_EMPTY,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: 0 },
            }),
        },
    }
}

/// 给这段 range 打上"组合串"的标记（应用据此画虚线）。
///
/// 必须在读写编辑会话里调。atom 没登记上就什么也不做。
pub fn mark(ctx: &ITfContext, ec: u32, range: &ITfRange) -> &'static str {
    let atom = attr_atom();
    if atom == 0 {
        return "no atom";
    }
    // SAFETY: 标准 TSF 调用；range 来自同一个上下文，生命周期由调用方保证
    unsafe {
        let Ok(prop) = ctx.GetProperty(&GUID_PROP_ATTRIBUTE) else {
            return "GetProperty failed";
        };
        let var = variant_i4(atom as i32);
        match prop.SetValue(ec, range, &var) {
            Ok(()) => "ok",
            Err(_) => "SetValue failed",
        }
    }
}

/// 把标记清掉。
///
/// 上屏前必须清：否则那几个字会带着"组合串"的样式留在文档里，
/// 表现为**打出去的字一直是虚线的**。
pub fn clear(ctx: &ITfContext, ec: u32, range: &ITfRange) {
    // SAFETY: 同 `mark`
    unsafe {
        let Ok(prop) = ctx.GetProperty(&GUID_PROP_ATTRIBUTE) else {
            return;
        };
        let var = variant_empty();
        let _ = prop.SetValue(ec, range, &var);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 样式必须是"点线 + 橙色线条，不改字色和背景"。
    #[test]
    fn 组合串的样式是点线() {
        let a = attribute();
        assert_eq!(a.lsStyle, TF_LS_DOT);
        assert_eq!(a.crLine.r#type, TF_CT_COLORREF);
        assert_eq!(a.crText.r#type, TF_CT_NONE);
        assert_eq!(a.crBk.r#type, TF_CT_NONE);
        assert_eq!(a.bAttr, TF_ATTR_INPUT);
    }

    /// 只认我们自己的 GUID，别的要报错（否则应用拿不到样式会当成"没有下划线"，
    /// 但不该给它一个错的样式）。
    #[test]
    fn 只认自己的_guid() {
        let info = info_for(&GUID_INKLINK_ATTRIBUTE).expect("自己的 GUID 要认得");
        // SAFETY: info 是本进程内新建的 COM 对象
        let got = unsafe { info.GetGUID() }.unwrap();
        assert_eq!(got, GUID_INKLINK_ATTRIBUTE);

        let other = GUID::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
        assert!(info_for(&other).is_err());
    }

    /// 枚举器只有一个元素：第一次 `Next` 给 1 个，第二次给 0 个，`Reset` 后又能拿到。
    #[test]
    fn 枚举器只有一个元素() {
        let e = AttrEnum::new();
        let mut slot: Option<ITfDisplayAttributeInfo> = None;
        let mut fetched = 9u32;
        e.fetch_next(1, &mut slot, &mut fetched).unwrap();
        assert_eq!(fetched, 1);
        assert!(slot.is_some());

        let mut slot2: Option<ITfDisplayAttributeInfo> = None;
        let mut fetched2 = 9u32;
        e.fetch_next(1, &mut slot2, &mut fetched2).unwrap();
        assert_eq!(fetched2, 0);
        assert!(slot2.is_none());

        e.reset_index();
        let mut slot3: Option<ITfDisplayAttributeInfo> = None;
        let mut fetched3 = 0u32;
        e.fetch_next(1, &mut slot3, &mut fetched3).unwrap();
        assert_eq!(fetched3, 1);
    }
}
