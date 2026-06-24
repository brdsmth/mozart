<script lang="ts">
  import TaskList from "$lib/TaskList.svelte";
  import ChatPane from "$lib/ChatPane.svelte";
</script>

<div class="drag-region" data-tauri-drag-region></div>
<main class="app">
  <TaskList />
  <ChatPane />
</main>

<style>
  :global(:root) {
    --color-accent: #3a6df0;
    --color-accent-soft: #e3e8ff;
    --color-border: #ddd;
    --color-surface-hover: #f0f0f0;
    --color-text-muted: #888;
    --color-text-faint: #999;
    --color-error-bg: #fde8e8;
    --color-error-text: #a33;

    --radius-sm: 6px;
    --radius-md: 10px;

    --space-xs: 0.3rem;
    --space-sm: 0.5rem;
    --space-md: 0.75rem;
    --space-lg: 1rem;
  }

  :global(body) {
    margin: 0;
    font-family: "Avenir Next", -apple-system, BlinkMacSystemFont, sans-serif;
  }

  .app {
    display: flex;
    height: 100vh;
  }

  .drag-region {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    height: 28px;
    z-index: 1000;
  }

  :global(.sr-only) {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
  }
</style>
