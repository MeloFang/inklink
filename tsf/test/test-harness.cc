// inklink 自动化测试台
//
// 原理：自己建一个带 EDIT 控件的窗口，用 TSF 接口把 inklink 激活到本线程，
// 然后 SendInput 模拟真实按键，最后读回 EDIT 里的文本做断言。
//
// 编译（x64，和 64 位 TSF DLL 配套）：
//   cl /nologo /utf-8 /EHsc /std:c++17 /O2 /MD test-harness.cc ole32.lib oleaut32.lib user32.lib gdi32.lib

#include <windows.h>
#include <msctf.h>
#include <cstdio>
#include <string>
#include <vector>

// inklink 的 CLSID / Profile GUID（与 lib.rs 一致）
static const GUID CLSID_INKLINK = {
    0x8F2A6C1E, 0x4B3D, 0x4F7A, {0x9A, 0x21, 0x5C, 0x6D, 0x7E, 0x8F, 0x9A, 0x0B}};
static const GUID GUID_INKLINK_PROFILE = {
    0x1B2C3D4E, 0x5F60, 0x4718, {0x9A, 0x2B, 0x3C, 0x4D, 0x5E, 0x6F, 0x70, 0x81}};

static HWND g_hwnd = nullptr;
static HWND g_edit = nullptr;
static int g_pass = 0;
static int g_fail = 0;

static void pump(int ms) {
    DWORD end = GetTickCount() + ms;
    MSG msg;
    while (GetTickCount() < end) {
        while (PeekMessage(&msg, nullptr, 0, 0, PM_REMOVE)) {
            TranslateMessage(&msg);
            DispatchMessage(&msg);
        }
        Sleep(5);
    }
}

static std::wstring edit_text() {
    int len = GetWindowTextLengthW(g_edit);
    std::wstring s(len + 1, L'\0');
    GetWindowTextW(g_edit, &s[0], len + 1);
    s.resize(len);
    return s;
}

static void edit_clear() {
    SetWindowTextW(g_edit, L"");
    pump(120);
}

// 发一个按键（vk 是虚拟键码；shift 表示同时按住 Shift）
static void key(BYTE vk, bool shift) {
    INPUT in[4] = {};
    int n = 0;
    if (shift) {
        in[n].type = INPUT_KEYBOARD;
        in[n].ki.wVk = VK_SHIFT;
        n++;
    }
    in[n].type = INPUT_KEYBOARD;
    in[n].ki.wVk = vk;
    n++;
    if (shift) {
        in[n].type = INPUT_KEYBOARD;
        in[n].ki.wVk = VK_SHIFT;
        in[n].ki.dwFlags = KEYEVENTF_KEYUP;
        n++;
    }
    SendInput(n, in, sizeof(INPUT));
    pump(45);
}

static void key_up(BYTE vk) {
    INPUT in = {};
    in.type = INPUT_KEYBOARD;
    in.ki.wVk = vk;
    in.ki.dwFlags = KEYEVENTF_KEYUP;
    SendInput(1, &in, sizeof(INPUT));
    pump(30);
}

static void key_down(BYTE vk) {
    INPUT in = {};
    in.type = INPUT_KEYBOARD;
    in.ki.wVk = vk;
    SendInput(1, &in, sizeof(INPUT));
    pump(30);
}

// 逐字母敲（小写）
static void type_ascii(const char* s) {
    for (const char* p = s; *p; ++p) {
        char c = *p;
        if (c >= 'a' && c <= 'z') key((BYTE)('A' + (c - 'a')), false);
        else if (c == ' ') key(VK_SPACE, false);
    }
}

static void expect(const char* name, const std::wstring& got, const std::wstring& want) {
    bool ok = (got == want);
    if (ok) {
        g_pass++;
        printf("  PASS  %s\n", name);
    } else {
        g_fail++;
        printf("  FAIL  %s\n", name);
        printf("        got  = \"%ls\"\n", got.c_str());
        printf("        want = \"%ls\"\n", want.c_str());
    }
}

