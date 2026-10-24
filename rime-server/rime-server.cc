// rime-server.exe —— 用 librime 做引擎，通过命名管道给 TSF DLL 供候选。
//
// 架构（和 Weasel 一样）：x86 进程承载 librime，x64 的 TSF DLL 通过管道查询，
// 这样不用解决 x64 与 x86 的 ABI 问题。
//
// 协议（与 Rust 版引擎完全一致）：
//   请求: [u32 len LE][UTF-8 拼音]
//   响应: [u32 count LE] + count * ([u16 len LE][UTF-8 候选词])
//
// 编译（x86）：
//   cl /nologo /utf-8 /EHsc /std:c++17 /O2 /MD rime-server.cc advapi32.lib /Fe:rime-server.exe

#include <windows.h>
#include <sddl.h>
#include <algorithm>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <string>
#include <vector>
#include <mutex>
#include <thread>
#include "rime_api.h"

// ───────────────────────── 日志 ─────────────────────────
static FILE* g_log = nullptr;

static std::mutex g_log_mutex;

static void logf(const char* fmt, ...) {
    if (!g_log) return;
    std::lock_guard<std::mutex> lock(g_log_mutex);
    va_list ap;
    va_start(ap, fmt);
    vfprintf(g_log, fmt, ap);
    va_end(ap);
    fputc('\n', g_log);
    fflush(g_log);
}

// ───────────────────────── rime 函数指针 ─────────────────────────
static HMODULE g_dll;

template <typename T>
static T sym(const char* name) {
    T fn = reinterpret_cast<T>(GetProcAddress(g_dll, name));
    if (!fn) {
        logf("missing export: %s", name);
        ExitProcess(2);
    }
    return fn;
}

typedef void (*FnSetup)(RimeTraits*);
typedef void (*FnInitialize)(RimeTraits*);
typedef Bool (*FnStartMaintenance)(Bool);
typedef void (*FnJoinMaintenanceThread)();
typedef void (*FnFinalize)();
typedef RimeSessionId (*FnCreateSession)();
typedef Bool (*FnSelectSchema)(RimeSessionId, const char*);
typedef Bool (*FnSimulateKeySequence)(RimeSessionId, const char*);
typedef Bool (*FnClearComposition)(RimeSessionId);
typedef Bool (*FnGetContext)(RimeSessionId, RimeContext*);
typedef void (*FnFreeContext)(RimeContext*);
typedef void (*FnCleanupAllSessions)();
typedef Bool (*FnSelectCandidate)(RimeSessionId, size_t);
typedef Bool (*FnSelectOnPage)(RimeSessionId, size_t);
typedef Bool (*FnGetCommit)(RimeSessionId, RimeCommit*);
typedef void (*FnFreeCommit)(RimeCommit*);

static FnClearComposition g_clear;
static FnSimulateKeySequence g_simulate;
static FnGetContext g_get_context;
static FnFreeContext g_free_context;
static FnSelectCandidate g_select_candidate;
static FnSelectOnPage g_select_on_page;
static FnGetCommit g_get_commit;
static FnFreeCommit g_free_commit;

static RimeSessionId g_session = 0;
static std::mutex g_rime_mutex;
static std::string g_last_code;
static std::vector<std::string> g_last_cands;

// ───────── 自建用户记忆层 ─────────
// librime 这版 RimeSelectCandidate 不产生 commit，用户词库学不到东西。
// 自己记：code -> (词 -> 次数)，下次同一拼音优先出你选过的。
static std::map<std::string, std::map<std::string, int>> g_user;
static std::string g_user_path;

static void load_user() {
    FILE* f = fopen(g_user_path.c_str(), "rb");
    if (!f) return;
    char line[1024];
    while (fgets(line, sizeof(line), f)) {
        std::string s(line);
        while (!s.empty() && (s.back() == '\n' || s.back() == '\r')) s.pop_back();
        size_t a = s.find('\t');
        if (a == std::string::npos) continue;
        size_t b = s.find('\t', a + 1);
        if (b == std::string::npos) continue;
        std::string code = s.substr(0, a);
        std::string text = s.substr(a + 1, b - a - 1);
        int count = atoi(s.c_str() + b + 1);
        if (!code.empty() && !text.empty() && count > 0) g_user[code][text] = count;
    }
    fclose(f);
}

static void save_user() {
    FILE* f = fopen(g_user_path.c_str(), "wb");
    if (!f) return;
    for (const auto& kv : g_user) {
        for (const auto& t : kv.second) {
            fprintf(f, "%s\t%s\t%d\n", kv.first.c_str(), t.first.c_str(), t.second);
        }
    }
    fclose(f);
}

/// 把用户选过的词提到前面（同一拼音内按次数降序）。
static void apply_user_boost(const std::string& code, std::vector<std::string>& cands) {
    auto it = g_user.find(code);
    if (it == g_user.end() || it->second.empty()) return;
    std::stable_sort(cands.begin(), cands.end(), [&](const std::string& a, const std::string& b) {
        int ca = 0, cb = 0;
        auto fa = it->second.find(a);
        if (fa != it->second.end()) ca = fa->second;
        auto fb = it->second.find(b);
        if (fb != it->second.end()) cb = fb->second;
        return ca > cb;
    });
}

