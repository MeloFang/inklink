"""极简 minidump 解析器：只为回答「崩溃到底是谁调进 user32 的」。

用法: python parse-dump.py <dmp 路径>

输出:
  1. 异常记录（代码 / 地址 / 参数，c0000005 的 ExceptionInformation[1] 就是坏指针）
  2. 模块表（基址 + 大小 + 名字），用来把地址映射回 module+offset
  3. 出问题的线程的 RIP/RSP，以及栈上所有能映射到模块的返回地址（粗栈）
"""

import struct
import sys

MINIDUMP_STREAM_THREAD_LIST = 3
MINIDUMP_STREAM_MODULE_LIST = 4
MINIDUMP_STREAM_EXCEPTION = 6
MINIDUMP_STREAM_MEMORY64_LIST = 9
MINIDUMP_STREAM_MEMORY_LIST = 5

# CONTEXT (x64) 里我们关心的偏移
CTX_RSP = 0x98
CTX_RBP = 0xA0
CTX_RIP = 0xF8


class Dump:
    def __init__(self, path):
        with open(path, "rb") as f:
            self.buf = f.read()
        sig, _ver, nstreams, dir_rva = struct.unpack_from("<IIII", self.buf, 0)
        if sig != 0x504D444D:
            raise SystemExit("不是 minidump 文件")
        self.streams = {}
        for i in range(nstreams):
            stype, size, rva = struct.unpack_from("<III", self.buf, dir_rva + i * 12)
            self.streams.setdefault(stype, []).append((size, rva))
        self.mem_ranges = self._read_memory64()
        self.modules = self._read_modules()

    # ---- 内存段：虚拟地址 -> dump 文件偏移 ----
    def _read_memory64(self):
        if MINIDUMP_STREAM_MEMORY64_LIST not in self.streams:
            return []
        _size, rva = self.streams[MINIDUMP_STREAM_MEMORY64_LIST][0]
        n, base_rva = struct.unpack_from("<QQ", self.buf, rva)
        ent = rva + 16
        ranges = []
        cur = base_rva
        for _ in range(n):
            start, length = struct.unpack_from("<QQ", self.buf, ent)
            ent += 16
            ranges.append((start, length, cur))
            cur += length
        return ranges

    def read_mem(self, va, n):
        for start, length, off in self.mem_ranges:
            if start <= va and va + n <= start + length:
                p = off + (va - start)
                return self.buf[p:p + n]
        return None

    # ---- 模块表 ----
    def _read_modules(self):
        mods = []
        if MINIDUMP_STREAM_MODULE_LIST not in self.streams:
            return mods
        _size, rva = self.streams[MINIDUMP_STREAM_MODULE_LIST][0]
        (n,) = struct.unpack_from("<I", self.buf, rva)
        p = rva + 4
        for _ in range(n):
            base = struct.unpack_from("<Q", self.buf, p)[0]
            size = struct.unpack_from("<I", self.buf, p + 8)[0]
            name_rva = struct.unpack_from("<I", self.buf, p + 20)[0]
            mods.append((base, size, self._read_string(name_rva)))
            p += 108
        mods.sort()
        return mods

    def _read_string(self, rva):
        (length,) = struct.unpack_from("<I", self.buf, rva)
        raw = self.buf[rva + 4:rva + 4 + length]
        return raw.decode("utf-16-le", "replace")

    def which(self, addr):
        for base, size, name in self.modules:
            if base <= addr < base + size:
                return "%s+0x%x" % (name.rsplit("\\", 1)[-1], addr - base)
        return None

    # ---- 异常 ----
    def exception(self):
        if MINIDUMP_STREAM_EXCEPTION not in self.streams:
            return None
        _size, rva = self.streams[MINIDUMP_STREAM_EXCEPTION][0]
        tid = struct.unpack_from("<I", self.buf, rva)[0]
        code, flags = struct.unpack_from("<II", self.buf, rva + 8)
        addr = struct.unpack_from("<Q", self.buf, rva + 24)[0]
        nparams = struct.unpack_from("<I", self.buf, rva + 32)[0]
        params = [struct.unpack_from("<Q", self.buf, rva + 40 + i * 8)[0] for i in range(min(nparams, 4))]
        ctx_size, ctx_rva = struct.unpack_from("<II", self.buf, rva + 160)
        return dict(tid=tid, code=code, flags=flags, addr=addr, params=params,
                    ctx_rva=ctx_rva, ctx_size=ctx_size)

    def regs(self, ctx_rva):
        out = {}
        for name, off in (("rip", CTX_RIP), ("rsp", CTX_RSP), ("rbp", CTX_RBP)):
            out[name] = struct.unpack_from("<Q", self.buf, ctx_rva + off)[0]
        return out


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else r"C:\Users\Administrator\AppData\Local\CrashDumps\DeepSeek Harness.exe.11468.dmp"
    d = Dump(path)
    ex = d.exception()
    print("dump:", path)
    if not ex:
        print("没有异常流")
        return
    print("线程: %d" % ex["tid"])
    print("异常代码: 0x%08X  标志: 0x%X" % (ex["code"], ex["flags"]))
    print("异常地址: 0x%X  -> %s" % (ex["addr"], d.which(ex["addr"]) or "?"))
    for i, v in enumerate(ex["params"]):
        extra = ""
        if i == 1 and ex["code"] == 0xC0000005:
            extra = "  <- 访问的坏地址  %s" % (d.which(v) or "无模块")
        print("  参数[%d] = 0x%X%s" % (i, v, extra))
    regs = d.regs(ex["ctx_rva"])
    print("RIP=0x%X (%s)  RSP=0x%X  RBP=0x%X" % (regs["rip"], d.which(regs["rip"]) or "?", regs["rsp"], regs["rbp"]))

    total = sum(r[1] for r in d.mem_ranges)
    print("\n内存段: %d 段 / %.1f MB；模块 %d 个" % (len(d.mem_ranges), total / 1048576.0, len(d.modules)))
    if d.mem_ranges:
        near = [r for r in d.mem_ranges if r[0] <= regs["rsp"] < r[0] + r[1]]
        print("RSP 所在段: %s" % ("有" if near else "无"))
        print("最低/最高段: 0x%X .. 0x%X" % (d.mem_ranges[0][0], d.mem_ranges[-1][0] + d.mem_ranges[-1][1]))

    print("\n=== 栈上的返回地址（RSP 起 96 个 qword，只列能映射到模块的） ===")
    stack = d.read_mem(regs["rsp"], 96 * 8)
    if not stack:
        print("(栈内存不在 dump 里)")
        return
    for i in range(0, len(stack), 8):
        v = struct.unpack_from("<Q", stack, i)[0]
        w = d.which(v)
        if w:
            print("  [rsp+0x%03x] 0x%X  %s" % (i, v, w))


if __name__ == "__main__":
    main()