// 后台进程直接 SetForegroundWindow 会被系统拒绝，先挂到前台线程的输入队列上
static void force_foreground(HWND hwnd) {
    HWND fg = GetForegroundWindow();
    DWORD fgThread = fg ? GetWindowThreadProcessId(fg, nullptr) : 0;
    DWORD myThread = GetCurrentThreadId();
    if (fgThread && fgThread != myThread) {
        AttachThreadInput(myThread, fgThread, TRUE);
    }
    ShowWindow(hwnd, SW_SHOW);
    SetForegroundWindow(hwnd);
    BringWindowToTop(hwnd);
    SetActiveWindow(hwnd);
    SetFocus(g_edit);
    if (fgThread && fgThread != myThread) {
        AttachThreadInput(myThread, fgThread, FALSE);
    }
}

static LRESULT CALLBACK wndproc(HWND h, UINT m, WPARAM w, LPARAM l) {
    if (m == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcW(h, m, w, l);
}

static bool create_window() {
    WNDCLASSW wc = {};
    wc.lpfnWndProc = wndproc;
    wc.hInstance = GetModuleHandleW(nullptr);
    wc.lpszClassName = L"inklink-test-harness";
    RegisterClassW(&wc);

    g_hwnd = CreateWindowExW(0, wc.lpszClassName, L"inklink test",
                             WS_OVERLAPPEDWINDOW, 100, 100, 500, 200,
                             nullptr, nullptr, wc.hInstance, nullptr);
    if (!g_hwnd) return false;
    // RichEdit 支持 TSF 文本服务；普通 EDIT 只走 IMM32，我们的 TSF 输入法收不到键
    LoadLibraryW(L"Msftedit.dll");
    g_edit = CreateWindowExW(0, L"RICHEDIT50W", L"",
                             WS_CHILD | WS_VISIBLE | WS_VSCROLL | ES_MULTILINE,
                             10, 10, 460, 140, g_hwnd, nullptr, wc.hInstance, nullptr);
    if (!g_edit) return false;
    ShowWindow(g_hwnd, SW_SHOW);
    SetForegroundWindow(g_hwnd);
    SetFocus(g_edit);
    pump(500);
    return true;
}

static ITfThreadMgr* g_threadMgr = nullptr;
static TfClientId g_clientId = 0;

// 把 inklink 激活到本线程（必须先初始化 TSF 线程管理器，否则激活是空操作）
static bool activate_ime() {
    HRESULT hr = CoCreateInstance(CLSID_TF_ThreadMgr, nullptr, CLSCTX_INPROC_SERVER,
                                  IID_ITfThreadMgr, (void**)&g_threadMgr);
    if (FAILED(hr) || !g_threadMgr) {
        printf("  !! CoCreateInstance(ThreadMgr) 失败 0x%08lX\n", hr);
        return false;
    }
    hr = g_threadMgr->Activate(&g_clientId);
    if (FAILED(hr)) {
        printf("  !! ThreadMgr::Activate 失败 0x%08lX\n", hr);
        return false;
    }
    printf("  TSF thread mgr 已激活 clientId=%u\n", g_clientId);

    ITfInputProcessorProfiles* profiles = nullptr;
    hr = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr,
                                  CLSCTX_INPROC_SERVER, IID_ITfInputProcessorProfiles,
                                  (void**)&profiles);
    if (FAILED(hr) || !profiles) {
        printf("  !! CoCreateInstance(InputProcessorProfiles) 失败 0x%08lX\n", hr);
        return false;
    }
    hr = profiles->ActivateLanguageProfile(CLSID_INKLINK, 0x0804, GUID_INKLINK_PROFILE);
    profiles->Release();
    pump(700);
    if (FAILED(hr)) {
        printf("  !! ActivateLanguageProfile 失败 0x%08lX\n", hr);
        return false;
    }
    return true;
}

