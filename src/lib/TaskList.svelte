<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { appState, refreshTasks, selectTask, createTask } from "./state.svelte";

  let newName = $state("");
  let newWorkingDir = $state("");
  let allowRealExecution = $state(false);
  let creating = $state(false);

  onMount(refreshTasks);

  function basename(path: string): string {
    return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
  }

  async function pickWorkingDir() {
    const selected = await open({ directory: true, multiple: false });
    if (!selected || Array.isArray(selected)) return;
    newWorkingDir = selected;
    if (!newName.trim()) {
      newName = basename(selected);
    }
  }

  async function handleCreate(event: Event) {
    event.preventDefault();
    if (!newName.trim() || !newWorkingDir.trim()) return;
    creating = true;
    try {
      await createTask(
        newName,
        newWorkingDir,
        allowRealExecution ? "bypassPermissions" : "plan",
      );
      newName = "";
      newWorkingDir = "";
      allowRealExecution = false;
    } finally {
      creating = false;
    }
  }
</script>

<aside class="task-list">
  <h2>Sessions</h2>
  <ul>
    {#each appState.tasks as task (task.id)}
      <li>
        <button
          class="task"
          class:selected={task.id === appState.selectedTaskId}
          onclick={() => selectTask(task.id)}
        >
          <span class="name">{task.name}</span>
          <span class="meta">
            <span class="status status-{task.status}">
              <span class="status-dot"></span>{task.status}
            </span>
            {#if task.permission_mode !== "plan"}
              <span class="mode-badge" title="This session's agent can edit files and run commands">live</span>
            {/if}
          </span>
        </button>
      </li>
    {/each}
  </ul>

  <form onsubmit={handleCreate}>
    <label class="sr-only" for="new-session-name">Session name</label>
    <input id="new-session-name" placeholder="Session name" bind:value={newName} />

    <button type="button" class="dir-picker" onclick={pickWorkingDir}>
      {newWorkingDir ? basename(newWorkingDir) : "Choose working directory…"}
    </button>
    {#if newWorkingDir}
      <span class="dir-path" title={newWorkingDir}>{newWorkingDir}</span>
    {/if}

    <label class="permission-toggle">
      <input type="checkbox" bind:checked={allowRealExecution} />
      Let agent edit files &amp; run commands
    </label>

    <button type="submit" disabled={creating || !newWorkingDir.trim()}>New Session</button>
  </form>
</aside>

<style>
  .task-list {
    width: 260px;
    border-right: 1px solid var(--color-border);
    display: flex;
    flex-direction: column;
    padding: var(--space-md);
    padding-top: 2rem;
    box-sizing: border-box;
  }

  h2 {
    font-size: 0.9rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--color-text-muted);
    margin: 0 0 var(--space-sm) 0;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    flex: 1;
    overflow-y: auto;
  }

  .task {
    width: 100%;
    display: flex;
    justify-content: space-between;
    background: none;
    border: none;
    padding: var(--space-sm);
    text-align: left;
    cursor: pointer;
    border-radius: var(--radius-sm);
  }

  .task:hover {
    background: var(--color-surface-hover);
  }

  .task.selected {
    background: var(--color-accent-soft);
  }

  .meta {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
  }

  .status {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    font-size: 0.75rem;
    color: var(--color-text-faint);
  }

  .mode-badge {
    font-size: 0.65rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    color: var(--color-error-text);
    border: 1px solid var(--color-error-text);
    border-radius: 4px;
    padding: 0 0.25rem;
  }

  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #bbb;
  }

  .status-running .status-dot {
    background: var(--color-accent);
    animation: pulse 1.2s ease-in-out infinite;
  }

  .status-error {
    color: var(--color-error-text);
  }

  .status-error .status-dot {
    background: var(--color-error-text);
  }

  .status-cancelled .status-dot {
    background: var(--color-text-muted);
  }

  @keyframes pulse {
    0%, 100% {
      opacity: 1;
    }
    50% {
      opacity: 0.35;
    }
  }

  form {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    margin-top: var(--space-md);
  }

  input {
    padding: 0.4rem;
    font-size: 0.85rem;
  }

  .permission-toggle {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.75rem;
    color: var(--color-text-muted);
    cursor: pointer;
  }

  .permission-toggle input {
    padding: 0;
    margin: 0;
  }

  .dir-picker {
    width: 100%;
    text-align: left;
    padding: 0.4rem;
    font-size: 0.85rem;
    font-family: inherit;
    border: 1px dashed var(--color-border);
    border-radius: var(--radius-sm);
    background: var(--color-surface-hover);
    color: var(--color-text-muted);
    cursor: pointer;
  }

  .dir-picker:hover {
    border-color: var(--color-accent);
    color: inherit;
  }

  .dir-path {
    display: block;
    font-size: 0.7rem;
    color: var(--color-text-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-top: -0.2rem;
  }
</style>
