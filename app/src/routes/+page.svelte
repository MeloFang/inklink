<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
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
  let inputEl = $state<HTMLInputElement>();

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

  function onInput(e: Event) {
    input = (e.target as HTMLInputElement).value;
    void refresh(input);
  }

  function move(delta: number) {
    if (!cands.length) return;
    sel = (sel + delta + cands.length) % cands.length;
  }

  function hide() {
    void win.hide();
  }

  function onKey(e: KeyboardEvent) {
    switch (e.key) {
      case "Escape":
        hide();
        e.preventDefault();
        break;
      case "ArrowDown":
        move(1);
        e.preventDefault();
        break;
      case "ArrowUp":
        move(-1);
        e.preventDefault();
        break;
      default:
        if (/^[1-9]$/.test(e.key)) {
          const i = Number(e.key) - 1;
          if (i < cands.length) {
            sel = i;
            e.preventDefault();
          }
        }
    }
  }

  onMount(() => {
    void win.onFocusChanged(({ payload: focused }) => {
      if (focused) {
        input = "";
        cands = [];
        sel = 0;
        costUs = 0;
        inputEl?.focus();
      }
    });
  });
</script>

<svelte:window on:keydown={onKey} />

<main class="panel">
  <header>
    <span class="brand">inklink</span>
    <span class="hint">Esc 隐藏 · ↑↓ 选择 · 数字键直选</span>
  </header>

  <input
    bind:this={inputEl}
    value={input}
    oninput={onInput}
    placeholder="输入拼音，例如 xiangmu / zgrm"
    spellcheck="false"
    autocomplete="off"
  />

  <ul>
    {#each cands as c, i}
      <li class:active={i === sel} onclick={() => (sel = i)}>
        <span class="idx">{i + 1}</span>
        <span class="text">{c.text}</span>
        <span class="py">{c.pinyin}</span>
        <span class="meta">{c.source} · {c.freq}</span>
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
    background: #1e1f22f2;
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

  input {
    box-sizing: border-box;
    width: 100%;
    padding: 10px 12px;
    border-radius: 9px;
    border: 1px solid #ffffff1f;
    background: #00000038;
    color: #eceff4;
    font-size: 18px;
    letter-spacing: 0.6px;
    outline: none;
  }

  input:focus {
    border-color: #3b82f6;
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
    flex: 1;
    overflow: hidden;
  }

  li {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 6px 8px;
    border-radius: 7px;
    font-size: 15px;
    cursor: default;
  }

  li.active {
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
