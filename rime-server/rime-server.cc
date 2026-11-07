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
#include <winhttp.h>
#include <sddl.h>
#include <algorithm>
#include <array>
#include <cctype>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <set>
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
// 「精确方案」会话：同一个词典，但 speller/algebra 不带模糊音补丁。
// 每次查询先问它，让精确命中的候选排在模糊命中之前（否则打 ni 第一个是「里」）。
static RimeSessionId g_exact = 0;
// 用户有没有开模糊音。没开的话两个会话候选完全一样，白跑一遍没意义。
static bool g_fuzzy_on = false;
static std::mutex g_rime_mutex;

// ───────── 自建用户记忆层 ─────────
// librime 这版 RimeSelectCandidate 不产生 commit，用户词库学不到东西。
// 自己记：code -> (词 -> 次数)，下次同一拼音优先出你选过的。
//
// 存哪儿：`%LOCALAPPDATA%\inklink\user-words.tsv`。
// 不能放 `C:\inklink-engine\`（安装目录，普通用户没有写权限），也不该放 rime 的
// user_dir（那里是 rime 自己 deploy 的地盘，混进去容易被误删）。
//
// **按应用分开记账**：同一个拼音在不同应用里想打的词是不一样的
// （微信里 `hai` 常选「还行」，VSCode 里 `hai` 更可能选「函数」？其实都是词，
// 但 `xiangmu` 在聊天里是「想你」、在工位上是「项目」）。所以每个词除了
// 全局次数（`total`，= 各应用次数之和）还记一份 `app -> 次数`。
// 查询时按"当前应用"加分：当前应用里的使用次数算双倍。
struct UserWord {
    /// 全局次数（各应用之和，读盘时重算，不单独存）
    int total = 0;
    /// 应用名（exe 名小写，如 `weixin`）-> 次数；空串 = 老版本/未知应用
    std::map<std::string, int> apps;
};
static std::map<std::string, std::map<std::string, UserWord>> g_user;
static std::string g_user_path;
static bool g_user_dirty = false;
/// 每个连接一个线程，学习与查询可能并发 → 用户词库要自己上锁。
static std::mutex g_user_mutex;

/// 单条记录最多记到多少次（再往上没有排序意义，防止数值膨胀）。
static const int USER_COUNT_CAP = 999;
/// 最多记多少个"拼音串"（每个串下词数不限）。超了就丢最弱的。
static const size_t USER_MAX_CODES = 4000;
/// 少于这么多字的词不学（**长码**上不学，短码例外，见 should_learn）。
///
/// 单字本来就该由语言模型的整体概率排序，学一份"用户在这个码上选过这个字"
/// 反而会跟模型打架。但短码（`l`、`de`）不一样：码本身就是那个字，模型在几百个
/// 同首字母的字里按词频猜，用户选一百次也记不住（2026-11-06："打 l 总是出来来"）。
static const size_t USER_MIN_CHARS = 2;

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

/// 去掉分词符（`xiang-mu` → `xiangmu`）。`'` 也算分词符：查询 librime 时要还原成整串。
static std::string strip_separators(const std::string& s) {
    std::string out;
    out.reserve(s.size());
    for (char c : s) {
        if (c == '-' || c == '_' || c == '\'') continue;
        out.push_back(c);
    }
    return out;
}

/// 把用户打的分词符统一成音节边界 `'`（`xi-an` / `xi_an` → `xi'an`）。
///
/// 边界是**用户表达的意图**：`xian` 是一个音节（先），`xi'an` 是两个（西安）。
/// 所以学习要按带边界的写法记（见 learn_key），下次同样的边界才复用；
/// 而送给 librime 查询时两边都还原成 `xian`（见 query）。
static std::string normalize_boundaries(const std::string& s) {
    std::string out;
    out.reserve(s.size());
    for (char c : s) {
        if (c == '-' || c == '_' || c == ' ' || c == '\'') {
            if (!out.empty() && out.back() != '\'') out.push_back('\'');
            continue;
        }
        out.push_back(c);
    }
    while (!out.empty() && out.back() == '\'') out.pop_back();
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
        // 第四列（应用名）是 1.0.9 加的；老文件只有三列，当成"未知应用"
        size_t c = s.find('\t', b + 1);
        std::string app;
        int count = 0;
        if (c == std::string::npos) {
            count = atoi(s.c_str() + b + 1);
        } else {
            count = atoi(s.c_str() + b + 1);
            app = s.substr(c + 1);
        }
        if (!code.empty() && !text.empty() && count > 0) {
            UserWord& w = g_user[code][text];
            w.apps[app] += count;
            w.total += count;
            total++;
        }
    }
    fclose(f);
    logf("user dict: loaded %zu entries from %s", total, g_user_path.c_str());
}

/// 落盘。先写 `.tmp` 再改名 —— 半截写坏的文件比没有更糟（会把整本词库读成空）。
///
/// 文件格式：`码 \t 词 \t 次数 \t 应用名`，**每个 (码,词,应用) 一行**。
/// 全局次数不单独存，读盘时把各应用加起来 —— 少一个字段就少一处不一致。
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
            if (t.second.apps.empty()) {
                fprintf(f, "%s\t%s\t%d\t\n", kv.first.c_str(), t.first.c_str(), t.second.total);
                total++;
                continue;
            }
            for (const auto& ap : t.second.apps) {
                fprintf(f, "%s\t%s\t%d\t%s\n", kv.first.c_str(), t.first.c_str(), ap.second,
                        ap.first.c_str());
                total++;
            }
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
            for (const auto& t : it->second) best = best > t.second.total ? best : t.second.total;
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

/// 在指定会话里把候选收集到 max_cands 个（当前页 + 往后翻几页）。
///
/// 抽出来是因为现在有两个会话：带模糊音的 g_session 和不带的 g_exact，
/// 两边都要跑同一套「模拟按键 → 读 context → 翻页」流程。
static std::vector<std::string> collect_candidates(RimeSessionId session, const std::string& code,
                                                   size_t max_cands) {
    std::vector<std::string> out;
    g_clear(session);
    if (!g_simulate(session, code.c_str())) {
        g_clear(session);
        return out;
    }
    for (int page = 0; page < 6 && out.size() < max_cands; page++) {
        RimeContext ctx;
        std::memset(&ctx, 0, sizeof(ctx));
        ctx.data_size = sizeof(RimeContext);
        if (!g_get_context(session, &ctx)) break;
        int n = ctx.menu.num_candidates;
        for (int i = 0; i < n && out.size() < max_cands; i++) {
            if (ctx.menu.candidates[i].text && ctx.menu.candidates[i].text[0]) {
                out.push_back(ctx.menu.candidates[i].text);
            }
        }
        bool last = ctx.menu.is_last_page != 0;
        g_free_context(&ctx);
        if (last) break;
        if (!g_simulate(session, "{Page_Down}")) break;
    }
    g_clear(session);
    return out;
}

/// 一个词在"当前应用"里的排序分。
///
/// 规则：全局次数 + 当前应用次数 ×2 —— 当前应用里的使用次数算双倍。
/// 为什么不是"只用当前应用的次数"：新应用里你一次都没选过，等于把整本词库
/// 关掉；为什么不是"只看全局"：那就又退回到所有应用混在一起了。
/// 双倍是个能解释、能单测的经验值：同一份使用次数下，本应用里的词一定赢，
/// 但别的应用里反复用过的词仍能靠全局次数压过一个刚用一次的。
static int boost_score(const UserWord& w, const std::string& app) {
    int app_count = 0;
    if (!app.empty()) {
        auto it = w.apps.find(app);
        if (it != w.apps.end()) app_count = it->second;
    }
    return w.total + app_count * 2;
}

