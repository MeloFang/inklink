<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";

  // M0 步骤 0：只有一个静态骨架，验证构建链通了。
  // 热键唤起、候选列表、上屏在步骤 5–6 实现。
  async function close() {
    await getCurrentWindow().hide();
  }
</script>

<svelte:window on:keydown={(e) => e.key === "Escape" && close()} />

<main class="panel">
  <header>
    <span class="brand">inklink</span>
    <span class="stage">M0 · 骨架</span>
  </header>

  <div class="hint">
    <p>面板还没接引擎。</p>
    <p class="dim">按 Esc 隐藏窗口。</p>
  </div>
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
    padding: 18px 20px;
    border-radius: 14px;
    background: #1e1f22f2;
    border: 1px solid #ffffff1a;
    box-shadow: 0 12px 40px #00000066;
    color: #eceff4;
    font-family: "Segoe UI", "Microsoft YaHei UI", system-ui, sans-serif;
    display: flex;
    flex-direction: column;
    gap: 14px;
    user-select: none;
  }

  header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
  }

  .brand {
    font-size: 15px;
    letter-spacing: 0.4px;
    font-weight: 600;
  }

  .stage {
    font-size: 11px;
    color: #8b93a1;
  }

  .hint {
    margin-top: auto;
    font-size: 13px;
    line-height: 1.9;
  }

  .hint p {
    margin: 0;
  }

  .dim {
    color: #7b8494;
    font-size: 12px;
  }
</style>
