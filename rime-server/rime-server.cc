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
#include <array>
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
#include "pinyin-syllables.h"

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

// ───────── 自建用户记忆层 ─────────
// librime 这版 RimeSelectCandidate 不产生 commit，用户词库学不到东西。
// 自己记：code -> (词 -> 次数)，下次同一拼音优先出你选过的。
//
// 存哪儿：`%LOCALAPPDATA%\inklink\user-words.tsv`。
// 不能放 `C:\inklink-engine\`（安装目录，普通用户没有写权限），也不该放 rime 的
// user_dir（那里是 rime 自己 deploy 的地盘，混进去容易被误删）。
static std::map<std::string, std::map<std::string, int>> g_user;
static std::string g_user_path;
static bool g_user_dirty = false;
/// 每个连接一个线程，学习与查询可能并发 → 用户词库要自己上锁。
static std::mutex g_user_mutex;

/// 单条记录最多记到多少次（再往上没有排序意义，防止数值膨胀）。
static const int USER_COUNT_CAP = 999;
/// 最多记多少个"拼音串"（每个串下词数不限）。超了就丢最弱的。
static const size_t USER_MAX_CODES = 4000;

/// UTF-8 字符数（汉字算 1 个）。候选词长度要用它判断"选了几个字"。
static size_t utf8_char_count(const std::string& s) {
    size_t n = 0;
    for (size_t i = 0; i < s.size();) {
        unsigned char c = (unsigned char)s[i];
        if (c < 0x80) i += 1;
        else if ((c >> 5) == 0x6) i += 2;
        else if ((c >> 4) == 0xE) i += 3;
        else if ((c >> 3) == 0x1E) i += 4;
        else i += 1;
        n++;
    }
    return n;
}

/// 去掉分词符（`xiang-mu` → `xiangmu`）。
static std::string strip_separators(const std::string& s) {
    std::string out;
    out.reserve(s.size());
    for (char c : s) {
        if (c == '-' || c == '_') continue;
        out.push_back(c);
    }
    return out;
}


static void load_user() {
    FILE* f = fopen(g_user_path.c_str(), "rb");
    if (!f) {
        logf("user dict: %s not found (first run)", g_user_path.c_str());
        return;
    }
    char line[1024];
    size_t total = 0;
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
        if (!code.empty() && !text.empty() && count > 0) {
            g_user[code][text] = count;
            total++;
        }
    }
    fclose(f);
    logf("user dict: loaded %zu entries from %s", total, g_user_path.c_str());
}

/// 落盘。先写 `.tmp` 再改名 —— 半截写坏的文件比没有更糟（会把整本词库读成空）。
static void save_user() {
    if (!g_user_dirty) return;
    std::string tmp = g_user_path + ".tmp";
    FILE* f = fopen(tmp.c_str(), "wb");
    if (!f) {
        logf("user dict: cannot write %s", tmp.c_str());
        return;
    }
    size_t total = 0;
    for (const auto& kv : g_user) {
        for (const auto& t : kv.second) {
            fprintf(f, "%s\t%s\t%d\n", kv.first.c_str(), t.first.c_str(), t.second);
            total++;
        }
    }
    fclose(f);
    if (!MoveFileExA(tmp.c_str(), g_user_path.c_str(), MOVEFILE_REPLACE_EXISTING)) {
        logf("user dict: rename failed %lu", GetLastError());
        return;
    }
    g_user_dirty = false;
    logf("user dict: saved %zu entries to %s", total, g_user_path.c_str());
}

/// 剪枝：串数超过上限时，丢掉"最高次数只有 1"的那些串（用过一次就再没选过的）。
/// 都在 1 次以上的话就丢串本身最小的，保证文件不会无限长。
static void prune_user() {
    while (g_user.size() > USER_MAX_CODES) {
        bool dropped = false;
        for (auto it = g_user.begin(); it != g_user.end();) {
            int best = 0;
            // 注意：windows.h 有 max 宏，`std::max(` 会被宏展开成语法错误，
            // 三元写最稳（本文件其它地方用 `std::min<size_t>(...)`，带模板参数所以没事）。
            for (const auto& t : it->second) best = best > t.second ? best : t.second;
            if (best <= 1) {
                it = g_user.erase(it);
                dropped = true;
                break;
            }
            ++it;
        }
        if (!dropped) g_user.erase(g_user.begin());
        g_user_dirty = true;
    }
}