/// 把用户选过的词提到前面（同一拼音内按分数降序）。
///
/// `code` 是**带边界的原始码**（`xi'an`）。两种记法都算：
///   - 精确匹配（`xi'an` 下学过的）权重 ×4 —— 用户用边界明确表达的分隔意图最准；
///   - 抹掉边界（`xian` 下学过的）权重 ×1 —— 历史上按整串学的也算数。
/// 精确匹配、但 librime 这次没给出这个词的，直接补到最前面（最多 3 个）：
/// 用户明明为这个边界选过它，藏起来等于没学。
static void apply_user_boost(const std::string& code, const std::string& app,
                             std::vector<std::string>& cands) {
    std::lock_guard<std::mutex> lock(g_user_mutex);
    std::string exact = normalize_boundaries(code);
    std::string flat = strip_separators(exact);
    auto it_exact = g_user.find(exact);
    auto it_flat = (flat == exact) ? g_user.end() : g_user.find(flat);
    bool has_exact = it_exact != g_user.end() && !it_exact->second.empty();
    bool has_flat = it_flat != g_user.end() && !it_flat->second.empty();
    if (!has_exact && !has_flat) return;
    auto score = [&](const std::string& w) {
        int s = 0;
        if (has_exact) {
            auto f = it_exact->second.find(w);
            if (f != it_exact->second.end()) s += boost_score(f->second, app) * 4;
        }
        if (has_flat) {
            auto f = it_flat->second.find(w);
            if (f != it_flat->second.end()) s += boost_score(f->second, app);
        }
        return s;
    };
    std::stable_sort(cands.begin(), cands.end(),
                     [&](const std::string& a, const std::string& b) { return score(a) > score(b); });
    if (has_exact) {
        size_t added = 0;
        for (const auto& kv : it_exact->second) {
            if (added >= 3) break;
            if (std::find(cands.begin(), cands.end(), kv.first) != cands.end()) continue;
            cands.insert(cands.begin() + added, kv.first);
            added++;
        }
        if (added) logf("boost %s: inserted %zu learned words", exact.c_str(), added);
    }
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
///
/// **分词边界要留住**（2026-10-28，M1 续：分词边界学习）：
///   xi'an   + 西安 (2 字) → 学 `xi'an`  → 下次打 xi'an 才优先，打 xian 仍是「先」
///   xi-an   + 西安        → 同上（`-` 统一成 `'`）
///   xiang-mu + 项目       → 学 `xiang-mu`
/// 边界是用户主动表达的分隔意图，抹掉它等于把"西安"和"先"混成同一个码。
static std::string learn_key(const std::string& raw_code, const std::string& word) {
    std::string code = normalize_boundaries(raw_code);
    std::string flat = strip_separators(code);
    if (flat.empty() || word.empty()) return std::string();
    size_t chars = utf8_char_count(word);
    if (chars == 0) return std::string();
    std::vector<size_t> lens = split_lengths(flat);
    size_t used = 0;
    if (lens.empty() || chars >= lens.size()) {
        used = flat.size();                     // 不像拼音（纯英文）或整串都吃掉了
    } else {
        for (size_t i = 0; i < chars && i < lens.size(); i++) used += lens[i];
    }
    if (used == 0) used = flat.size();
    // 把"吃掉几个字母"映射回带边界的原串（字母才算数，`'` 跳过）
    size_t letters = 0, pos = 0;
    for (; pos < code.size(); pos++) {
        if (code[pos] == '\'') continue;
        if (letters >= used) break;
        letters++;
    }
    std::string key = code.substr(0, pos);
    while (!key.empty() && key.back() == '\'') key.pop_back();
    return key.empty() ? flat : key;
}

/// 这次选择值不值得学？
///
/// 三道筛子（纯逻辑，可以单独测）：
///   1. 纯英文上屏（码本身就是词，比如打 github 直接上屏）不学——那是打英文
///   2. 单字不学——单字由语言模型排序，学了反而跟模型打架（见 USER_MIN_CHARS）
///   3. 空词不学
static bool should_learn(const std::string& raw_code, const std::string& word) {
    if (word.empty()) return false;
    if (word == strip_separators(raw_code)) return false;
    // 单字：只在**码很短**的时候学。
    //
    // 打 `l` 想要「了」、打 `de` 想要「的」——这种短码本身就是一个字，模型却在所有
    // l*/d* 里按词频猜（`l` 给的是 来/里/老/啦/了…，「了」排第五），用户选一百次
    // 也记不住。2026-11-06 用户报"打 l 总是出来来"。
    //
    // 长码不学：`xiangmu` 里选「项」是分词副产品，学了只会跟语言模型打架。
    if (utf8_char_count(word) < USER_MIN_CHARS && strip_separators(raw_code).size() > 2) {
        return false;
    }
    return true;
}

/// 记一次"用户在这个拼音下选了这个词"。
///
/// `app` 是应用名（exe 名小写），空串表示不知道（老 DLL）。
static void learn(const std::string& raw_code, const std::string& word, const std::string& app) {
    if (!should_learn(raw_code, word)) return;
    std::string key = learn_key(raw_code, word);
    if (key.empty()) return;
    std::lock_guard<std::mutex> lock(g_user_mutex);
    UserWord& w = g_user[key][word];
    if (w.total >= USER_COUNT_CAP) return;
    w.total++;
    w.apps[app]++;
    g_user_dirty = true;
    prune_user();
    logf("learn key=%s word=%s app=%s count=%d (this app %d)", key.c_str(), word.c_str(),
         app.empty() ? "-" : app.c_str(), w.total, w.apps[app]);
    save_user();
}

/// 撤销一次学习：用户上屏后马上删掉了，说明刚才那个候选是选错的。
///
/// 计数减 1，减到 0 就把词删掉；这个码下没有词了就把码也删掉。
/// 只在"上屏后立刻删"时调用（DLL 侧计时 2 秒），所以不会误伤真正想要的词。
/// 分应用记账之后：先扣当前应用的次数，当前应用没记过就扣用得最多的那个应用
/// （用户在 A 应用里学了、在 B 应用里删掉，也该把那条记录减回去）。
static void unlearn(const std::string& raw_code, const std::string& word,
                    const std::string& app) {
    std::string key = learn_key(raw_code, word);
    if (key.empty() || word.empty()) return;
    std::lock_guard<std::mutex> lock(g_user_mutex);
    auto it = g_user.find(key);
    if (it == g_user.end()) {
        logf("unlearn key=%s word=%s (no such code)", key.c_str(), word.c_str());
        return;
    }
    auto wit = it->second.find(word);
    if (wit == it->second.end()) {
        logf("unlearn key=%s word=%s (no such word)", key.c_str(), word.c_str());
        return;
    }
    UserWord& w = wit->second;
    // 选择扣哪个应用：当前应用优先，否则扣次数最多的那个
    std::string target;
    if (!app.empty() && w.apps.find(app) != w.apps.end()) {
        target = app;
    } else if (!w.apps.empty()) {
        int best = -1;
        for (const auto& ap : w.apps) {
            if (ap.second > best) {
                best = ap.second;
                target = ap.first;
            }
        }
    }
    if (!w.apps.empty()) {
        auto ait = w.apps.find(target);
        if (ait != w.apps.end()) {
            ait->second--;
            if (ait->second <= 0) w.apps.erase(ait);
        }
    }
    if (w.total > 0) w.total--;
    logf("unlearn key=%s word=%s app=%s total=%d", key.c_str(), word.c_str(),
         target.empty() ? "-" : target.c_str(), w.total);
    if (w.total <= 0) {
        it->second.erase(wit);
        logf("unlearn key=%s word=%s (removed)", key.c_str(), word.c_str());
        if (it->second.empty()) g_user.erase(it);
    }
    g_user_dirty = true;
    save_user();
}

/// Windows 的 `argv` 是按 ANSI 代码页解码的（中文系统上是 GBK），而内部一律用 UTF-8。
/// 命令行里带中文（`--forget <码> <词>` 的第二个参数）时必须转一道，否则永远匹配不上。
static std::string argv_to_utf8(const char* a) {
    if (!a || !*a) return std::string();
    int n = MultiByteToWideChar(CP_ACP, 0, a, -1, nullptr, 0);
    if (n <= 1) return std::string(a);
    std::wstring w((size_t)n - 1, L'\0');
    MultiByteToWideChar(CP_ACP, 0, a, -1, &w[0], n);
    int m = WideCharToMultiByte(CP_UTF8, 0, w.c_str(), (int)w.size(), nullptr, 0, nullptr, nullptr);
    if (m <= 0) return std::string(a);
    std::string out((size_t)m, '\0');
    WideCharToMultiByte(CP_UTF8, 0, w.c_str(), (int)w.size(), &out[0], m, nullptr, nullptr);
    return out;
}

/// 词库巡检 / 清理命令（纯命令行，不需要 librime、不用连管道）。
///
///   --review-dict       列出全部条目，按"选中次数"升序（最没把握的在最上面）
///   --prune-single      删掉所有单字条目（旧版本学的噪音，新版本已经不学了）
///   --forget <码> [词]  删掉某个码下的条目（不给词就删整个码）
///
/// 为什么不自动判断"哪条是选错的"：离线能看到的只有计数，猜不准——
/// 删错比留错更让人火大。真正可靠的信号是"上屏后立刻删掉"，
/// 那个由 DLL 在线采集（协议 type=3，命中就发一次撤回），不靠离线猜测。
static int dict_command(const std::string& mode, const std::string& code, const std::string& word) {
    // 终端默认代码页是 GBK，直接 printf UTF-8 的中文会乱码；先切到 UTF-8。
    SetConsoleOutputCP(65001);
    std::lock_guard<std::mutex> lock(g_user_mutex);

    if (mode == "review") {
        struct Row {
            std::string code;
            std::string word;
            int count;
            std::string apps;
        };
        std::vector<Row> rows;
        for (const auto& kv : g_user) {
            for (const auto& t : kv.second) {
                // 分应用明细：哪个应用里用过几次，一眼看出"这话是在哪儿打的"
                std::string apps;
                for (const auto& ap : t.second.apps) {
                    if (!apps.empty()) apps += ",";
                    apps += ap.first.empty() ? "(unknown)" : ap.first;
                    apps += ":";
                    apps += std::to_string(ap.second);
                }
                rows.push_back({kv.first, t.first, t.second.total, apps});
            }
        }
        std::stable_sort(rows.begin(), rows.end(),
                         [](const Row& a, const Row& b) { return a.count < b.count; });
        printf("user dict : %s\n", g_user_path.c_str());
        printf("entries   : %zu\n\n", rows.size());
        for (const auto& r : rows) {
            const char* tag = utf8_char_count(r.word) < USER_MIN_CHARS ? "  <- single char" : "";
            printf("  %-12s -> %s  x%d  {%s}%s\n", r.code.c_str(), r.word.c_str(), r.count,
                   r.apps.c_str(), tag);
        }
        printf("\ndelete one : --forget <code> [word]\n");
        printf("drop chars : --prune-single\n");
        return 0;
    }

    if (mode == "prune-single") {
        size_t removed = 0;
        for (auto it = g_user.begin(); it != g_user.end();) {
            for (auto wit = it->second.begin(); wit != it->second.end();) {
                if (utf8_char_count(wit->first) < USER_MIN_CHARS) {
                    wit = it->second.erase(wit);
                    removed++;
                } else {
                    ++wit;
                }
            }
            if (it->second.empty()) it = g_user.erase(it);
            else ++it;
        }
        if (removed > 0) {
            g_user_dirty = true;
            save_user();
        }
        printf("removed %zu single-char entries\n", removed);
        return 0;
    }

    if (mode == "forget") {
        if (code.empty()) {
            printf("usage: --forget <code> [word]\n");
            return 1;
        }
        auto it = g_user.find(code);
        if (it == g_user.end()) {
            printf("no such code: %s\n", code.c_str());
            return 1;
        }
        size_t removed = 0;
        if (word.empty()) {
            removed = it->second.size();
            g_user.erase(it);
        } else {
            removed = it->second.erase(word);
            if (it->second.empty()) g_user.erase(code);
        }
        if (removed > 0) {
            g_user_dirty = true;
            save_user();
        }
        printf("removed %zu entries under %s\n", removed, code.c_str());
        return removed > 0 ? 0 : 1;
    }

    return 1;
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


// ───────────────────── 英文补全词表 ─────────────────────
// 打 `gith` 想要 `github`：librime 只认拼音，纯英文输入它只能切成「给/过/跟」，
// 补全只能自己带一份词表（`en-words.txt`，4 万词、按词频排序、与 rime-server.exe 同目录）。
//
// 为什么不塞进 rime 的词典：librime 的查询按拼音切分，英文词进去只会被当成拼音码，
// 排序也压不过语言模型 —— 和 ADR-0009 对个人语料的结论一样。
static std::vector<std::pair<std::string, uint32_t>> g_en_words;   // (词, 词频名次)，按词排序
static bool g_en_tried = false;

/// 读同目录的 `en-words.txt`：一行一个词，越靠前越常用。
static void load_en_words() {
    if (g_en_tried) return;
    g_en_tried = true;
    const std::string path = exe_dir() + "\\en-words.txt";
    FILE* f = fopen(path.c_str(), "rb");
    if (!f) {
        logf("en words: %s not found (no completion)", path.c_str());
        return;
    }
    char buf[128];
    uint32_t rank = 0;
    while (fgets(buf, sizeof(buf), f)) {
        std::string s(buf);
        while (!s.empty() && (s.back() == '\n' || s.back() == '\r')) s.pop_back();
        if (s.size() < 2 || s.size() > 32) continue;
        bool ok = true;
        for (char c : s) {
            if (c < 'a' || c > 'z') { ok = false; break; }
        }
        if (!ok) continue;
        g_en_words.emplace_back(s, rank++);
    }
    fclose(f);
    std::sort(g_en_words.begin(), g_en_words.end(),
              [](const std::pair<std::string, uint32_t>& a,
                 const std::pair<std::string, uint32_t>& b) { return a.first < b.first; });
    logf("en words: %zu loaded", g_en_words.size());
}

/// 词表里以 `prefix` 开头的词，按词频名次取前 `limit` 个（不含 prefix 本身）。
/// 纯函数，`--selftest` 里用假词表验。前缀短于 2 个字母不补全（`a` 会命中几千个）。
static std::vector<std::string> prefix_words(
    const std::vector<std::pair<std::string, uint32_t>>& dict, const std::string& prefix,
    size_t limit) {
    std::vector<std::string> out;
    if (prefix.size() < 2 || limit == 0) return out;
    auto it = std::lower_bound(
        dict.begin(), dict.end(), prefix,
        [](const std::pair<std::string, uint32_t>& a, const std::string& p) { return a.first < p; });
    std::vector<std::pair<uint32_t, const std::string*>> hits;
    for (; it != dict.end(); ++it) {
        if (it->first.compare(0, prefix.size(), prefix) != 0) break;
        // 和输入一模一样的那个词不算"补全"——原样候选由调用方自己插
        if (it->first.size() == prefix.size()) continue;
        hits.emplace_back(it->second, &it->first);
        if (hits.size() >= 2000) break;   // 极端前缀（`re`）够挑了，别再往下扫
    }
    std::sort(hits.begin(), hits.end(),
              [](const std::pair<uint32_t, const std::string*>& a,
                 const std::pair<uint32_t, const std::string*>& b) { return a.first < b.first; });
    for (const auto& h : hits) {
        if (out.size() >= limit) break;
        out.push_back(*h.second);
    }
    return out;
}

/// 跟着用户打的大小写：`Git` → `Github`，`GIT` → `GITHUB`，`git` 原样。
static std::string match_case(const std::string& typed, const std::string& word) {
    if (typed.empty() || word.empty()) return word;
    if (typed[0] < 'A' || typed[0] > 'Z') return word;   // 用户打的是小写，原样给
    bool all_upper = typed.size() >= 2;
    for (char c : typed) {
        if (c < 'A' || c > 'Z') { all_upper = false; break; }
    }
    std::string out = word;
    if (all_upper) {
        for (char& c : out) c = (char)toupper((unsigned char)c);
        return out;
    }
    out[0] = (char)toupper((unsigned char)out[0]);
    return out;
}

/// 给纯字母输入补几个英文词（`gith` → github / gitee / …）。纯函数入口，只读全局词表。
static std::vector<std::string> english_completions(const std::string& typed, size_t limit) {
    load_en_words();
    if (g_en_words.empty()) return {};
    std::string lower;
    lower.reserve(typed.size());
    for (char c : typed) lower.push_back((char)tolower((unsigned char)c));
    std::vector<std::string> out = prefix_words(g_en_words, lower, limit);
    for (std::string& w : out) w = match_case(typed, w);
    return out;
}

// ───────────────────── 联想（词接词）─────────────────────
// 用户连着打「健全」「考核」「激励体系」，这里记 (健全 → 考核, 3)、(考核 → 激励体系, 2)。
// 下次上屏「健全」就把「考核」递过去 —— 个人输入法里最有感知的一块（2026-11-06 用户提的）。
//
// 只记**词对**，不记句子、不记段落：句子在磁盘上就是隐私，词对没有上下文也能看出。
// 文件在 %LOCALAPPDATA%\inklink\phrase-pairs.tsv，和用户词库同一个目录。
static std::map<std::string, std::map<std::string, int>> g_pairs;   // 前词 -> 后词 -> 次数
static std::string g_pairs_path;
static std::mutex g_pairs_mutex;
static bool g_pairs_dirty = false;

static const size_t PAIR_MIN_CHARS = 2;
static const size_t PAIR_MAX_CHARS = 8;
static const size_t PAIR_MAX_PREV = 3000;   // 最多记多少个"前词"，超了丢最弱的
static const int PAIR_COUNT_CAP = 99;

/// 和用户词库同目录的 `phrase-pairs.tsv`。
static std::string pairs_path() {
    std::string user = user_dict_path();
    size_t slash = user.find_last_of("\\/");
    std::string dir = (slash == std::string::npos) ? std::string() : user.substr(0, slash + 1);
    return dir + "phrase-pairs.tsv";
}

/// 这一对值不值得记：两边都是 2~8 个汉字、不是纯英文、不是同一个词。
///
/// 单字不记（`了` 后面接什么太发散，记了也是噪音）；超过 8 个字多半是整句，
/// 那更像"语料"而不是"联想"，留给 M2。
static bool pair_ok(const std::string& prev, const std::string& cur) {
    if (prev.empty() || cur.empty()) return false;
    if (prev == cur) return false;
    if (ascii_only(prev) || ascii_only(cur)) return false;
    size_t pc = utf8_char_count(prev);
    size_t cc = utf8_char_count(cur);
    return pc >= PAIR_MIN_CHARS && pc <= PAIR_MAX_CHARS && cc >= PAIR_MIN_CHARS &&
           cc <= PAIR_MAX_CHARS;
}

/// 从"后词 -> 次数"的表里取前 `limit` 个（次数多的在前，同次数按字的顺序，稳定）。纯函数。
static std::vector<std::string> top_next(const std::map<std::string, int>& m, size_t limit) {
    std::vector<std::pair<int, std::string>> all;
    all.reserve(m.size());
    for (const auto& kv : m) all.emplace_back(kv.second, kv.first);
    std::sort(all.begin(), all.end(), [](const auto& a, const auto& b) {
        if (a.first != b.first) return a.first > b.first;
        return a.second < b.second;
    });
    std::vector<std::string> out;
    for (const auto& kv : all) {
        if (out.size() >= limit) break;
        out.push_back(kv.second);
    }
    return out;
}

static void save_pairs() {
    if (g_pairs_path.empty()) return;
    std::string tmp = g_pairs_path + ".tmp";
    FILE* f = fopen(tmp.c_str(), "wb");
    if (!f) return;
    for (const auto& kv : g_pairs) {
        for (const auto& n : kv.second) {
            fprintf(f, "%s\t%s\t%d\n", kv.first.c_str(), n.first.c_str(), n.second);
        }
    }
    fclose(f);
    // 先写临时文件再换，断电不会留下半个词库（和用户词库同一套做法）
    MoveFileExA(tmp.c_str(), g_pairs_path.c_str(), MOVEFILE_REPLACE_EXISTING);
    g_pairs_dirty = false;
}

static void load_pairs() {
    g_pairs_path = pairs_path();
    FILE* f = fopen(g_pairs_path.c_str(), "rb");
    if (!f) {
        logf("pairs: %s not found (first run)", g_pairs_path.c_str());
        return;
    }
    char line[512];
    size_t n = 0;
    while (fgets(line, sizeof(line), f)) {
        std::string s(line);
        while (!s.empty() && (s.back() == '\n' || s.back() == '\r')) s.pop_back();
        size_t a = s.find('\t');
        if (a == std::string::npos) continue;
        size_t b = s.find('\t', a + 1);
        if (b == std::string::npos) continue;
        std::string prev = s.substr(0, a);
        std::string cur = s.substr(a + 1, b - a - 1);
        int count = atoi(s.c_str() + b + 1);
        if (!pair_ok(prev, cur) || count <= 0) continue;
        g_pairs[prev][cur] = (std::min)(count, PAIR_COUNT_CAP);
        n++;
    }
    fclose(f);
    logf("pairs: %zu loaded", n);
}

/// 超了上限就丢一个最弱的"前词"（按它所有后词的次数和算）。
static void prune_pairs() {
    while (g_pairs.size() > PAIR_MAX_PREV) {
        auto weakest = g_pairs.begin();
        long long weakest_score = -1;
        for (auto it = g_pairs.begin(); it != g_pairs.end(); ++it) {
            long long score = 0;
            for (const auto& kv : it->second) score += kv.second;
            if (weakest_score < 0 || score < weakest_score) {
                weakest_score = score;
                weakest = it;
            }
        }
        g_pairs.erase(weakest);
        g_pairs_dirty = true;
    }
}

/// 记一次"上一个词后面接了 cur"。
static void learn_pair(const std::string& prev, const std::string& cur) {
    if (!pair_ok(prev, cur)) return;
    std::lock_guard<std::mutex> lock(g_pairs_mutex);
    int& c = g_pairs[prev][cur];
    if (c >= PAIR_COUNT_CAP) return;
    c++;
    g_pairs_dirty = true;
    prune_pairs();
    save_pairs();
    logf("pair %s -> %s (%d)", prev.c_str(), cur.c_str(), c);
}

/// 上屏 `word` 之后能接什么（最多 3 个）。
static std::vector<std::string> next_words(const std::string& word, size_t limit) {
    if (word.empty()) return {};
    std::lock_guard<std::mutex> lock(g_pairs_mutex);
    auto it = g_pairs.find(word);
    if (it == g_pairs.end()) return {};
    return top_next(it->second, limit);
}

// ───────────────────────── 查询 ─────────────────────────
static const int MAX_CANDS = 50;

// ───────────────────── 个人语料（M2）─────────────────────
// 笔记里出现的词（`赤兔`、`光纤入户`）不在 rime 词库里，打 `chitu` 永远出不来。
// 抽取管线扫 Markdown -> terms.json -> corpus.tsv，这里启动时读入，查询时把命中的词
// 顶到最前面。**不改 Rime 方案**：合并词库试过，权重再大也压不过语言模型（见 ADR-0009）。
//
// 文件：%LOCALAPPDATA%\inklink\corpus.tsv，一行 `码 \t 词 \t 权重`。
static std::map<std::string, std::vector<std::pair<std::string, int>>> g_corpus;
static std::string g_corpus_path;
/// 每个码最多带多少个语料词（再多也翻不到，白占候选位）
static const size_t CORPUS_MAX_PER_CODE = 8;

static std::string corpus_path() {
    std::string user = user_dict_path();
    size_t slash = user.find_last_of("\\/");
    std::string dir = (slash == std::string::npos) ? std::string() : user.substr(0, slash + 1);
    return dir + "corpus.tsv";
}

/// 语料表的键：去掉分词符和空格的拼音串（`chi tu` 和 `chitu` 是同一个键）。
static std::string corpus_key(const std::string& code) {
    return strip_separators(normalize_boundaries(code));
}

/// 把语料词顶到候选最前面：已有的先删掉再插（不重复），按权重序。纯函数，可单测。
static void corpus_apply(std::vector<std::string>& out,
                         const std::vector<std::pair<std::string, int>>& words) {
    if (words.empty()) return;
    std::vector<std::string> ordered;
    ordered.reserve(words.size());
    for (const auto& kv : words) ordered.push_back(kv.first);
    for (const auto& w : ordered) {
        out.erase(std::remove(out.begin(), out.end(), w), out.end());
    }
    size_t at = 0;
    for (const auto& w : ordered) {
        if (at > out.size()) at = out.size();
        out.insert(out.begin() + (std::ptrdiff_t)at, w);
        ++at;
    }
    if (out.size() > (size_t)MAX_CANDS) out.resize(MAX_CANDS);
}

static void load_corpus() {
    g_corpus_path = corpus_path();
    FILE* f = fopen(g_corpus_path.c_str(), "rb");
    if (!f) {
        logf("corpus: %s not found (run tools\\import-corpus.ps1)", g_corpus_path.c_str());
        return;
    }
    char line[1024];
    size_t n = 0;
    while (fgets(line, sizeof(line), f)) {
        std::string s(line);
        while (!s.empty() && (s.back() == '\n' || s.back() == '\r')) s.pop_back();
        if (s.empty() || s[0] == '#') continue;
        size_t a = s.find('\t');
        if (a == std::string::npos) continue;
        size_t b = s.find('\t', a + 1);
        std::string code = s.substr(0, a);
        std::string word = (b == std::string::npos) ? s.substr(a + 1) : s.substr(a + 1, b - a - 1);
        int weight = (b == std::string::npos) ? 1 : atoi(s.c_str() + b + 1);
        if (code.empty() || word.empty()) continue;
        g_corpus[corpus_key(code)].emplace_back(word, weight);
        n++;
    }
    fclose(f);
    for (auto& kv : g_corpus) {
        std::sort(kv.second.begin(), kv.second.end(),
                  [](const std::pair<std::string, int>& x, const std::pair<std::string, int>& y) {
                      if (x.second != y.second) return x.second > y.second;
                      return x.first < y.first;
                  });
        if (kv.second.size() > CORPUS_MAX_PER_CODE) kv.second.resize(CORPUS_MAX_PER_CODE);
    }
    logf("corpus: %zu codes / %zu entries", g_corpus.size(), n);
}

/// 查询末尾：命中的语料词顶到最前（用户学习词之后、librime 之前）。
static void apply_corpus(const std::string& code, std::vector<std::string>& out) {
    if (g_corpus.empty()) return;
    auto it = g_corpus.find(corpus_key(code));
    if (it == g_corpus.end()) return;
    corpus_apply(out, it->second);
    logf("corpus hit %s -> %zu", code.c_str(), it->second.size());
}


/// 多音节输入里单字候选放哪、给多少。
///
/// librime 对 `xiangmu` 会把第一个音节的单字（想/向/相/…）夹在词中间，像刷屏，
/// 所以挪到多字词后面。1.1.39 挪完只留 5 个，太少了：词不是固定搭配、要一个字一个字
/// 找的时候，想要的字常常不在那 5 个里（用户反馈"单个的字只能出现少数几个，找不到"）。
///
/// 1.1.43 起：
///   - 单字**全部保留**，挪到多字词后面（多字词不到 3 个时不挪，原位不动）；
///   - 再用第一个音节单独查一次（`first_singles`），把 librime 没列出来的单字补在后面，
///     第一个字总能翻到；选了它之后剩下的拼音继续分段查，后面的字自然齐全；
///   - 单字最多 `kSingleMax` 个；用户选过的单字（`keep`）留在原位；英文原样不动。
/// 返回放到末尾的单字个数。
static const size_t kSingleMax = 60;
static size_t arrange_single_chars(std::vector<std::string>& cands, size_t syllables,
                                   const std::set<std::string>& keep,
                                   const std::vector<std::string>& first_singles) {
    if (syllables < 2) return 0;
    auto movable = [&](const std::string& w) {
        return utf8_char_count(w) < 2 && !ascii_only(w) && keep.find(w) == keep.end();
    };
    size_t multi = 0;
    for (const auto& w : cands) {
        if (utf8_char_count(w) >= 2) ++multi;
    }
    std::vector<std::string> singles;
    if (multi >= 3) {
        for (const auto& w : cands) {
            if (movable(w)) singles.push_back(w);
        }
        cands.erase(std::remove_if(cands.begin(), cands.end(), movable), cands.end());
    }
    for (const auto& w : first_singles) {
        if (utf8_char_count(w) != 1 || ascii_only(w)) continue;
        if (std::find(cands.begin(), cands.end(), w) != cands.end()) continue;
        if (std::find(singles.begin(), singles.end(), w) != singles.end()) continue;
        singles.push_back(w);
    }
    if (singles.size() > kSingleMax) singles.resize(kSingleMax);
    cands.insert(cands.end(), singles.begin(), singles.end());
    return singles.size();
}


/// 精确命中最多占前多少个候选位。
///
/// 不能把精确会话的 50 个全放前面：那样模糊音就"等于关掉了"——打 ni 想找「里」
/// 的人得翻十几页。留 15 个（三页）给精确命中，模糊命中的从第 16 位开始接，
/// 既保证第一屏永远是对的，又不至于让模糊音变成摆设。
static const size_t EXACT_HEAD = 15;

/// `app` 是发起查询的应用名（exe 名小写），空串表示不知道（老 DLL）。
static std::vector<std::string> query(const std::string& code, const std::string& app) {
    std::vector<std::string> out;
    if (code.empty()) return out;

    // 带边界的原始码（`xi'an`）只给用户词库用；送给 librime 的一律还原成整串
    // （它不认 `'` 当分隔，实测 `xi'an` 与 `xian` 的候选集一样）。
    const std::string flat = strip_separators(code);
    if (flat.empty()) return out;

    bool looks_pinyin = is_pinyin_code(flat);

    // ── 纠错探针 ──
    // 三种情况分开处理：
    //  - 正常拼音（`xiangmu`）：整串切得开，一个探针都不发，不占同步路径预算；
    //  - 末尾打了一半（`zheg`/`xiangm`）：librime 的前缀匹配本就处理得好，
    //    插手反而会把「项目」换成「向」；
    //  - 中间多/错/换位一个字母（`zhemge`/`niihao`/`jintain`）：rime 只能拿多余
    //    字母硬切出一串怪词，这时候纠错结果才更可信。
    std::vector<std::string> corrected;
    if (flat.size() >= 3 && !is_full_pinyin(flat) && !ends_with_half_syllable(flat)) {
        std::vector<std::string> vars = edit_variants(flat);
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

    // ── 精确优先 ──
    // 先问「不带模糊音」的会话。开了模糊音时，librime 会把 huang 也匹配到 fang，
    // 而「放」的频次是「黄」的十倍，结果第一个候选变成「放」。
    // 两个会话的候选合并时把精确命中的放前面，模糊命中接在后面 ——
    // 模糊音该起作用的地方（真打错、口音）仍然能兜到，只是不再抢第一屏。
    std::vector<std::string> exact;
    if (g_exact && g_fuzzy_on) {
        exact = collect_candidates(g_exact, flat, EXACT_HEAD);
    }

    out = collect_candidates(g_session, flat, MAX_CANDS);
    if (out.empty() && exact.empty()) {
        if (corrected.empty()) return out;
        return corrected;   // 连 rime 都喂不进去，直接给纠错结果
    }

    if (!exact.empty()) {
        std::vector<std::string> merged = exact;
        for (const std::string& w : out) {
            if (std::find(merged.begin(), merged.end(), w) == merged.end()) merged.push_back(w);
        }
        if (merged.size() > (size_t)MAX_CANDS) merged.resize(MAX_CANDS);
        out.swap(merged);
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

    // 英文候选：把原始输入作为候选追加（github、vscode 这类英文词），
    // 再补几个以它开头的英文词（1.1.46：`gith` → github / gitee / …）。
    // librime 只认拼音，纯英文会被切成 给/过/跟，用户就没法直接上屏英文了。
    // 用户打了分词边界（`xi'an`）就是在说"这是拼音"，不再塞英文字母候选。
    if (flat.size() >= 2 && flat == code) {
        bool is_pure_alpha = true;
        for (char c : flat) {
            if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'))) { is_pure_alpha = false; break; }
        }
        if (is_pure_alpha) {
            // 不像拼音时英文原样要留在第一屏：没有纠错结果（github/vscode）排最前；
            // 有纠错结果说明可能是打错的拼音（niihao → 你好），让纠错占第一，
            // 原样紧跟其后（`update` 这种"恰好差一个字母就是合法拼音"的英文词
            // 因此仍然一眼能看到）。像拼音的（`code`）原样排最后，一直如此。
            //
            // 2026-10-28：再补一条 —— 前缀能连着切出两个完整音节，说明用户是在打
            // 拼音、只是后面打坏了（`shashahs` → sha|sha|…），这种情况原始字母
            // 也不该抢第一屏：搜狗、微信输入法都是直接给中文，用户选中即是中文，
            // 想要英文按 Shift/回车即可。真正的英文词（`github`/`hello`/`test`）
            // 前缀一个音节都凑不出两个，仍然排第一。
            size_t pos = out.size();   // 默认：排在候选最后
            if (!looks_pinyin && corrected.empty()) {
                bool pinyin_typo = split_lengths(flat).size() >= 2;
                pos = pinyin_typo ? out.size() : 0;
            } else if (!looks_pinyin) {
                pos = 1;
            }
            auto it = std::find(out.begin(), out.end(), flat);
            if (it == out.end()) {
                // 候选满 50 个时先挤掉最后一个：librime 对 github/hello 这种纯英文
                // 会吐满一页中文（给/过/国…），原来的 `out.size() < MAX_CANDS` 守卫
                // 让原始英文**永远挤不进去**，用户连"原样上屏"都点不到。
                if (out.size() >= (size_t)MAX_CANDS) out.pop_back();
                if (pos > out.size()) pos = out.size();
                out.insert(out.begin() + pos, flat);
            } else {
                pos = (size_t)(it - out.begin());
            }
            // 补全只补"看着就是英文"的输入（原样在第一屏）。像拼音的 `shi`/`code`
            // 用户多半在打中文，一个英文词都不塞，免得把中文顶下去。
            if (pos <= 1) {
                std::vector<std::string> compl = english_completions(flat, 6);
                size_t at = pos + 1;
                for (const std::string& w : compl) {
                    if (std::find(out.begin(), out.end(), w) != out.end()) continue;
                    if (at >= out.size()) { out.push_back(w); ++at; continue; }
                    // 候选已经满 50 个：挤掉最后一个（最不常用的中文）给补全腾位。
                    // 不挤的话 `github` 这种一页塞满中文的输入一个补全都进不来。
                    if (out.size() >= (size_t)MAX_CANDS) out.pop_back();
                    if (at > out.size()) at = out.size();
                    out.insert(out.begin() + at, w);
                    ++at;
                }
            }
        }
    }
    // 多音节输入里的单字挪到词后面、并用第一个音节补全（见 arrange_single_chars），
    // 在用户词库加权之前做。
    // 为什么放在 boost 前：boost 只会把用户选过的词往前挪，
    // 不会新增候选，所以先剔后挪的结果与反过来一样，但少一次遍历。
    {
        std::set<std::string> keep;
        {
            std::lock_guard<std::mutex> lock(g_user_mutex);
            const std::string exact = normalize_boundaries(code);
            const std::string flatk = strip_separators(exact);
            for (const std::string* k : {&exact, &flatk}) {
                auto it = g_user.find(*k);
                if (it == g_user.end()) continue;
                for (const auto& kv : it->second) keep.insert(kv.first);
            }
        }
        // 按用户打的分词边界分段（xi'an = xi + an），每段再贪心切音节；顺手记下第一个音节
        const std::string bounded = normalize_boundaries(code);
        size_t syllables = 0;
        std::string first;
        size_t start = 0;
        while (start <= bounded.size()) {
            const size_t q = bounded.find('\'', start);
            const std::string seg =
                bounded.substr(start, q == std::string::npos ? std::string::npos : q - start);
            const std::vector<size_t> lens = split_lengths(seg);
            if (first.empty() && !lens.empty()) first = seg.substr(0, lens[0]);
            syllables += lens.size();
            if (q == std::string::npos) break;
            start = q + 1;
        }
        // 第一个音节单独查一次：单音节查询给的是这个音的全部常用字（按字频），
        // 多音节查询里 librime 只顺带列出其中一部分。多一次 librime 查询，约 1~2ms。
        std::vector<std::string> first_singles;
        if (syllables >= 2 && !first.empty()) {
            first_singles = collect_candidates(g_session, first, kSingleMax);
        }
        const size_t moved = arrange_single_chars(out, syllables, keep, first_singles);
        if (moved) {
            logf("singles at tail: %zu (code=%s first=%s)", moved, code.c_str(), first.c_str());
        }
    }

    // 用户词库：这个拼音下用户选过的词提前（次数多的更前）。
    // 放在纠错/英文兜底之后、返回之前 —— 用户的手动选择优先级最高。
    // 传**带边界**的原始码：`xi'an` 下学过的「西安」只在打 `xi'an` 时提前，
    // 打 `xian` 仍然是「先」（这就是分词边界学习）。
    apply_user_boost(code, app, out);

    // 个人语料最后压上来：笔记里的词优先级最高（ADR-0009：语料 > 学习词 > 模型）
    apply_corpus(code, out);

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
static bool select_candidate(uint32_t index, const std::string& code, const std::string& word,
                            const std::string& app) {
    logf("select %u code=%s word=%s app=%s", index, code.c_str(), word.c_str(),
         app.empty() ? "-" : app.c_str());
    if (word.empty()) return true;
    learn(code, word, app);
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

/// 有没有后续数据可读（最多等 `ms` 毫秒），**不阻塞**。
///
/// 为什么需要它：协议 v3 的选词报文比 v2 多了 code/word 三个字段。老 DLL
/// （< 1.0.0）只发 `[type][index]` 五个字节就等响应，而引擎如果在
/// `read_exact(&clen, 4)` 上死等，两边就互相等 —— 老应用选中候选的瞬间整个
/// UI 线程挂住（2026-10-27 微信被系统当"未响应"杀掉那次的根因）。
/// 所以先看有没有后续字节，没有就按老协议立即回应。
static bool wait_readable(HANDLE p, DWORD ms) {
    DWORD start = GetTickCount();
    for (;;) {
        DWORD avail = 0;
        if (!PeekNamedPipe(p, nullptr, 0, nullptr, &avail, nullptr)) return false;
        if (avail > 0) return true;
        if (GetTickCount() - start >= ms) return false;
        Sleep(5);
    }
}

/// 读可选的"应用名"尾巴（协议 v5）：`[u16 app_len][app]`。
///
/// 老 DLL 不会发这一段，所以先 `wait_readable` 探测 —— 没有就当空串，
/// 绝不能死等（否则又是一次"两边互等，UI 线程挂死"）。
static std::string read_app(HANDLE p) {
    // 只等 5ms，不是 100ms。
    //
    // 当前 DLL 把 `[type][len][code]` 和 `[app_len][app]` 一次 WriteFile 全写出来，
    // 到这里 PeekNamedPipe 立刻就能看到字节，根本不用等。等 100ms 的意义只在
    // "老 DLL 完全不发 app 段"那种情况 —— 而那会让**每次查询都白等 100ms**
    // （实测：不带 app 段 108ms，带 app 段 2ms）。
    // 5ms 足够覆盖管道里跨两次写到达的极端情况，又不至于把老客户端拖慢一个数量级。
    if (!wait_readable(p, 5)) return std::string();
    uint16_t alen = 0;
    if (!read_exact(p, &alen, 2)) return std::string();
    if (alen == 0 || alen > 256) return std::string();
    std::string app(alen, '\0');
    if (!read_exact(p, &app[0], alen)) return std::string();
    return app;
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

// ───────────────────── AI（P4）：翻译 / 改写 / 续写 ─────────────────────
//
// 协议：
//   6 = 发起   [u32 seq][u32 hwnd][u8 action][u16 len][text]   -> []
//   7 = 取结果 [u32 seq]                                       -> [u8 state][u16 len][text]
//       state: 0 还在跑 / 1 好了 / 2 失败（text 是原因，给日志看）
//
// 为什么是"发起 + 再取"两趟，而不是一次把结果等回来：
// DLL 跑在别人的 UI 线程上，一次 HTTP 要几秒，直接等就是把用户的窗口挂住。
// 所以引擎在后台线程里发请求，完了 PostMessage 通知那个 HWND，DLL 收到消息再来取
// （那时结果已经在内存里，取一次是微秒级）。DLL 里始终不起线程、不阻塞——铁律第一条。
//
// 触发入口在浮条的 ☰ 菜单（AI 那一组），作用对象是**当前组合串**。

enum AiAction { AI_TRANSLATE = 0, AI_REWRITE = 1, AI_CONTINUE = 2 };

struct AiJob {
    int state = 0;      // 0 跑着 / 1 好了 / 2 失败
    std::string text;   // 结果，或错误原因
};

static std::mutex g_ai_mutex;
static std::map<uint32_t, AiJob> g_ai;

/// 从注册表读一项字符串配置；没有就返回空。
static std::string reg_string(HKEY root, const char* path, const char* name) {
    HKEY key = nullptr;
    if (RegOpenKeyExA(root, path, 0, KEY_READ, &key) != ERROR_SUCCESS) return {};
    char buf[1024] = {0};
    DWORD cb = sizeof(buf) - 1;
    DWORD type = 0;
    std::string out;
    if (RegQueryValueExA(key, name, nullptr, &type, reinterpret_cast<LPBYTE>(buf), &cb) ==
            ERROR_SUCCESS &&
        (type == REG_SZ || type == REG_EXPAND_SZ)) {
        out.assign(buf, cb);
        while (!out.empty() && (out.back() == '\0' || out.back() == '\r' || out.back() == '\n')) {
            out.pop_back();
        }
    }
    RegCloseKey(key);
    return out;
}

static DWORD reg_dword(HKEY root, const char* path, const char* name, DWORD fallback) {
    HKEY key = nullptr;
    if (RegOpenKeyExA(root, path, 0, KEY_READ, &key) != ERROR_SUCCESS) return fallback;
    DWORD v = fallback, cb = sizeof(v), type = 0;
    if (RegQueryValueExA(key, name, nullptr, &type, reinterpret_cast<LPBYTE>(&v), &cb) !=
            ERROR_SUCCESS ||
        type != REG_DWORD) {
        v = fallback;
    }
    RegCloseKey(key);
    return v;
}

struct AiConfig {
    bool enabled = false;
    std::string endpoint;
    std::string model;
};

static AiConfig ai_config() {
    AiConfig c;
    c.enabled = reg_dword(HKEY_CURRENT_USER, "Software\\inklink\\Config", "AI_Enable", 0) != 0;
    c.endpoint = reg_string(HKEY_CURRENT_USER, "Software\\inklink\\Config", "AI_Endpoint");
    c.model = reg_string(HKEY_CURRENT_USER, "Software\\inklink\\Config", "AI_Model");
    if (c.endpoint.empty()) c.endpoint = "http://127.0.0.1:11434";
    if (c.model.empty()) c.model = "qwen2.5:7b";
    while (!c.endpoint.empty() && c.endpoint.back() == '/') c.endpoint.pop_back();
    return c;
}

static std::string json_escape(const std::string& s) {
    std::string out;
    out.reserve(s.size() + 8);
    for (char ch : s) {
        switch (ch) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default:
                if (static_cast<unsigned char>(ch) < 0x20) {
                    char buf[8];
                    snprintf(buf, sizeof(buf), "\\u%04x", ch);
                    out += buf;
                } else {
                    out += ch;
                }
        }
    }
    return out;
}

/// 按动作拼提示词。要求"只输出结果"——模型的解释会混进用户的正文里。
static std::string ai_prompt(int action, const std::string& text) {
    std::string head;
    switch (action) {
        case AI_REWRITE:
            // 1.1.40 起"改写"的语义收窄成校对型：错别字、用词、逻辑漏洞
            head = "把下面这段文字改写：修正错别字、优化用词、理顺逻辑（有明显漏洞就补上），"
                   "保持原意和语气；只输出改写后的正文：\n";
            break;
        case AI_CONTINUE:
            // 续写的硬约束：前文一个字都不动，只输出新增部分（DLL 负责接在后面）
            head = "保持下面这段文字不变，顺着它的语气和逻辑往后写，不要重复已有的内容；"
                   "只输出新写出来的部分：\n";
            break;
        default: head = "把下面这段内容翻译成中文，只输出译文：\n";
    }
    return head + text;
}

/// 最小的 JSON 字符串取值：找 "response":"..." 或 "content":"..."。
/// 不引 JSON 库——只要一个字段，而且 Ollama 的响应结构是固定的。
static std::string json_field(const std::string& body, const char* name) {
    // 冒号两侧可能有空格（Ollama 的 python 客户端就带），这里一路跳过
    std::string key = std::string(1, '"') + name + std::string(1, '"');
    size_t pos = body.find(key);
    if (pos == std::string::npos) return {};
    pos += key.size();
    auto skip_ws = [&]() {
        while (pos < body.size()) {
            char c2 = body[pos];
            if (c2 == ' ' || c2 == '\t' || c2 == '\n' || c2 == '\r') {
                ++pos;
            } else {
                break;
            }
        }
    };
    skip_ws();
    if (pos >= body.size() || body[pos] != ':') return {};
    ++pos;
    skip_ws();
    if (pos >= body.size() || body[pos] != '"') return {};
    ++pos;
    std::string out;
    for (size_t i = pos; i < body.size(); ++i) {
        char ch = body[i];
        if (ch == '"') break;
        if (ch == '\\' && i + 1 < body.size()) {
            char nx = body[++i];
            switch (nx) {
                case 'n': out += '\n'; break;
                case 'r': out += '\r'; break;
                case 't': out += '\t'; break;
                case '"': out += '"'; break;
                case '\\': out += '\\'; break;
                case '/': out += '/'; break;
                case 'u': {
                    // 只处理 ASCII 区间的 \uXXXX；中文一般直接是 UTF-8，走不到这里
                    if (i + 4 < body.size()) {
                        int code = std::strtol(body.substr(i + 1, 4).c_str(), nullptr, 16);
                        if (code < 0x80) out += static_cast<char>(code);
                        i += 4;
                    }
                    break;
                }
                default: out += nx;
            }
            continue;
        }
        out += ch;
    }
    return out;
}

/// 拼 Ollama `/api/generate` 的请求体。
///
/// `no_think`：要不要带 `"think":false`。qwen3.x、gpt-oss 这类"会先想一想"的模型，
/// 不关掉就先输出几百个 token 的推理再给结果——实测 qwen3.5:9b 翻一句 `hello world`
/// 关掉要 145ms、开着要 3.5s，长文更是动辄超时。对不支持思考的模型（gemma3 等）
/// 新版 Ollama 会忽略这个字段；老版本可能报 400，调用方会去掉它再试一次。
static std::string ai_request_body(const std::string& model, const std::string& prompt,
                                   bool no_think) {
    std::string b = "{\"model\":\"" + json_escape(model) + "\",\"prompt\":\"" +
                    json_escape(prompt) + "\",\"stream\":false";
    if (no_think) b += ",\"think\":false";
    b += "}";
    return b;
}

/// 去掉模型输出里的 `<think>…</think>` 段，再去掉开头的空白。
///
/// 关了思考的模型一般不会输出它，但有的模板（老版本 Ollama、自制模型）还是会把推理
/// 写进 `response`——那段混进用户正文就糟了。没闭合的 `<think>` 当作整段都是推理。
static std::string strip_think(const std::string& s) {
    std::string out;
    size_t pos = 0;
    while (pos < s.size()) {
        size_t open = s.find("<think>", pos);
        if (open == std::string::npos) {
            out += s.substr(pos);
            break;
        }
        out += s.substr(pos, open - pos);
        size_t close = s.find("</think>", open);
        if (close == std::string::npos) break;
        pos = close + 8;
    }
    size_t start = 0;
    while (start < out.size() &&
           (out[start] == ' ' || out[start] == '\n' || out[start] == '\r' || out[start] == '\t')) {
        ++start;
    }
    return out.substr(start);
}

/// 把 HTTP 层的失败翻成用户看得懂的一句话（显示在 AI 面板里）。
///
/// 以前面板上只有「WinHttp 请求失败: 12029」——用户看不出是 Ollama 没开。
/// `win_err` 是 WinHTTP 的 `GetLastError()`（0 = 请求本身成功），`http_status` 是状态码。
static std::string ai_error_text(DWORD win_err, DWORD http_status, const std::string& body,
                                 const std::string& endpoint, const std::string& model) {
    if (win_err == 12029 || win_err == 12007) {   // CANNOT_CONNECT / NAME_NOT_RESOLVED
        return "连不上 AI 服务（" + endpoint + "）：Ollama 没启动？先运行 Ollama 再点「重新生成」";
    }
    if (win_err == 12002) {   // ERROR_WINHTTP_TIMEOUT
        return "AI 服务响应超时：模型可能还在加载，稍等再点「重新生成」";
    }
    if (win_err != 0) {
        return "请求 AI 服务失败（WinHTTP 错误 " + std::to_string(win_err) + "）";
    }
    if (http_status == 404 && body.find("not found") != std::string::npos) {
        return "模型 " + model + " 没装：在命令行运行 ollama pull " + model +
               "，或在设置里换一个已装的模型";
    }
    return "AI 服务返回 HTTP " + std::to_string(http_status) + "：" + body.substr(0, 200);
}

/// POST 一段 JSON 到 http://host:port/path。成功（2xx）返回 true，body 写进 out。
///
/// 失败时 `out` 是响应体（可能为空），`http_status` / `win_err` 给调用方拼错误提示。
static bool http_post_json(const std::string& url, const std::string& body, std::string* out,
                           DWORD* http_status, DWORD* win_err) {
    *http_status = 0;
    *win_err = 0;
    std::string rest = url;
    const std::string scheme = "http://";
    if (rest.rfind(scheme, 0) == 0) rest = rest.substr(scheme.size());
    size_t slash = rest.find('/');
    std::string hostport = slash == std::string::npos ? rest : rest.substr(0, slash);
    std::string path = slash == std::string::npos ? "/" : rest.substr(slash);
    size_t colon = hostport.find(':');
    std::string host = colon == std::string::npos ? hostport : hostport.substr(0, colon);
    int port = colon == std::string::npos ? 80 : std::atoi(hostport.c_str() + colon + 1);
    if (host.empty()) {
        *win_err = 12007;
        return false;
    }
    std::wstring whost(host.begin(), host.end());
    std::wstring wpath(path.begin(), path.end());

    HINTERNET session = WinHttpOpen(L"inklink/1.0", WINHTTP_ACCESS_TYPE_NO_PROXY,
                                    WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
    if (!session) {
        *win_err = GetLastError();
        return false;
    }
    // 不走系统代理：目标是本机 Ollama，挂上代理反而连不上。
    // 接收超时 120s（原来 30s）：9B 模型冷启动光加载就 7~15s，再改写一段 2000 字的
    // 文字，30s 经常不够，表现就是"点了没反应，过一会儿报失败"。这在引擎的后台线程里，
    // 不占任何同步路径。
    WinHttpSetTimeouts(session, 3000, 3000, 30000, 120000);
    bool ok = false;
    HINTERNET conn = WinHttpConnect(session, whost.c_str(), static_cast<INTERNET_PORT>(port), 0);
    if (!conn) *win_err = GetLastError();
    if (conn) {
        HINTERNET req = WinHttpOpenRequest(conn, L"POST", wpath.c_str(), nullptr,
                                           WINHTTP_NO_REFERER, WINHTTP_DEFAULT_ACCEPT_TYPES, 0);
        if (req) {
            const wchar_t* headers = L"Content-Type: application/json\r\n";
            if (WinHttpSendRequest(req, headers, static_cast<DWORD>(-1L),
                                   const_cast<char*>(body.data()),
                                   static_cast<DWORD>(body.size()),
                                   static_cast<DWORD>(body.size()), 0) &&
                WinHttpReceiveResponse(req, nullptr)) {
                DWORD status = 0, len = sizeof(status);
                WinHttpQueryHeaders(req, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                                    WINHTTP_HEADER_NAME_BY_INDEX, &status, &len,
                                    WINHTTP_NO_HEADER_INDEX);
                std::string data;
                DWORD avail = 0;
                do {
                    if (!WinHttpQueryDataAvailable(req, &avail)) break;
                    if (avail == 0) break;
                    std::string chunk(avail, '\0');
                    DWORD got = 0;
                    if (!WinHttpReadData(req, &chunk[0], avail, &got) || got == 0) break;
                    data.append(chunk.data(), got);
                } while (true);
                *http_status = status;
                *out = data;
                ok = status >= 200 && status < 300;
            } else {
                *win_err = GetLastError();
            }
            WinHttpCloseHandle(req);
        } else {
            *win_err = GetLastError();
        }
        WinHttpCloseHandle(conn);
    }
    WinHttpCloseHandle(session);
    return ok;
}

static void ai_set(uint32_t seq, int state, const std::string& text) {
    std::lock_guard<std::mutex> lock(g_ai_mutex);
    AiJob& job = g_ai[seq];
    job.state = state;
    job.text = text;
}

static void ai_worker(uint32_t seq, uint32_t hwnd, int action, std::string text) {
    const AiConfig cfg = ai_config();
    const std::string url = cfg.endpoint + "/api/generate";
    const std::string prompt = ai_prompt(action, text);
    std::string resp;
    DWORD status = 0, win_err = 0;
    bool ok = http_post_json(url, ai_request_body(cfg.model, prompt, true), &resp, &status,
                             &win_err);
    // 老版本 Ollama 或不支持思考的模型可能对 "think" 报 400：去掉再试一次
    if (!ok && win_err == 0 && status == 400 && resp.find("think") != std::string::npos) {
        logf("ai seq=%u: endpoint rejected think=false, retry without it", seq);
        ok = http_post_json(url, ai_request_body(cfg.model, prompt, false), &resp, &status,
                            &win_err);
    }
    std::string result;
    if (ok) {
        result = strip_think(json_field(resp, "response"));
        if (result.empty()) result = strip_think(json_field(resp, "content"));   // OpenAI 兼容端点
        if (result.empty()) {
            ai_set(seq, 2, "模型没返回内容（换一个模型试试）");
        } else {
            ai_set(seq, 1, result);
        }
    } else {
        // 服务端自己的 {"error":"..."} 比状态码更说明问题，优先拿它
        std::string err_body = json_field(resp, "error");
        if (err_body.empty()) err_body = resp;
        result = ai_error_text(win_err, status, err_body, cfg.endpoint, cfg.model);
        ai_set(seq, 2, result);
    }
    logf("ai seq=%u action=%d ok=%d status=%lu winerr=%lu len=%zu", seq, action, ok ? 1 : 0,
         static_cast<unsigned long>(status), static_cast<unsigned long>(win_err), result.size());
    if (hwnd) {
        PostMessageW(reinterpret_cast<HWND>(static_cast<uintptr_t>(hwnd)), WM_APP + 30,
                     static_cast<WPARAM>(seq), 0);
    }
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
        // 老 DLL（< 1.0.0）只发 `[type][index]` 就等响应，后续字段永远不来。
        // 这里必须先探测再读，否则两边互等，调用方 UI 线程直接挂死。
        if (!wait_readable(p, 100) || !read_exact(p, &clen, 4)) {
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
        if (!wait_readable(p, 100) || !read_exact(p, &wlen, 2)) {
            write_response(p, {});
            return;
        }
        std::string word(wlen, '\0');
        if (wlen > 0 && !read_exact(p, &word[0], wlen)) return;
        select_candidate(index, code, word, read_app(p));
        write_response(p, {});
        return;
    }

    if (type == 6) {
        // 发起 AI 请求：[u32 seq][u32 hwnd][u8 action][u16 len][text]
        uint32_t seq = 0, hwnd = 0;
        uint8_t action = 0;
        uint16_t len = 0;
        if (!read_exact(p, &seq, 4) || !read_exact(p, &hwnd, 4) || !read_exact(p, &action, 1) ||
            !read_exact(p, &len, 2)) {
            return;
        }
        std::string text(len, '\0');
        if (len > 0 && !read_exact(p, &text[0], len)) return;
        if (!ai_config().enabled) {
            ai_set(seq, 2, "AI 没开（设置里勾上「启用 AI」）");
        } else if (text.empty()) {
            ai_set(seq, 2, "没有可处理的文字");
        } else {
            ai_set(seq, 0, "");
            std::thread(ai_worker, seq, hwnd, static_cast<int>(action), text).detach();
        }
        write_response(p, {});
        return;
    }

    if (type == 7) {
        // 取结果：[u32 seq] -> [u8 state][u16 len][text]
        uint32_t seq = 0;
        if (!read_exact(p, &seq, 4)) return;
        int state = 3;
        std::string text;
        {
            std::lock_guard<std::mutex> lock(g_ai_mutex);
            auto it = g_ai.find(seq);
            if (it != g_ai.end()) {
                state = it->second.state;
                text = it->second.text;
                if (state != 0) g_ai.erase(it);   // 取走就清，免得越积越多
            }
        }
        if (text.size() > 60000) text.resize(60000);
        std::vector<uint8_t> out;
        out.push_back(static_cast<uint8_t>(state));
        uint16_t n = static_cast<uint16_t>(text.size());
        out.push_back(static_cast<uint8_t>(n & 0xFF));
        out.push_back(static_cast<uint8_t>(n >> 8));
        out.insert(out.end(), text.begin(), text.end());
        write_all(p, out.data(), static_cast<DWORD>(out.size()));
        FlushFileBuffers(p);
        return;
    }

    if (type == 9) {
        // 联想（协议 v9）：[u16 prev_len][prev][u16 cur_len][cur]
        //   prev 为空 = 只问不学；返回格式和查询一样（[u32 count] + 候选）。
        uint16_t plen = 0;
        if (!read_exact(p, &plen, 2)) return;
        std::string prev(plen, '\0');
        if (plen && !read_exact(p, &prev[0], plen)) return;
        uint16_t clen = 0;
        if (!read_exact(p, &clen, 2)) return;
        std::string cur(clen, '\0');
        if (clen && !read_exact(p, &cur[0], clen)) return;
        if (!prev.empty()) learn_pair(prev, cur);
        write_response(p, next_words(cur, 3));
        return;
    }

    if (type == 3) {
        // 撤回学习（协议 v4）：[u32 code_len][code][u16 word_len][word]
        // DLL 发现"上屏后立刻被删掉"就发这个，把刚才那次学习扣回去。
        uint32_t clen = 0;
        if (!read_exact(p, &clen, 4)) return;
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
        unlearn(code, word, read_app(p));
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
        cands = query(code, read_app(p));
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
    ck(strip_separators("xi'an") == "xian", "strip_separators: drop apostrophe");
    ck(normalize_boundaries("xi-an") == "xi'an", "normalize_boundaries: dash -> apostrophe");
    ck(normalize_boundaries("xi_an") == "xi'an", "normalize_boundaries: underscore -> apostrophe");
    ck(normalize_boundaries("xi--an") == "xi'an", "normalize_boundaries: collapse repeats");
    ck(normalize_boundaries("xian") == "xian", "normalize_boundaries: plain unchanged");
    ck(normalize_boundaries("-xian-") == "xian", "normalize_boundaries: trim edges");

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
    ck(learn_key("xiang-mu", "项目") == "xiang'mu", "learn_key: boundary kept");
    ck(learn_key("xi'an", "西安") == "xi'an", "learn_key: apostrophe boundary kept");
    ck(learn_key("xi-an", "西安") == "xi'an", "learn_key: dash becomes apostrophe");
    ck(learn_key("zhemge", "这个") == "zhemge", "learn_key: mistyped code kept as-is");
    ck(learn_key("x", "西") == "x", "learn_key: single letter");

    ck(should_learn("l", "了"), "should_learn: single char on a short code is learned");
    ck(should_learn("de", "的"), "should_learn: single char on a 2-letter code is learned");
    ck(!should_learn("xiangmu", "项"), "should_learn: single char on a long code is not learned");
    ck(!should_learn("github", "github"), "should_learn: english commit is not learned");
    ck(should_learn("xiangmu", "项目"), "should_learn: normal word is learned");
    ck(should_learn("pikeqiu", "皮克"), "should_learn: two chars is enough");
    ck(!should_learn("xiangmu", ""), "should_learn: empty word is not learned");

    // boost_score：全局 + 当前应用次数 ×2（当前应用里的词一定赢，但全局次数仍能
    // 让"在别处反复用过的词"压过"刚用过一次的词"）
    UserWord bw;
    bw.total = 5;
    bw.apps["weixin"] = 2;
    ck(boost_score(bw, "weixin") == 9, "boost_score: own app doubled");
    ck(boost_score(bw, "code") == 5, "boost_score: other app counts global only");
    // 单字（1.1.43）：多音节输入里挪到词后面，但全部保留，并用第一个音节补全
    {
        std::set<std::string> keep;
        const std::vector<std::string> none;
        std::vector<std::string> v = {"项目", "橡木", "香木", "向慕", "向暮",
                                      "想", "向", "相", "像", "象", "降", "项"};
        ck(arrange_single_chars(v, 2, keep, none) == 7, "singles: all 7 kept, moved to tail");
        ck(v.size() == 12 && v[0] == "项目" && v[4] == "向暮" && v[5] == "想" && v[11] == "项",
           "singles: words first, singles after in rime order");

        std::vector<std::string> w = {"项目", "橡木", "香木", "想", "向"};
        const std::vector<std::string> first = {"想", "向", "相", "香", "乡", "项目"};
        ck(arrange_single_chars(w, 2, keep, first) == 5, "singles: first syllable fills in");
        ck(w.size() == 8 && w[3] == "想" && w[4] == "向" && w[5] == "相" && w[7] == "乡",
           "singles: fill-ins after rime's own, no dups, no words");

        std::vector<std::string> one = {"你", "尼", "泥"};
        ck(arrange_single_chars(one, 1, keep, first) == 0 && one.size() == 3,
           "singles: single syllable untouched");

        std::vector<std::string> few = {"王佳", "王", "望"};
        const std::vector<std::string> wang = {"王", "望", "往", "忘"};
        ck(arrange_single_chars(few, 2, keep, wang) == 2, "singles: few words -> no move, still fill");
        ck(few.size() == 5 && few[1] == "王" && few[3] == "往" && few[4] == "忘",
           "singles: few words keep order, fill-ins appended");

        std::vector<std::string> kept = {"项目", "橡木", "香木", "想", "向"};
        std::set<std::string> liked = {"想"};
        ck(arrange_single_chars(kept, 2, liked, none) == 1 && kept[3] == "想" && kept[4] == "向",
           "singles: user-picked single stays in place");

        std::vector<std::string> eng = {"项目", "橡木", "香木", "github"};
        ck(arrange_single_chars(eng, 2, keep, none) == 0 && eng[3] == "github",
           "singles: ascii raw untouched");

        std::vector<std::string> big = {"项目", "橡木", "香木"};
        std::vector<std::string> lots;
        for (int i = 0; i < 100; ++i) {   // U+4E00.. 一百个不同的单字
            std::string s;
            s += static_cast<char>(0xE4);
            s += static_cast<char>(0xB8 + (i >> 6));
            s += static_cast<char>(0x80 + (i & 63));
            lots.push_back(s);
        }
        ck(arrange_single_chars(big, 2, keep, lots) == kSingleMax && big.size() == 3 + kSingleMax,
           "singles: capped at kSingleMax");
    }

    ck(boost_score(bw, "") == 5, "boost_score: unknown app counts global only");

    UserWord once;
    once.total = 1;
    once.apps["code"] = 1;
    ck(boost_score(bw, "code") > boost_score(once, "code"),
        "boost_score: used in other apps still beats used once");

    // AI 提示词（1.1.40 改过语义）：改写要覆盖错别字/用词/逻辑，续写只输出新增部分。
    ck(ai_prompt(AI_REWRITE, "x").find("错别字") != std::string::npos,
       "ai_prompt: rewrite is proofreading");
    ck(ai_prompt(AI_CONTINUE, "x").find("不要重复") != std::string::npos,
       "ai_prompt: continue appends only");

    // 1.1.43：思考型模型（qwen3.5:9b）不关思考就慢到超时 —— 请求体必须带 think:false
    ck(ai_request_body("qwen3.5:9b", "hi", true) ==
           "{\"model\":\"qwen3.5:9b\",\"prompt\":\"hi\",\"stream\":false,\"think\":false}",
       "ai_request_body: think off");
    ck(ai_request_body("m", "a\"b", false) ==
           "{\"model\":\"m\",\"prompt\":\"a\\\"b\",\"stream\":false}",
       "ai_request_body: retry without think, escaped");
    ck(strip_think("<think>\nreasoning\n</think>\n\nhello") == "hello",
       "strip_think: drops think block");
    ck(strip_think("hello") == "hello", "strip_think: plain text unchanged");
    ck(strip_think("<think>never closed") == "", "strip_think: unclosed means all reasoning");
    ck(ai_error_text(12029, 0, "", "http://127.0.0.1:11434", "m").find("Ollama") !=
           std::string::npos,
       "ai_error_text: cannot connect mentions Ollama");
    ck(ai_error_text(12002, 0, "", "e", "m").find("超时") != std::string::npos,
       "ai_error_text: timeout");
    ck(ai_error_text(0, 404, "model 'x' not found", "e", "qwen3.5:9b").find("ollama pull qwen3.5:9b") !=
           std::string::npos,
       "ai_error_text: missing model tells how to pull");

    // 英文补全（1.1.46）：词表排序、前缀匹配、词频名次、大小写跟随
    {
        std::vector<std::pair<std::string, uint32_t>> dict = {
            {"git", 1}, {"gitee", 2}, {"github", 0}, {"githubcom", 4}, {"gizmo", 3},
        };
        std::sort(dict.begin(), dict.end(),
                  [](const std::pair<std::string, uint32_t>& a,
                     const std::pair<std::string, uint32_t>& b) { return a.first < b.first; });
        std::vector<std::string> c = prefix_words(dict, "git", 6);
        ck(c.size() == 3, "prefix_words: git 命中 3 个（不含原样）");
        ck(c.size() == 3 && c[0] == "github" && c[1] == "gitee" && c[2] == "githubcom",
           "prefix_words: 按词频名次排");
        ck(prefix_words(dict, "gi", 2).size() == 2 && prefix_words(dict, "gi", 2)[0] == "github",
           "prefix_words: limit 取最常用的");
        // gir 之类不在表里；`gi` 命中 git/gitee/github/githubcom/gizmo 五个
        ck(prefix_words(dict, "gi", 99).size() == 5, "prefix_words: gi 命中 5 个");
        ck(prefix_words(dict, "zz", 6).empty(), "prefix_words: 没命中给空");
        ck(prefix_words(dict, "g", 6).empty(), "prefix_words: 单字母不补全");
        ck(match_case("Git", "github") == "Github", "match_case: 首字母大写跟随");
        ck(match_case("GIT", "github") == "GITHUB", "match_case: 全大写跟随");
        ck(match_case("git", "github") == "github", "match_case: 小写原样");
    }

    // 联想（1.1.52）：什么样的词对值得记、取前几个、同次数怎么排
    {
        ck(pair_ok("健全", "考核"), "pair_ok: two 2-char words ok");
        ck(!pair_ok("健", "考核"), "pair_ok: single char prev rejected");
        ck(!pair_ok("健全", "一二三四五六七八九"), "pair_ok: too long rejected");
        ck(!pair_ok("code", "考核"), "pair_ok: ascii rejected");
        ck(!pair_ok("健全", "健全"), "pair_ok: same word rejected");
        std::map<std::string, int> m = {{"考核", 3}, {"考查", 1}, {"考核体系", 3}};
        std::vector<std::string> t = top_next(m, 2);
        ck(t.size() == 2, "top_next: respects limit");
        ck(t.size() == 2 && t[0] == "考核" && t[1] == "考核体系",
           "top_next: count desc, then stable by text");
        ck(top_next(m, 9).size() == 3, "top_next: all when limit bigger");
    }

    // 个人语料（M2）：键归一化 + 顶到最前 + 去重
    {
        ck(corpus_key("chi tu") == "chitu", "corpus_key: strips spaces");
        ck(corpus_key("xi'an") == "xian", "corpus_key: strips boundary marks");
        std::vector<std::string> out = {"词", "次", "赤兔", "翅"};
        std::vector<std::pair<std::string, int>> words = {{"赤兔", 9}};
        corpus_apply(out, words);
        ck(!out.empty() && out[0] == "赤兔", "corpus_apply: hit goes first");
        ck(std::count(out.begin(), out.end(), "赤兔") == 1, "corpus_apply: no duplicate");
        std::vector<std::pair<std::string, int>> two = {{"赤兔", 9}, {"翅", 3}};
        std::vector<std::string> out2 = {"词", "翅"};
        corpus_apply(out2, two);
        ck(out2.size() >= 2 && out2[0] == "赤兔" && out2[1] == "翅",
           "corpus_apply: keeps weight order");
        std::vector<std::string> out3;
        corpus_apply(out3, {});
        ck(out3.empty(), "corpus_apply: empty words is a no-op");
    }

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
    load_en_words();
    load_pairs();
    load_corpus();

    // 词库巡检命令（不需要 librime、不连管道，可以在引擎运行时跑）
    for (int i = 1; i < argc; i++) {
        std::string arg = argv[i];
        if (arg == "--review-dict") return dict_command("review", "", "");
        if (arg == "--prune-single") return dict_command("prune-single", "", "");
        if (arg == "--review-corpus") {
            printf("corpus: %s\n", g_corpus_path.c_str());
            size_t words = 0;
            for (const auto& kv : g_corpus) words += kv.second.size();
            printf("codes=%zu entries=%zu\n", g_corpus.size(), words);
            size_t shown = 0;
            for (const auto& kv : g_corpus) {
                if (shown++ >= 10) break;
                printf("  %-20s", kv.first.c_str());
                for (const auto& w : kv.second) printf(" %s(%d)", w.first.c_str(), w.second);
                printf("\n");
            }
            return 0;
        }
        if (arg == "--forget") {
            // argv 里带中文时是 ANSI（中文系统上是 GBK）编码，内部一律 UTF-8，
            // 不转的话 `--forget wojude 我局的` 永远匹配不上。
            std::string code = (i + 1 < argc) ? argv[i + 1] : "";
            std::string word = (i + 2 < argc) ? argv_to_utf8(argv[i + 2]) : "";
            return dict_command("forget", code, word);
        }
    }


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

    // 第二个会话：不带模糊音补丁的方案，用来做「精确优先」排序。
    // 选不上（老引擎目录里没有这个 schema）就退化成单会话，功能不受影响。
    g_exact = create_session();
    if (g_exact && select_schema(g_exact, "luna_pinyin_exact")) {
        logf("exact session=%u ready", (unsigned)g_exact);
    } else {
        logf("exact schema unavailable -> exact-first disabled");
        g_exact = 0;
    }
    // 模糊音补丁打到 user/luna_pinyin_simp.custom.yaml。没有它（或全被注释掉）
    // 说明用户没开模糊音，两个会话结果一样，没必要多跑一遍。
    {
        std::string custom = user_dir + "\\luna_pinyin_simp.custom.yaml";
        FILE* f = fopen(custom.c_str(), "rb");
        if (f) {
            std::string content;
            char buf[4096];
            size_t n;
            while ((n = fread(buf, 1, sizeof(buf), f)) > 0) content.append(buf, n);
            fclose(f);
            g_fuzzy_on = content.find("pinyin:/") != std::string::npos;
        }
        logf("fuzzy_on=%d", (int)g_fuzzy_on);
    }
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
