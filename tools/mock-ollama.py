r"""A tiny fake Ollama endpoint for inklink's AI plumbing (P4a/P4b).

Why it exists: the AI path in the engine is just "POST /api/generate, read the
`response` field". That can be verified without a 7B model, on any machine, and
deterministically in CI.

    python tools/mock-ollama.py 11435

Then point inklink at it (settings window, or the registry):

    HKCU\Software\inklink\Config
      AI_Enable   DWORD 1
      AI_Endpoint SZ    http://127.0.0.1:11435
      AI_Model    SZ    mock

The answer is chosen from the prompt (the engine prefixes it with the action), so
translate/rewrite/continue can be told apart. Every request is appended to
%TEMP%\inklink-mock-ollama.log as UTF-8, which is how you confirm *what text the
DLL actually sent* (e.g. that "hello" was selected, not the whole document).
"""

import json
import os
import sys
import tempfile
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.path.join(tempfile.gettempdir(), "inklink-mock-ollama.log")

ANSWERS = (
    ("翻译", "你好"),
    ("改写", "你好呀"),
    ("续写", "，欢迎光临。"),
)


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(n).decode("utf-8", "replace")
        try:
            prompt = json.loads(raw).get("prompt", "")
        except Exception:
            prompt = ""
        # 400ms is enough to make "the UI must not block" observable by hand,
        # and too short to make the smoke test slow.
        time.sleep(0.4)
        answer = next((a for key, a in ANSWERS if key in prompt), "（未知动作）")
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(f"{time.strftime('%H:%M:%S')} prompt={prompt!r} -> {answer!r}\n")
        body = json.dumps({"response": answer, "done": True}, ensure_ascii=False).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        pass


def main() -> int:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 11435
    print(f"mock ollama on 127.0.0.1:{port}, log: {LOG}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