int main() {
    SetConsoleOutputCP(65001);
    CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);

    if (!create_window()) {
        printf("创建窗口失败: %lu\n", GetLastError());
        return 1;
    }
    printf("== inklink 自动化测试 ==\n");
    if (!activate_ime()) {
        printf("激活输入法失败——确认已安装且 DLL 已注册\n");
        return 2;
    }
    printf("输入法已激活\n\n");

    // 测试前强制关掉 CapsLock（否则字母会全部透传，测试全乱）
    if ((GetKeyState(VK_CAPITAL) & 1) != 0) {
        INPUT in[2] = {};
        in[0].type = INPUT_KEYBOARD; in[0].ki.wVk = VK_CAPITAL;
        in[1].type = INPUT_KEYBOARD; in[1].ki.wVk = VK_CAPITAL; in[1].ki.dwFlags = KEYEVENTF_KEYUP;
        SendInput(2, in, sizeof(INPUT));
        pump(500);
    }
    printf("CapsLock = %d\n", (int)(GetKeyState(VK_CAPITAL) & 1));
    printf("前台窗口 = %p (本窗口 %p) 焦点 = %p (edit %p)\n\n",
           GetForegroundWindow(), g_hwnd, GetFocus(), g_edit);
    if (GetForegroundWindow() != g_hwnd) {
        printf("  !! 前台窗口不是本窗口，按键会打偏，测试中止\n");
        return 3;
    }

    // 1. 基本拼音
    edit_clear();
    type_ascii("xiangmu");
    key(VK_SPACE, false);
    pump(700);
    expect("拼音 xiangmu + 空格 -> 项目", edit_text(), L"项目");

    // 2. 另一个词
    edit_clear();
    type_ascii("nihao");
    key(VK_SPACE, false);
    pump(700);
    expect("拼音 nihao + 空格 -> 你好", edit_text(), L"你好");

    // 3. 顿号（\）
    edit_clear();
    key(VK_OEM_5, false);   // 反斜杠
    pump(500);
    expect("反斜杠 -> 顿号", edit_text(), L"、");

    // 4. 逗号 -> 中文逗号
    edit_clear();
    key(VK_OEM_COMMA, false);
    pump(500);
    expect("逗号 -> 中文逗号", edit_text(), L"，");

    // 5. 问号：Shift+/ 应该出「？」，且不切英文
    edit_clear();
    key(VK_OEM_2, true);    // Shift+/
    pump(500);
    expect("Shift+/ -> ？", edit_text(), L"？");

    // 6. 上一步之后应该还在中文模式：打 nihao 仍出中文
    edit_clear();
    type_ascii("nihao");
    key(VK_SPACE, false);
    pump(700);
    expect("打 ? 之后仍是中文模式", edit_text(), L"你好");

    // 7. Shift 单击 -> 英文模式
    edit_clear();
    key_down(VK_SHIFT);
    pump(60);
    key_up(VK_SHIFT);
    pump(500);
    type_ascii("nihao");
    pump(700);
    expect("Shift 单击后打英文", edit_text(), L"nihao");

    // 8. 再单击 Shift -> 回中文
    key_down(VK_SHIFT);
    pump(60);
    key_up(VK_SHIFT);
    pump(500);
    edit_clear();
    type_ascii("nihao");
    key(VK_SPACE, false);
    pump(700);
    expect("再 Shift 单击回到中文", edit_text(), L"你好");

    // 9. CapsLock 开 -> 大写
    edit_clear();
    // 切 CapsLock（模拟按下抬起，系统会切换开关）
    {
        INPUT in[2] = {};
        in[0].type = INPUT_KEYBOARD; in[0].ki.wVk = VK_CAPITAL;
        in[1].type = INPUT_KEYBOARD; in[1].ki.wVk = VK_CAPITAL; in[1].ki.dwFlags = KEYEVENTF_KEYUP;
        SendInput(2, in, sizeof(INPUT));
        pump(200);
    }
    bool capsNow = (GetKeyState(VK_CAPITAL) & 1) != 0;
    type_ascii("nihao");
    pump(700);
    {
        std::wstring want = capsNow ? L"NIHAO" : L"nihao";
        expect(capsNow ? "CapsLock 开 -> NIHAO" : "CapsLock 关 -> nihao", edit_text(), want);
    }
    // 恢复 CapsLock
    {
        INPUT in[2] = {};
        in[0].type = INPUT_KEYBOARD; in[0].ki.wVk = VK_CAPITAL;
        in[1].type = INPUT_KEYBOARD; in[1].ki.wVk = VK_CAPITAL; in[1].ki.dwFlags = KEYEVENTF_KEYUP;
        SendInput(2, in, sizeof(INPUT));
        pump(200);
    }

    // 10. 回车：拼音原样上屏
    edit_clear();
    type_ascii("ceshi");
    key(VK_RETURN, false);
    pump(700);
    expect("回车 -> 拼音原样上屏", edit_text(), L"ceshi");

    printf("\n结果: %d 通过, %d 失败\n", g_pass, g_fail);
    DestroyWindow(g_hwnd);
    CoUninitialize();
    return g_fail == 0 ? 0 : 1;
}
