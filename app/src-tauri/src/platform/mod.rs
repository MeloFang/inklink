//! 平台层：热键注册、窗口显隐、上屏、剪贴板。
//!
//! Windows 专有 API 集中在这里，便于将来隔离与替换。

/// 关掉某个窗口（含全部子窗口）的输入法关联（IME）。
///
/// 面板自己就是输入法，不能让系统输入法截走按键：否则用户敲 `xiangmu`，
/// 搜狗/微软拼音会先弹它们自己的候选框，我们的输入框收到的是空气。
///
/// 坑：WebView2 的输入框是**另一个进程**创建的子窗口，只对顶层窗口摘上下文没用，
/// 必须把子树都走一遍。
#[cfg(windows)]
pub fn disable_ime(hwnd: isize) {
    if hwnd == 0 {
        return;
    }

    unsafe extern "system" {
        fn ImmAssociateContext(hwnd: isize, himc: isize) -> isize;
        fn EnumChildWindows(
            parent: isize,
            callback: unsafe extern "system" fn(isize, isize) -> i32,
            lparam: isize,
        ) -> i32;
    }

    unsafe extern "system" fn on_child(child: isize, _lparam: isize) -> i32 {
        unsafe extern "system" {
            fn ImmAssociateContext(hwnd: isize, himc: isize) -> isize;
        }
        // SAFETY: child 是系统回调给出的有效 HWND；失败返回 0 也不需要处理
        unsafe {
            ImmAssociateContext(child, 0);
        }
        1 // 继续枚举
    }

    // SAFETY: 句柄由调用方给出；回调只做一次 IME 解绑
    unsafe {
        ImmAssociateContext(hwnd, 0);
        EnumChildWindows(hwnd, on_child, 0);
    }
}

#[cfg(not(windows))]
pub fn disable_ime(_hwnd: isize) {}
