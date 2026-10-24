// 管道测试客户端：连 \\.\pipe\inklink-engine，发一个拼音，打印候选。
#include <windows.h>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

static bool read_exact(HANDLE p, void* buf, DWORD n) {
    DWORD got = 0;
    BYTE* p8 = (BYTE*)buf;
    while (got < n) {
        DWORD r = 0;
        if (!ReadFile(p, p8 + got, n - got, &r, nullptr) || r == 0) return false;
        got += r;
    }
    return true;
}

static bool write_all(HANDLE p, const void* buf, DWORD n) {
    DWORD w = 0;
    return WriteFile(p, buf, n, &w, nullptr) && w == n;
}

int main(int argc, char** argv) {
    if (argc < 2) {
        printf("usage: client <pinyin>\n");
        return 1;
    }
    HANDLE p = CreateFileW(L"\\\\.\\pipe\\inklink-engine",
                           GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                           OPEN_EXISTING, 0, nullptr);
    if (p == INVALID_HANDLE_VALUE) {
        printf("connect failed: %lu\n", GetLastError());
        return 2;
    }
    const char* code = argv[1];
    uint32_t len = (uint32_t)strlen(code);
    if (!write_all(p, &len, 4) || !write_all(p, code, len)) {
        printf("write failed\n");
        CloseHandle(p);
        return 3;
    }
    uint32_t count = 0;
    if (!read_exact(p, &count, 4)) {
        printf("read count failed\n");
        CloseHandle(p);
        return 4;
    }
    printf("count=%u\n", count);
    for (uint32_t i = 0; i < count; i++) {
        uint16_t l = 0;
        if (!read_exact(p, &l, 2)) { printf("read len failed at %u\n", i); break; }
        std::string s(l, '\0');
        if (l && !read_exact(p, &s[0], l)) { printf("read text failed at %u\n", i); break; }
        printf("  %u. %s\n", i + 1, s.c_str());
        if (i >= 9) { printf("  ...\n"); break; }
    }
    CloseHandle(p);
    return 0;
}
