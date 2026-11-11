//! AI 原文编辑器：普通原生窗口承载多行 EDIT，避开分层面板无法可靠承载子控件的问题。
use std::cell::Cell;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    static WINDOW: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
    static EDIT: Cell<HWND> = const { Cell::new(std::ptr::null_mut()) };
}
const APPLY: usize = 101;
const CANCEL: usize = 102;
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn open(source: &str) {
    unsafe {
        let existing = WINDOW.with(Cell::get);
        if !existing.is_null() && IsWindow(existing) != 0 {
            ShowWindow(existing, SW_RESTORE);
            SetForegroundWindow(existing);
            return;
        }
        let class = wide("inklink-source-editor");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_IBEAM),
            hbrBackground: (windows_sys::Win32::Graphics::Gdi::COLOR_WINDOW + 1) as _,
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            wide("编辑 AI 原文 — 可滚动、选择和修改").as_ptr(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            680,
            500,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return;
        }
        WINDOW.with(|w| w.set(hwnd));
        let edit = EDIT.with(Cell::get);
        // 不设 ES_AUTOHSCROLL：按窗口宽度自动换行；滚轮/滚动条可浏览全文。
        SendMessageW(edit, 0x00C5, 65535, 0); // EM_SETLIMITTEXT（UTF-16 单位）
        SetWindowTextW(edit, wide(source).as_ptr());
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(edit);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                let edit = CreateWindowExW(
                    WS_EX_CLIENTEDGE,
                    wide("EDIT").as_ptr(),
                    wide("").as_ptr(),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | WS_VSCROLL
                        | ES_MULTILINE as u32
                        | ES_AUTOVSCROLL as u32
                        | ES_WANTRETURN as u32,
                    12,
                    12,
                    630,
                    380,
                    hwnd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                );
                EDIT.with(|e| e.set(edit));
                for (id, label) in [(APPLY, "应用修改"), (CANCEL, "取消")] {
                    CreateWindowExW(
                        0,
                        wide("BUTTON").as_ptr(),
                        wide(label).as_ptr(),
                        WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                        if id == APPLY { 12 } else { 140 },
                        410,
                        116,
                        30,
                        hwnd,
                        id as _,
                        std::ptr::null_mut(),
                        std::ptr::null(),
                    );
                }
                0
            }
            WM_SIZE => {
                let (w, h) = (
                    (lp as u32 & 0xffff) as i32,
                    ((lp as u32 >> 16) & 0xffff) as i32,
                );
                MoveWindow(
                    EDIT.with(Cell::get),
                    12,
                    12,
                    (w - 24).max(1),
                    (h - 66).max(1),
                    1,
                );
                MoveWindow(
                    GetDlgItem(hwnd, APPLY as i32),
                    12,
                    (h - 42).max(0),
                    116,
                    30,
                    1,
                );
                MoveWindow(
                    GetDlgItem(hwnd, CANCEL as i32),
                    140,
                    (h - 42).max(0),
                    116,
                    30,
                    1,
                );
                0
            }
            WM_COMMAND => {
                match wp & 0xffff {
                    APPLY => {
                        let edit = EDIT.with(Cell::get);
                        let n = GetWindowTextLengthW(edit).max(0) as usize;
                        let mut buf = vec![0u16; n + 1];
                        let read = GetWindowTextW(edit, buf.as_mut_ptr(), buf.len() as i32).max(0)
                            as usize;
                        let text = String::from_utf16_lossy(&buf[..read]);
                        if text.trim().is_empty()
                            || text.chars().count() > crate::ai::MAX_INPUT_CHARS
                        {
                            MessageBoxW(
                                hwnd,
                                wide(&format!(
                                    "请输入 1～{} 字原文，修改内容仍保留在窗口内。",
                                    crate::ai::MAX_INPUT_CHARS
                                ))
                                .as_ptr(),
                                wide("AI 原文").as_ptr(),
                                MB_OK,
                            );
                            return 0;
                        }
                        crate::ai_panel::set_source_pasted(&text);
                        DestroyWindow(hwnd);
                    }
                    CANCEL => {
                        DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                0
            }
            WM_DESTROY => {
                WINDOW.with(|w| w.set(std::ptr::null_mut()));
                EDIT.with(|e| e.set(std::ptr::null_mut()));
                0 // 宿主线程的消息循环归宿主管理，不能 PostQuitMessage。
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