// ───────────────────────── 路径 ─────────────────────────
static std::string exe_dir() {
    char buf[MAX_PATH];
    DWORD n = GetModuleFileNameA(nullptr, buf, MAX_PATH);
    std::string p(buf, n);
    size_t pos = p.find_last_of("\\/");
    return pos == std::string::npos ? "." : p.substr(0, pos);
}

// ───────────────────────── 查询 ─────────────────────────
static const int MAX_CANDS = 50;

static std::vector<std::string> query(const std::string& code) {
    std::vector<std::string> out;
    if (code.empty()) return out;

    g_clear(g_session);
    if (!g_simulate(g_session, code.c_str())) {
        g_clear(g_session);
        return out;
    }
    g_last_code = code;

    // 收集当前页 + 往后翻几页，凑够 MAX_CANDS 个候选
    for (int page = 0; page < 6 && (int)out.size() < MAX_CANDS; page++) {
        RimeContext ctx;
        std::memset(&ctx, 0, sizeof(ctx));
        ctx.data_size = sizeof(RimeContext);
        if (!g_get_context(g_session, &ctx)) break;

        int n = ctx.menu.num_candidates;
        for (int i = 0; i < n && (int)out.size() < MAX_CANDS; i++) {
            if (ctx.menu.candidates[i].text && ctx.menu.candidates[i].text[0]) {
                out.push_back(ctx.menu.candidates[i].text);
            }
        }
        bool last = ctx.menu.is_last_page != 0;
        g_free_context(&ctx);
        if (last) break;
        if (!g_simulate(g_session, "{Page_Down}")) break;
    }

    // 翻回第一页，这样后续"选词上报"按数字键时下标才对得上。
    // 注意：这里**不能**清组合，否则紧接着的选词就没候选可选了。
    // 组合留着，下次查询开头会清
    return out;
}

// ───────────────────────── 选词上报 ─────────────────────────
// 用户选了第 index 个候选：让 librime 记住，它才会把词写进用户词库。
// RimeSelectCandidate 会顺带"提交"，我们拿到 commit 后丢弃（上屏由 DLL 自己做）。
/// 选词上报：记录"这个拼音下用户选了哪个词"，并落盘。
///
/// 不走 librime 的 RimeSelectCandidate——那个在这版库里返回成功但不产生
/// commit，用户词库学不到东西。自己记一份，下次同拼音把它提前。
static bool select_candidate(uint32_t index) {
    // TODO: 用户词库学习（librime 这版 RimeSelectCandidate 不产生 commit，
    // 自建记忆层待单独实现）。当前只记日志，不影响上屏。
    logf("select %u (learning disabled)", index);
    return true;
}

// ───────────────────────── 管道 ─────────────────────────
static bool read_exact(HANDLE p, void* buf, DWORD n) {
    DWORD got = 0;
    BYTE* p8 = static_cast<BYTE*>(buf);
    while (got < n) {
        DWORD r = 0;
        if (!ReadFile(p, p8 + got, n - got, &r, nullptr) || r == 0) return false;
        got += r;
    }
    return true;
}

static bool write_all(HANDLE p, const void* buf, DWORD n) {
    DWORD written = 0;
    const BYTE* p8 = static_cast<const BYTE*>(buf);
    while (written < n) {
        DWORD w = 0;
        if (!WriteFile(p, p8 + written, n - written, &w, nullptr) || w == 0) return false;
        written += w;
    }
    return true;
}

static void write_response(HANDLE p, const std::vector<std::string>& cands) {
    std::vector<BYTE> out;
    uint32_t count = (uint32_t)cands.size();
    out.insert(out.end(), (BYTE*)&count, (BYTE*)&count + 4);
    for (const auto& c : cands) {
        uint16_t l = (uint16_t)(c.size() > 0xFFFF ? 0xFFFF : c.size());
        out.insert(out.end(), (BYTE*)&l, (BYTE*)&l + 2);
        out.insert(out.end(), c.begin(), c.begin() + l);
    }
    write_all(p, out.data(), (DWORD)out.size());
    // 关键：必须等客户端读完再断开，否则尾部数据会被丢弃
    FlushFileBuffers(p);
}

