<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, tick } from "svelte";

  type Cand = {
    text: string;
    pinyin: string;
    freq: number;
    score: number;
    source: string;
  };

  const PAGE = 5;
  const BAR_HEIGHT = 78;

  let input = $state("");
  let all = $state<Cand[]>([]);
  let sel = $state(0); // 当前页内的下标
  let page = $state(0);

  let barEl = $state<HTMLElement>();

  let pageCands = $derived(all.slice(page * PAGE, page * PAGE + PAGE));
  let pageCount = $derived(Math.max(1, Math.ceil(all.length / PAGE)));

  async function refresh(q: string) {
    if (!q.trim()) {
      all = [];
      page = 0;
      sel = 0;
      void resize();
      return;
    }
    const t0 = performance.now();
    all = await invoke<Cand[]>("search", { input: q, limit: 50 });
    page = 0;
    sel = 0;
    void resize();
  }

  /** 让窗口宽度贴合内容，像主流输入法的候选条 */
  async function resize() {
    await tick();
    if (!barEl) return;
    const width = Math.ceil(Math.max(220, barEl.scrollWidth + 4));
    void invoke("resize_panel", { width, height: BAR_HEIGHT });
  }

  function move(delta: number) {
    if (!pageCands.length) return;
    sel = (sel + delta + pageCands.length) % pageCands.length;
  }

  function turnPage(delta: number) {
    if (pageCount <= 1) return;
    page = (page + delta + pageCount) % pageCount;
    sel = 0;
    void resize();
  }

  function hide() {
    void invoke("hide_panel");
  }

  function commit(index = sel) {
    const c = pageCands[index];
    if (!c) return;
    const text = c.text;
    input = "";
    all = [];
    page = 0;
    sel = 0;
    void resize();
    invoke("commit_text", { text, keepOpen: true }).catch((e) => console.error("上屏失败", e));
  }

  function forward(ch: string) {
    invoke("commit_text", { text: ch, keepOpen: true }).catch((e) =>
      console.error("透传失败", e),
    );
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
  function onKey(key: string) {
    if (key === "Escape") {
      hide();
      return;
    }

    switch (key) {
      case "Backspace":
        backspace();
        return;
      case "ArrowLeft":
        move(-1);
        return;
      case "ArrowRight":
        move(1);
        return;
      case "ArrowUp":
      case "-":
      case ",":
        turnPage(-1);
        return;
      case "ArrowDown":
      case "=":
      case ".":
        turnPage(1);
        return;
      case "Enter":
      case "Tab":
      case " ":
        commit();
        return;
    }

    if (key.length === 1) {
      const ch = key.toLowerCase();
      if (/^[a-z'’]$/.test(ch)) {
        type(ch);
        return;
      }
      if (/^[0-9]$/.test(ch)) {
        const i = Number(ch) - 1;
        if (pageCands.length && ch !== "0") {
          if (i < pageCands.length) commit(i);
        } else if (!input) {
          forward(ch);
        }
        return;
      }
      if (!input) forward(key);
    }
  }

  onMount(() => {
    void listen<number>("lexicon-ready", () => {
      if (input.trim()) void refresh(input);
    });

    void listen<string>("key", (e) => onKey(e.payload));

    void listen("panel-shown", () => {
      input = "";
      all = [];
      page = 0;
      sel = 0;
      void resize();
    });

    void resize();
  });
</script>

<main class="bar" bind:this={barEl}>
  <div class="code">
    <span class:ph={!input}>{input || "拼音"}</span>
    <span class="caret"></span>
    {#if pageCount > 1}
      <span class="page">{page + 1}/{pageCount}</span>
    {/if}
  </div>

  <div class="cands">
    {#each pageCands as c, i}
      <button type="button" class="cand" class:active={i === sel} onclick={() => commit(i)}>
        <span class="idx">{i + 1}</span>
        <span class="txt">{c.text}</span>
      </button>
    {/each}
  </div>


</main>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: transparent;
    overflow: hidden;
  }

  .bar {
    box-sizing: border-box;
    display: inline-flex;
    align-items: stretch;
    flex-direction: column;
    gap: 2px;
    padding: 6px 10px 8px;
    border-radius: 10px;
    background: #f7f8fa;
    border: 1px solid #d7dae0;
    box-shadow: 0 8px 24px #00000024;
    color: #1f2329;
    font-family: "Microsoft YaHei UI", "Segoe UI", system-ui, sans-serif;
    user-select: none;
    white-space: nowrap;
    width: max-content;
    min-width: 220px;
  }

  .code {
    display: flex;
    align-items: center;
    font-size: 15px;
    line-height: 20px;
    color: #3b82f6;
    letter-spacing: 0.4px;
    min-height: 20px;
  }

  .code .ph {
    color: #9aa3af;
    font-size: 13px;
  }

  .caret {
    width: 1.5px;
    height: 15px;
    margin-left: 2px;
    background: #3b82f6;
    animation: blink 1.1s step-end infinite;
  }

  @keyframes blink {
    50% {
      opacity: 0;
    }
  }

  .cands {
    display: flex;
    align-items: center;
    gap: 2px;
    min-height: 30px;
  }

  .cand {
    display: flex;
    align-items: baseline;
    gap: 2px;
    padding: 3px 6px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: inherit;
    font: inherit;
    font-size: 17px;
    line-height: 22px;
    white-space: nowrap;
    cursor: default;
  }

  .cand.active {
    background: #3b82f6;
    color: #fff;
  }

  .idx {
    font-size: 10px;
    color: #98a2b3;
    margin-right: 1px;
  }

  .cand.active .idx {
    color: #dbeafe;
  }

  .txt {
    font-weight: 500;
  }

  .page {
    margin-left: auto;
    padding-left: 14px;
    font-size: 10px;
    color: #98a2b3;
  }
</style>