/// 单个音节是否合法。
static bool is_syllable(const std::string& s) {
    for (int i = 0; i < PINYIN_SYLLABLE_COUNT; i++) {
        if (s == PINYIN_SYLLABLES[i]) return true;
    }
    return false;
}

/// 是不是某个合法音节的**前缀**（1~6 个字母）。
///
/// 用于识别"打到一半"的拼音：`zheg` 的最后那个 `g` 是 `ge` 的前缀，
/// `zhge` 开头的 `zh` 是 `zha/zhe/zhi…` 的前缀。
static bool is_syllable_prefix(const std::string& s) {
    if (s.empty() || s.size() > 6) return false;
    // 音节表是排序的：二分找到第一个 >= s 的，看它是否以 s 开头
    int lo = 0, hi = PINYIN_SYLLABLE_COUNT;
    while (lo < hi) {
        int mid = (lo + hi) / 2;
        if (std::strcmp(PINYIN_SYLLABLES[mid], s.c_str()) < 0) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if (lo >= PINYIN_SYLLABLE_COUNT) return false;
    return std::strncmp(PINYIN_SYLLABLES[lo], s.c_str(), s.size()) == 0;
}

/// 整串能否完整切成合法拼音音节（动态规划）。
/// 用来区分「xiang 是拼音」和「github 不是拼音」，比"看候选是不是单字"可靠得多。
static bool is_pinyin_code(const std::string& code) {
    if (code.empty()) return false;
    size_t n = code.size();
    std::vector<char> dp(n + 1, 0);
    dp[0] = 1;
    for (size_t i = 1; i <= n; i++) {
        size_t lo = (i > 6) ? i - 6 : 0; // 最长音节 6 个字母（zhuang）
        for (size_t j = lo; j < i; j++) {
            if (dp[j] && is_syllable(code.substr(j, i - j))) {
                dp[i] = 1;
                break;
            }
        }
    }
    if (dp[n]) return true;
    // 简拼：全部是声母字母（无元音）也算拼音输入，如 zgrm / pz
    bool all_consonant = true;
    for (char c : code) {
        if (c < 'a' || c > 'z' || c == 'a' || c == 'e' || c == 'i' || c == 'o' || c == 'u' || c == 'v') {
            all_consonant = false;
            break;
        }
    }
    if (all_consonant) return true;
    // 「差一点点」的拼音：允许**一个**音节只打了一半，其余音节必须完整。
    // 这样 zheg（zhe + g）、zhge（zh + ge）、xiangm（xiang + m）都算拼音，
    // 而 hello / test / world / code 这类英文词仍然不算。
    if (n < 3) return false;
    // dp2[i][inc]：前缀 code[0..i) 切完后，有 inc 段是"半个音节"，
    // 且至少有一段是完整音节（has = 1）
    std::vector<std::vector<std::array<char, 2>>> dp2(
        n + 1, std::vector<std::array<char, 2>>(2, std::array<char, 2>{0, 0}));
    dp2[0][0][0] = 1;
    for (size_t i = 1; i <= n; i++) {
        size_t lo = (i > 6) ? i - 6 : 0;
        for (size_t j = lo; j < i; j++) {
            std::string chunk = code.substr(j, i - j);
            bool whole = is_syllable(chunk);
            if (!whole && !is_syllable_prefix(chunk)) continue;
            for (int inc = 0; inc < 2; inc++) {
                for (int has = 0; has < 2; has++) {
                    if (!dp2[j][inc][has]) continue;
                    int ninc = whole ? inc : inc + 1;
                    if (ninc > 1) continue;
                    int nhas = whole ? 1 : has;
                    dp2[i][ninc][nhas] = 1;
                }
            }
        }
    }
    return dp2[n][0][1] || dp2[n][1][1];
}

/// 整串能否切成**全部完整**的合法音节（不允许半个音节）。
///
/// 正确拼音（`nihao` / `pikeqiu`）为真；多打、打错、换位一个字母
/// （`zhemge` / `niihao` / `jintain`）为假 —— 这就是"要不要纠错"的判据。
static bool is_full_pinyin(const std::string& code) {
    if (code.empty()) return false;
    size_t n = code.size();
    std::vector<char> dp(n + 1, 0);
    dp[0] = 1;
    for (size_t i = 1; i <= n; i++) {
        size_t lo = (i > 6) ? i - 6 : 0;
        for (size_t j = lo; j < i; j++) {
            if (dp[j] && is_syllable(code.substr(j, i - j))) {
                dp[i] = 1;
                break;
            }
        }
    }
    return dp[n] != 0;
}

/// 末尾是不是"打了一半的音节"（`zheg` 的 `g`、`xiangm` 的 `m`）。
///
/// 这类输入 librime 的前缀匹配本来就处理得很好（`zheg` → 这个），
/// 再去纠错反而会把「项目」换成「向」。所以要区分开。
static bool ends_with_half_syllable(const std::string& code) {
    size_t n = code.size();
    size_t lo = (n > 6) ? n - 6 : 0;
    for (size_t j = lo; j < n; j++) {
        if (j > 0 && !is_full_pinyin(code.substr(0, j))) continue;
        std::string tail = code.substr(j);
        if (!is_syllable(tail) && is_syllable_prefix(tail)) return true;
    }
    return false;
}

/// 纯 ASCII（英文原文或拉丁字母候选）。
static bool ascii_only(const std::string& s) {
    for (unsigned char c : s) {
        if (c >= 0x80) return false;
    }
    return true;
}

/// 给一个"能完整切成音节"的串打分，越小越像人真正想打的东西。
///
/// 两个维度：① 需要几个单字母音节（`o`/`e`/`a` 单独成音节很少是本意，
/// `nioha` = ni+o+ha 就是这种垃圾切分）；② 音节个数（越少越紧）。
/// 用它给变体排序，`nihoa` 的两个合法变体里 `nihao`（ni+hao）就排在
/// `nioha`（ni+o+ha）前面。
static long variant_score(const std::string& s) {
    size_t n = s.size();
    if (n == 0) return 1000000;
    const long INF = 1000000;
    std::vector<long> single(n + 1, INF), count(n + 1, INF);
    single[0] = 0;
    count[0] = 0;
    for (size_t i = 1; i <= n; i++) {
        for (size_t len = 1; len <= 6 && len <= i; len++) {
            std::string part = s.substr(i - len, len);
            if (!is_syllable(part)) continue;
            long prev_single = single[i - len];
            long prev_count = count[i - len];
            if (prev_single >= INF) continue;
            long cur_single = prev_single + (len == 1 ? 1 : 0);
            long cur_count = prev_count + 1;
            if (cur_single < single[i] || (cur_single == single[i] && cur_count < count[i])) {
                single[i] = cur_single;
                count[i] = cur_count;
            }
        }
    }
    if (single[n] >= INF) return INF;
    return single[n] * 100 + count[n];
}

/// 生成"改一个字母就能变成合法拼音"的候选，按可能的先后排序：
///   ① 删掉相邻重复字母（`niihao` → `nihao`、`shanghhai` → `shanghai`）
///   ② 相邻两字母换位（`jintain` → `jintian`）
///   ③ 删掉任意一个字母（`zhemge` → `zhege`）
///
/// 只有**能完整切成音节**的变体才留下，所以 `github`/`vscode` 这类英文词
/// 一个变体都产不出来（后面的 rime 查询一次都不会发生）。
/// 最后按 `variant_score` 稳定排序：同一族的变体里，切分更"像人打的"排前面。
static std::vector<std::string> edit_variants(const std::string& code) {
    std::vector<std::string> v;
    auto add = [&](const std::string& s) {
        if (s.empty() || s == code) return;
        if (std::find(v.begin(), v.end(), s) != v.end()) return;
        if (!is_full_pinyin(s)) return;
        v.push_back(s);
    };
    for (size_t i = 1; i < code.size(); i++) {
        if (code[i] == code[i - 1]) add(code.substr(0, i) + code.substr(i + 1));
    }
    for (size_t i = 0; i + 1 < code.size(); i++) {
        std::string s = code;
        std::swap(s[i], s[i + 1]);
        add(s);
    }
    for (size_t i = 0; i < code.size(); i++) {
        add(code.substr(0, i) + code.substr(i + 1));
    }
    std::stable_sort(v.begin(), v.end(), [](const std::string& a, const std::string& b) {
        return variant_score(a) < variant_score(b);
    });
    return v;
}

/// 只查第一页、最多要 need 个候选（给纠错探针用，比主查询便宜）。
static std::vector<std::string> collect_page0(const std::string& code, size_t need) {
    std::vector<std::string> out;
    g_clear(g_session);
    if (!g_simulate(g_session, code.c_str())) {
        g_clear(g_session);
        return out;
    }
    RimeContext ctx;
    std::memset(&ctx, 0, sizeof(ctx));
    ctx.data_size = sizeof(RimeContext);
    if (g_get_context(g_session, &ctx)) {
        int n = ctx.menu.num_candidates;
        for (int i = 0; i < n && out.size() < need; i++) {
            if (ctx.menu.candidates[i].text && ctx.menu.candidates[i].text[0]) {
                out.push_back(ctx.menu.candidates[i].text);
            }
        }
        g_free_context(&ctx);
    }
    return out;
}

/// 把用户选过的词提到前面（同一拼音内按次数降序）。
static void apply_user_boost(const std::string& code, std::vector<std::string>& cands) {
    std::lock_guard<std::mutex> lock(g_user_mutex);
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

// ───────────────────────── 用户词库：学习 ─────────────────────────
/// 贪心最长匹配把拼音切成音节（和 DLL 里 `pinyin.rs` 的 `split_lengths` 同一套规则）。
/// 切不动就停，返回已切出的部分。
static std::vector<size_t> split_lengths(const std::string& code) {
    std::vector<size_t> lens;
    size_t i = 0;
    while (i < code.size()) {
        size_t take = 0;
        for (size_t len = std::min<size_t>(6, code.size() - i); len >= 1; len--) {
            if (is_syllable(code.substr(i, len))) {
                take = len;
                break;
            }
        }
        if (take == 0) break;
        lens.push_back(take);
        i += take;
    }
    return lens;
}

/// 算出"这次选择该记在哪个拼音串下"。
///
/// 分段上屏时用户只选了一个字/一个词（`pikeqiu` 里选「皮」），此时学到
/// `pikeqiu → 皮` 是错的 —— 下次打 pikeqiu 首选的应该是「皮克球」。
/// 所以按**选中的字数**取拼音串的前 N 个音节：
///   pikeqiu + 皮   (1 字) → 学 `pi`    → 皮
///   pikeqiu + 皮克 (2 字) → 学 `pike`  → 皮克
///   xiangmu + 项目 (2 字) → 学 `xiangmu` → 项目
/// 字数 ≥ 音节数时按整串记（多打的尾巴字母一起吃掉，反正下次是同一个串）。
static std::string learn_key(const std::string& raw_code, const std::string& word) {
    std::string code = strip_separators(raw_code);
    if (code.empty() || word.empty()) return std::string();
    size_t chars = utf8_char_count(word);
    if (chars == 0) return std::string();
    std::vector<size_t> lens = split_lengths(code);
    if (lens.empty()) return code;              // 不像拼音（纯英文），整串学
    if (chars >= lens.size()) return code;      // 整串都吃掉了
    size_t used = 0;
    for (size_t i = 0; i < chars && i < lens.size(); i++) used += lens[i];
    if (used == 0 || used > code.size()) return code;
    return code.substr(0, used);
}

/// 记一次"用户在这个拼音下选了这个词"。
static void learn(const std::string& raw_code, const std::string& word) {
    std::string key = learn_key(raw_code, word);
    if (key.empty() || word.empty()) return;
    // 纯英文上屏（码本身就是词）不学：那是"打英文"，不是选中文候选。
    if (word == strip_separators(raw_code)) return;
    std::lock_guard<std::mutex> lock(g_user_mutex);
    int& c = g_user[key][word];
    if (c < USER_COUNT_CAP) c++;
    g_user_dirty = true;
    prune_user();
    logf("learn key=%s word=%s count=%d", key.c_str(), word.c_str(), c);
    save_user();
}

// ───────────────────────── 路径 ─────────────────────────
static std::string exe_dir() {
    char buf[MAX_PATH];
    DWORD n = GetModuleFileNameA(nullptr, buf, MAX_PATH);
    std::string p(buf, n);
    size_t pos = p.find_last_of("\\/");
    return pos == std::string::npos ? "." : p.substr(0, pos);
}

/// 用户词库路径：`%LOCALAPPDATA%\inklink\user-words.tsv`
/// 拿不到 LOCALAPPDATA 就退回安装目录（少数精简系统）。
static std::string user_dict_path() {
    char buf[MAX_PATH];
    DWORD n = GetEnvironmentVariableA("LOCALAPPDATA", buf, MAX_PATH);
    if (n == 0 || n >= MAX_PATH) {
        return exe_dir() + "\\user-words.tsv";
    }
    std::string dir = std::string(buf, n) + "\\inklink";
    CreateDirectoryA(dir.c_str(), nullptr);   // 已存在会失败，无所谓
    return dir + "\\user-words.tsv";
}


// ───────────────────────── 查询 ─────────────────────────
static const int MAX_CANDS = 50;

static std::vector<std::string> query(const std::string& code) {
    std::vector<std::string> out;
    if (code.empty()) return out;

    bool looks_pinyin = is_pinyin_code(code);

    // ── 纠错探针 ──
    // 三种情况分开处理：
    //  - 正常拼音（`xiangmu`）：整串切得开，一个探针都不发，不占同步路径预算；
    //  - 末尾打了一半（`zheg`/`xiangm`）：librime 的前缀匹配本就处理得好，
    //    插手反而会把「项目」换成「向」；
    //  - 中间多/错/换位一个字母（`zhemge`/`niihao`/`jintain`）：rime 只能拿多余
    //    字母硬切出一串怪词，这时候纠错结果才更可信。
    std::vector<std::string> corrected;
    if (code.size() >= 3 && !is_full_pinyin(code) && !ends_with_half_syllable(code)) {
        std::vector<std::string> vars = edit_variants(code);
        if (vars.size() > 8) vars.resize(8);
        for (const std::string& v : vars) {
            for (const std::string& w : collect_page0(v, 6)) {
                if (ascii_only(w)) continue;
                if (std::find(corrected.begin(), corrected.end(), w) != corrected.end()) continue;
                corrected.push_back(w);
                if (corrected.size() >= 8) break;
            }
            if (corrected.size() >= 8) break;
        }
        if (!corrected.empty()) logf("correct %s -> %zu words", code.c_str(), corrected.size());
    }

    g_clear(g_session);
    if (!g_simulate(g_session, code.c_str())) {
        g_clear(g_session);
        if (corrected.empty()) return out;
        return corrected;   // 连 rime 都喂不进去，直接给纠错结果
    }

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

    // 合并纠错结果：探针只在"原串根本切不干净"时才跑，此时 rime 那串候选是拿多余
    // 字母硬切出来的（`zhemge` → 这么个 / 这么高 / 这么干…），纠错结果更可信，放最前。
    if (!corrected.empty()) {
        std::vector<std::string> merged = corrected;
        for (const std::string& w : out) {
            if (std::find(merged.begin(), merged.end(), w) == merged.end()) merged.push_back(w);
        }
        out.swap(merged);
    }

    // 英文候选：把原始输入作为一个候选追加（github、vscode 这类英文词）。
    // librime 只认拼音，纯英文会被切成 给/过/跟，用户就没法直接上屏英文了。
    if (code.size() >= 2) {
        bool is_pure_alpha = true;
        for (char c : code) {
            if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'))) { is_pure_alpha = false; break; }
        }
        if (is_pure_alpha && out.size() < (size_t)MAX_CANDS) {
            auto it = std::find(out.begin(), out.end(), code);
            if (it == out.end()) {
                // 不像拼音时英文原样要留在第一屏：没有纠错结果（github/vscode）排最前；
                // 有纠错结果说明可能是打错的拼音（niihao → 你好），让纠错占第一，
                // 原样紧跟其后（`update` 这种"恰好差一个字母就是合法拼音"的英文词
                // 因此仍然一眼能看到）。像拼音的（`code`）原样排最后，一直如此。
                if (!looks_pinyin) {
                    size_t pos = corrected.empty() ? 0 : 1;
                    if (pos > out.size()) pos = out.size();
                    out.insert(out.begin() + pos, code);
                } else {
                    out.push_back(code);
                }
            }
        }
    }
    // 用户词库：这个拼音下用户选过的词提前（次数多的更前）。
    // 放在纠错/英文兜底之后、返回之前 —— 用户的手动选择优先级最高。
    apply_user_boost(strip_separators(code), out);

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
///
/// DLL 会带上 `code`（用户当时打的拼音串）和 `word`（选中的词）：
///   引擎不用跨连接记住"上一次查询是什么"（多应用并发时会串），
///   也不用猜用户到底选了第几个（下标在应用之间没有意义）。
static bool select_candidate(uint32_t index, const std::string& code, const std::string& word) {
    logf("select %u code=%s word=%s", index, code.c_str(), word.c_str());
    if (word.empty()) return true;
    learn(code, word);
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
        // 选词（协议 v3）：[u32 index][u32 code_len][code][u16 word_len][word]
        // v2 只有 index；旧 DLL + 新引擎时后面的字段读不到，就当"没学到东西"，
        // 不能因此断开连接（否则老版本会表现为选词卡住）。
        uint32_t index = 0;
        if (!read_exact(p, &index, 4)) return;
        uint32_t clen = 0;
        if (!read_exact(p, &clen, 4)) {
            logf("select %u (no code: old protocol)", index);
            write_response(p, {});
            return;
        }
        if (clen > 4096) {
            write_response(p, {});
            return;
        }
        std::string code(clen, '\0');
        if (clen > 0 && !read_exact(p, &code[0], clen)) return;
        uint16_t wlen = 0;
        if (!read_exact(p, &wlen, 2)) {
            write_response(p, {});
            return;
        }
        std::string word(wlen, '\0');
        if (wlen > 0 && !read_exact(p, &word[0], wlen)) return;
        select_candidate(index, code, word);
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
// ───────────────────────── 自检 ─────────────────────────
// 引擎是 C++、没有测试框架，但纯函数（音节切分、学习键推导）值得单测。
// 跑 `rime-server.exe --selftest`，不初始化 librime、不碰管道，只验算逻辑。
static int run_selftest() {
    int fails = 0;
    auto check = [&](bool cond, const char* what) {
        printf("%s %s\n", cond ? "ok  " : "FAIL", what);
        if (!cond) fails++;
    };
    int total = 0;
    auto ck = [&](bool cond, const char* what) {
        total++;
        check(cond, what);
    };

    ck(utf8_char_count("项目") == 2, "utf8_char_count: 2 hanzi");
    ck(utf8_char_count("皮") == 1, "utf8_char_count: single hanzi");
    ck(utf8_char_count("github") == 6, "utf8_char_count: ascii per char");
    ck(strip_separators("xiang-mu") == "xiangmu", "strip_separators: drop dash");
    ck(strip_separators("xiang_mu") == "xiangmu", "strip_separators: drop underscore");

    std::vector<size_t> l1 = split_lengths("pikeqiu");
    ck(l1.size() == 3 && l1[0] == 2 && l1[1] == 2 && l1[2] == 3,
       "split_lengths: pikeqiu -> pi|ke|qiu");
    std::vector<size_t> l2 = split_lengths("xiangmu");
    ck(l2.size() == 2 && l2[0] == 5 && l2[1] == 2, "split_lengths: xiangmu -> xiang|mu");
    ck(split_lengths("xian").size() == 1, "split_lengths: xian is one syllable");
    ck(split_lengths("github").empty(), "split_lengths: github unsplittable");

    ck(learn_key("pikeqiu", "皮") == "pi", "learn_key: 1 char -> first 1 syllable");
    ck(learn_key("pikeqiu", "皮克") == "pike", "learn_key: 2 chars -> first 2 syllables");
    ck(learn_key("xiangmu", "项目") == "xiangmu", "learn_key: whole code");
    ck(learn_key("xiang-mu", "项目") == "xiangmu", "learn_key: separators stripped");
    ck(learn_key("zhemge", "这个") == "zhemge", "learn_key: mistyped code kept as-is");
    ck(learn_key("x", "西") == "x", "learn_key: single letter");

    if (fails == 0) {
        printf("\nall %d checks passed\n", total);
    } else {
        printf("\n%d of %d checks FAILED\n", fails, total);
    }
    return fails == 0 ? 0 : 1;
}

int main(int argc, char** argv) {
    for (int i = 1; i < argc; i++) {
        if (std::strcmp(argv[i], "--selftest") == 0) {
            return run_selftest();
        }
    }

    std::string dir = exe_dir();
    std::string log_path = dir + "\\rime-server.log";
    g_log = fopen(log_path.c_str(), "a");

    // 用户词库（个人词库）：启动就加载，第一次查询就能用上历史选择
    g_user_path = user_dict_path();
    load_user();


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