static void serve_client(HANDLE p) {
    // 协议 v2：首字节是消息类型
    //   1 = 查询: [u32 len][UTF-8 拼音]  -> [u32 count] + 候选
    //   2 = 选词: [u32 index]            -> [u32 0]
    uint8_t type = 0;
    if (!read_exact(p, &type, 1)) return;

    if (type == 2) {
        uint32_t index = 0;
        if (!read_exact(p, &index, 4)) return;
        std::lock_guard<std::mutex> lock(g_rime_mutex);
        select_candidate(index);
        write_response(p, {});
        return;
    }

    if (type != 1) return;

    uint32_t len = 0;
    if (!read_exact(p, &len, 4)) return;
    if (len == 0 || len > 4096) return;

    std::string code(len, '\0');
    if (!read_exact(p, &code[0], len)) return;

    std::vector<std::string> cands;
    {
        std::lock_guard<std::mutex> lock(g_rime_mutex);
        cands = query(code);
    }
    logf("q=%s n=%u", code.c_str(), (unsigned)cands.size());
    for (size_t i = 0; i < cands.size() && i < 3; i++) {
        logf("  [%u] size=%u", (unsigned)i, (unsigned)cands[i].size());
    }
    if (!cands.empty()) {
        logf("q=%s n=%u first=%s len=%u", code.c_str(), (unsigned)cands.size(),
             cands[0].c_str(), (unsigned)cands[0].size());
    } else {
        logf("q=%s n=0", code.c_str());
    }

    write_response(p, cands);
}
int main() {
    std::string dir = exe_dir();
    std::string log_path = dir + "\\rime-server.log";
    g_log = fopen(log_path.c_str(), "a");


    // 单例：和 Rust 版引擎同名互斥量，避免两个引擎抢同一个管道
    HANDLE mutex = CreateMutexW(nullptr, FALSE, L"Global\\inklink-engine-singleton");
    if (mutex && GetLastError() == ERROR_ALREADY_EXISTS) {
        logf("another engine instance is running, exit");
        return 0;
    }

    std::string dll_path = dir + "\\rime.dll";
    g_dll = LoadLibraryA(dll_path.c_str());
    if (!g_dll) {
        logf("LoadLibrary %s failed: %lu", dll_path.c_str(), GetLastError());
        return 1;
    }

    auto setup = sym<FnSetup>("RimeSetup");
    auto initialize = sym<FnInitialize>("RimeInitialize");
    auto start_maintenance = sym<FnStartMaintenance>("RimeStartMaintenance");
    auto join_maintenance = sym<FnJoinMaintenanceThread>("RimeJoinMaintenanceThread");
    g_clear = sym<FnClearComposition>("RimeClearComposition");
    g_simulate = sym<FnSimulateKeySequence>("RimeSimulateKeySequence");
    g_get_context = sym<FnGetContext>("RimeGetContext");
    g_free_context = sym<FnFreeContext>("RimeFreeContext");
    g_select_candidate = sym<FnSelectCandidate>("RimeSelectCandidate");
    g_select_on_page = sym<FnSelectOnPage>("RimeSelectCandidateOnCurrentPage");
    g_get_commit = sym<FnGetCommit>("RimeGetCommit");
    g_free_commit = sym<FnFreeCommit>("RimeFreeCommit");
    auto create_session = sym<FnCreateSession>("RimeCreateSession");
    auto select_schema = sym<FnSelectSchema>("RimeSelectSchema");
    auto finalize = sym<FnFinalize>("RimeFinalize");

    std::string shared_dir = dir + "\\rime\\data";
    std::string user_dir = dir + "\\rime\\user";

    RimeTraits traits;
    std::memset(&traits, 0, sizeof(traits));
    traits.data_size = sizeof(RimeTraits);
    traits.shared_data_dir = shared_dir.c_str();
    traits.user_data_dir = user_dir.c_str();
    traits.log_dir = user_dir.c_str();
    traits.distribution_name = "inklink";
    traits.distribution_code_name = "inklink";
    traits.distribution_version = "0.2";
    traits.app_name = "rime.inklink";
    traits.min_log_level = 2;

    logf("init: shared=%s user=%s", shared_dir.c_str(), user_dir.c_str());
    setup(&traits);
    initialize(&traits);

    if (start_maintenance(True)) {
        join_maintenance();
    }
    logf("maintenance done");

    g_session = create_session();
    if (!g_session) {
        logf("create session failed");
        return 3;
    }
    select_schema(g_session, "luna_pinyin_simp");
    logf("session=%u ready", (unsigned)g_session);

    // NULL DACL：允许其它用户/进程连接（输入法会被不同权限的应用调用）
    PSECURITY_DESCRIPTOR sd = nullptr;
    ConvertStringSecurityDescriptorToSecurityDescriptorW(L"D:(A;;GA;;;WD)", SDDL_REVISION_1, &sd, nullptr);
    SECURITY_ATTRIBUTES sa;
    sa.nLength = sizeof(sa);
    sa.lpSecurityDescriptor = sd;
    sa.bInheritHandle = FALSE;

    logf("listening on \\\\.\\pipe\\inklink-engine");
    for (;;) {
        HANDLE p = CreateNamedPipeW(
            L"\\\\.\\pipe\\inklink-engine",
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &sa);
        if (p == INVALID_HANDLE_VALUE) {
            logf("CreateNamedPipe failed: %lu", GetLastError());
            Sleep(1000);
            continue;
        }
        BOOL ok = ConnectNamedPipe(p, nullptr);
        if (!ok && GetLastError() != ERROR_PIPE_CONNECTED) {
            CloseHandle(p);
            continue;
        }
        // 每连接一个线程：避免一个卡住的客户端堵死其它应用
        std::thread([p]() {
            serve_client(p);
            DisconnectNamedPipe(p);
            CloseHandle(p);
        }).detach();
    }
}
