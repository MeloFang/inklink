//! inklink 的 TSF 文本服务（薄壳）。
//!
//! 设计约束（见 `docs/tsf-plan.md`）：
//! - DLL 里**不放引擎**：词库、切分、排序都在独立进程，这里只做转发
//! - `DllMain` 什么都不做；不启动线程
//! - `DllCanUnloadNow` **永远返回 S_FALSE**（上一版就死在这条）
//!
//! 当前阶段：T1 —— 能被注册、能被系统当成输入法选中，按键暂全部透传。

use std::sync::Mutex;

use windows::Win32::Foundation::{
    CLASS_E_NOAGGREGATION, E_NOINTERFACE, HINSTANCE, S_FALSE, S_OK,
};
use windows::core::BOOL;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::UI::TextServices::{
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr,
};
use windows::core::{GUID, HRESULT, Interface, Ref, implement};

/// 文本服务的 CLSID（固定值，注册表与代码必须一致）
pub const CLSID_INKLINK_TSF: GUID = GUID::from_u128(0x8f2a6c1e_4b3d_4f7a_9a21_5c6d7e8f9a0b);
/// 语言配置的 GUID
pub const GUID_PROFILE: GUID = GUID::from_u128(0x1b2c3d4e_5f60_4718_9a2b_3c4d5e6f7081);

const DLL_DESC: &str = "inklink 输入法";

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

struct TextServiceInner {
    thread_mgr: Mutex<Option<ITfThreadMgr>>,
}

#[implement(ITfTextInputProcessorEx)]
struct TextService {
    inner: TextServiceInner,
}

impl TextService {
    fn new() -> Self {
        Self {
            inner: TextServiceInner {
                thread_mgr: Mutex::new(None),
            },
        }
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, _tid: u32) -> windows::core::Result<()> {
        if let Some(mgr) = ptim.as_ref() {
            *self.inner.thread_mgr.lock().unwrap() = Some(mgr.clone());
        }
        Ok(())
    }

    fn Deactivate(&self) -> windows::core::Result<()> {
        *self.inner.thread_mgr.lock().unwrap() = None;
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
        // 目前与 Activate 相同；后续在这里挂 ITfKeyEventSink
        ITfTextInputProcessor_Impl::Activate(self, ptim, tid)
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

                profiles.Register(&CLSID_INKLINK_TSF)?;

                // 关键：注册到「键盘输入法」分类。没有这一步，Set-WinUserLanguageList
                // 不会把它当作可选键盘，输入法列表里永远不出现。
                let catmgr: ITfCategoryMgr =
                    CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
                catmgr.RegisterCategory(
                    &CLSID_INKLINK_TSF,
                    &GUID_TFCAT_TIP_KEYBOARD,
                    &CLSID_INKLINK_TSF,
                )?;
                drop(catmgr);

                let desc: Vec<u16> = DLL_DESC.encode_utf16().collect();
                let icon: Vec<u16> = dll_path.encode_utf16().collect();
                profiles.AddLanguageProfile(
                    &CLSID_INKLINK_TSF,
                    LANG_ZH_CN,
                    &GUID_PROFILE,
                    &desc,
                    &icon,
                    0,
                )?;

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
