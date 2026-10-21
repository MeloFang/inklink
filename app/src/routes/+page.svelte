<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  
  import { onMount } from "svelte";

  type Cand = {
    text: string;
    pinyin: string;
    freq: number;
    score: number;
    source: string;
  };

  let input = $state("");
  let cands = $state<Cand[]>([]);
  let sel = $state(0);
  let costUs = $state(0);




  async function refresh(q: string) {
    if (!q.trim()) {
      cands = [];
      costUs = 0;
      sel = 0;
      return;
    }
    const t0 = performance.now();
    cands = await invoke<Cand[]>("search", { input: q, limit: 9 });
    costUs = Math.round((performance.now() - t0) * 1000);
    sel = 0;
  }

  function move(delta: number) {
    if (!cands.length) return;
    sel = (sel + delta + cands.length) % cands.length;
  }

  function hide() {
    // 交给 Rust 处理：它要同步"可见"标记，否则下次热键会变成再隐藏一次
    void invoke("hide_panel");
  }

  /** 上屏：把选中的候选写进唤起面板前的那个窗口。 */
  async function commit(index = sel) {
    const c = cands[index];
    if (!c) return;
    const text = c.text;
    input = "";
    cands = [];
    sel = 0;
    try {
      // keepOpen: 上屏后把焦点收回面板，可以接着打下个词
      await invoke("commit_text", { text, keepOpen: true });
    } catch (err) {
      console.error("上屏失败", err);
    }
  }

  /** 非拼音字符（数字、标点）直接透传给目标应用。 */
  async function forward(ch: string) {
    try {
      await invoke("commit_text", { text: ch, keepOpen: true });
    } catch (err) {
      console.error("透传失败", err);
    }
  }

  function type(ch: string) {
    input += ch;
    void refresh(input);
  }

  function backspace() {
    if (!input) return;
    input = input.slice(0, -1);
    void refresh(input);
  }

  // 面板不抢焦点（WS_EX_NOACTIVATE），按键由 Rust 的键盘钩子采集后以事件发过来。
  // 这里只负责按语义分发：字母拼输入、数字选词、其余透传。
  function onKey(key: string) {
    if (key === "Escape") {
      hide();
      return;
    }

    switch (key) {
      case "Backspace":
        backspace();
        return;
      case "ArrowDown":
        move(1);
        return;
      case "ArrowUp":
        move(-1);
        return;
      case "Enter":
      case "Tab":
      case " ":
        // 空格 = 选第一个（当前高亮）候选上屏，和常见输入法一致
        void commit();
        return;
    }

    if (key.length === 1) {
      const ch = key.toLowerCase();
      // 字母、撇号（音节分隔）
      if (/^[a-z'’]$/.test(ch)) {
        type(ch);
        return;
      }
      // 数字键：有候选时选词，否则当作数字透传给目标应用
      if (/^[0-9]$/.test(ch)) {
        if (cands.length && ch !== "0") {
          const i = Number(ch) - 1;
          if (i < cands.length) void commit(i);
        } else if (!input) {
          void forward(ch);
        }
        return;
      }
      // 其他可打印字符（标点等）：不在拼音里，直接透传
      if (!input) {
        void forward(key);
      }
    }
  }

  onMount(() => {
    void listen<number>("lexicon-ready", () => {
      if (input.trim()) void refresh(input);
    });

    // 按键来自 Rust 的键盘钩子（面板不抢焦点）
    void listen<string>("key", (e) => onKey(e.payload));

    // 面板每次唤出都清空上一轮的输入
    void listen("panel-shown", () => {
      input = "";
      cands = [];
      sel = 0;
      costUs = 0;
    });
  });
</script>

<main class="panel">
  <header>
    <span class="brand">inklink</span>
    <span class="hint">Esc 隐藏 · ↑↓ 选择 · 空格/回车 上屏</span>
  </header>

  <div class="box" role="textbox" aria-label="拼音输入">
    {#if input}
      <span class="typed">{input}</span><span class="caret"></span>
    {:else}
      <span class="placeholder">输入拼音，例如 xiangmu / zgrm</span><span class="caret"></span>
    {/if}
  </div>

  <ul>
    {#each cands as c, i}
      <li>
        <button type="button" class="row" class:active={i === sel} onclick={() => void commit(i)}>
          <span class="idx">{i + 1}</span>
          <span class="text">{c.text}</span>
          <span class="py">{c.pinyin}</span>
          <span class="meta">{c.source} · {c.freq}</span>
        </button>
      </li>
    {:else}
      <li class="empty">{input.trim() ? "无候选" : " "}</li>
    {/each}
  </ul>

  <footer>{costUs ? `${costUs} µs` : "—"}</footer>
</main>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: transparent;
    overflow: hidden;
  }

  .panel {
    box-sizing: border-box;
    height: 100vh;
    padding: 14px 16px 10px;
    border-radius: 14px;
    background: #1e1f22;
    border: 1px solid #ffffff1a;
    box-shadow: 0 12px 40px #00000066;
    color: #eceff4;
    font-family: "Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif;
    display: flex;
    flex-direction: column;
    gap: 10px;
    user-select: none;
  }

  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }

  .brand {
    font-size: 14px;
    letter-spacing: 0.4px;
    font-weight: 600;
  }

  .hint {
    font-size: 11px;
    color: #7b8494;
  }

  .box {
    box-sizing: border-box;
    min-height: 44px;
    padding: 10px 12px;
    border-radius: 9px;
    border: 1px solid #ffffff1f;
    background: #00000038;
    font-size: 18px;
    letter-spacing: 0.6px;
    display: flex;
    align-items: center;
    outline: none;
  }

  .box:focus {
    border-color: #3b82f6;
  }

  .typed {
    color: #eceff4;
  }

  .placeholder {
    color: #6b7280;
    font-size: 15px;
  }

  .caret {
    width: 2px;
    height: 20px;
    margin-left: 1px;
    background: #3b82f6;
    animation: blink 1.1s step-end infinite;
  }

  @keyframes blink {
    50% {
      opacity: 0;
    }
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
    flex: 1;
    overflow: hidden;
  }

  li {
    display: block;
  }

  .row {
    box-sizing: border-box;
    width: 100%;
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 6px 8px;
    border: 0;
    border-radius: 7px;
    background: transparent;
    color: inherit;
    font: inherit;
    font-size: 15px;
    text-align: left;
    cursor: default;
  }

  .row.active {
    background: #3b82f62e;
  }

  .idx {
    width: 14px;
    color: #6b7280;
    font-size: 11px;
  }

  .text {
    font-weight: 500;
  }

  .py {
    color: #8b93a1;
    font-size: 12px;
  }

  .meta {
    margin-left: auto;
    color: #6b7280;
    font-size: 11px;
  }

  .empty {
    color: #6b7280;
    font-size: 13px;
  }

  footer {
    text-align: right;
    color: #4b5563;
    font-size: 11px;
  }
</style>
