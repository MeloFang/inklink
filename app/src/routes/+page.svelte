<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
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
  let boxEl = $state<HTMLDivElement>();

  const win = getCurrentWindow();

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

  function type(ch: string) {
    input += ch;
    void refresh(input);
  }

  function backspace() {
    if (!input) return;
    input = input.slice(0, -1);
    void refresh(input);
  }

  // 不用 <input>：可编辑元素会激活系统输入法，把它摘掉又会引发崩溃。
  // 这里自己接管按键，界面上只显示我们拼出来的字符串。
  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      hide();
      e.preventDefault();
      return;
    }
    if (e.ctrlKey || e.altKey || e.metaKey) return;

    switch (e.key) {
      case "Backspace":
        backspace();
        e.preventDefault();
        return;
      case "ArrowDown":
        move(1);
        e.preventDefault();
        return;
      case "ArrowUp":
        move(-1);
        e.preventDefault();
        return;
      case "Enter":
      case "Tab":
        e.preventDefault();
        return;
    }

    if (e.key.length === 1) {
      const ch = e.key.toLowerCase();
      // 字母、撇号（音节分隔）、空格
      if (/^[a-z'’ ]$/.test(ch)) {
        type(ch);
        e.preventDefault();
        return;
      }
      // 数字键选候选
      if (/^[1-9]$/.test(ch) && cands.length) {
        const i = Number(ch) - 1;
        if (i < cands.length) sel = i;
        e.preventDefault();
      }
    }
  }

  onMount(() => {
    void listen<number>("lexicon-ready", () => {
      if (input.trim()) void refresh(input);
    });

    void win.onFocusChanged(({ payload: focused }) => {
      if (focused) {
        input = "";
        cands = [];
        sel = 0;
        costUs = 0;
        boxEl?.focus();
      }
    });

    boxEl?.focus();
  });
</script>

<svelte:window on:keydown={onKey} />

<main class="panel">
  <header>
    <span class="brand">inklink</span>
    <span class="hint">Esc 隐藏 · ↑↓ 选择 · 数字键直选</span>
  </header>

  <div class="box" bind:this={boxEl} tabindex="0" role="textbox" aria-label="拼音输入">
    {#if input}
      <span class="typed">{input}</span><span class="caret"></span>
    {:else}
      <span class="placeholder">输入拼音，例如 xiangmu / zgrm</span><span class="caret"></span>
    {/if}
  </div>

  <ul>
    {#each cands as c, i}
      <li>
        <button type="button" class="row" class:active={i === sel} onclick={() => (sel = i)}>
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
