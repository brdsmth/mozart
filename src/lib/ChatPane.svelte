<script lang="ts">
  import { marked } from "marked";
  import DOMPurify from "dompurify";
  import {
    appState,
    currentMessages,
    isSending,
    isCancelling,
    isReconnecting,
    sendMessage,
    cancelMessage,
  } from "./state.svelte";
  import type { Message } from "./types";

  marked.setOptions({ breaks: true, gfm: true });

  function renderMarkdown(content: string): string {
    return DOMPurify.sanitize(marked.parse(content, { async: false }));
  }

  function isCancelledMessage(message: Message): boolean {
    return message.role === "system" && message.content === "Cancelled.";
  }

  function attachCommand(taskId: string): string {
    return `tmux attach -t mozart-${taskId}`;
  }

  let draft = $state("");
  let elapsedSeconds = $state(0);
  let copyLabel = $state("Copy");
  let copyAnnouncement = $state("");
  let copyResetTimeout: ReturnType<typeof setTimeout> | undefined;

  $effect(() => {
    // Reconciled-as-still-running tasks have no real client-side start time
    // to count up from (see `isReconnecting`), so the counter only runs for
    // turns this session actually started.
    if (!isSending() || isReconnecting()) {
      elapsedSeconds = 0;
      return;
    }
    const start = Date.now();
    elapsedSeconds = 0;
    const interval = setInterval(() => {
      elapsedSeconds = Math.floor((Date.now() - start) / 1000);
    }, 1000);
    return () => clearInterval(interval);
  });

  async function handleSend(event: Event) {
    event.preventDefault();
    const taskId = appState.selectedTaskId;
    if (!taskId || !draft.trim() || isSending()) return;
    const content = draft;
    draft = "";
    await sendMessage(taskId, content);
  }

  function handleCancel() {
    const taskId = appState.selectedTaskId;
    if (!taskId) return;
    cancelMessage(taskId);
  }

  async function handleCopyAttachCommand() {
    const taskId = appState.selectedTaskId;
    if (!taskId) return;
    const command = attachCommand(taskId);
    await navigator.clipboard.writeText(command);
    copyLabel = "Copied";
    copyAnnouncement = `Copied "${command}" to clipboard`;
    clearTimeout(copyResetTimeout);
    copyResetTimeout = setTimeout(() => {
      copyLabel = "Copy";
    }, 1500);
  }
</script>

{#if appState.selectedTaskId}
  <section class="chat">
    {#if currentMessages().length > 0}
      <div class="attach-affordance">
        <span class="attach-text">Attach: <code>{attachCommand(appState.selectedTaskId)}</code></span>
        <button type="button" class="copy-button" onclick={handleCopyAttachCommand} aria-label="Copy tmux attach command">
          {copyLabel}
        </button>
        <span class="sr-only" aria-live="polite">{copyAnnouncement}</span>
      </div>
    {/if}

    <div class="messages">
      {#each currentMessages() as message (message.id)}
        <div class="message {message.role}" class:cancelled={isCancelledMessage(message)}>
          <div class="role">{message.role}</div>
          <div class="content">{@html renderMarkdown(message.content)}</div>
        </div>
      {/each}
      {#if isSending()}
        <div class="message agent thinking">
          <span>{isReconnecting() ? "Reconnecting to a running agent…" : `thinking… ${elapsedSeconds}s`}</span>
          ·
          <button
            type="button"
            class="cancel-link"
            onclick={handleCancel}
            disabled={isCancelling()}
            aria-label="Cancel agent call"
          >
            {isCancelling() ? "Cancelling…" : "Cancel"}
          </button>
        </div>
      {/if}
    </div>

    <form onsubmit={handleSend}>
      <label class="sr-only" for="message-draft">Message the agent</label>
      <input
        id="message-draft"
        placeholder="Message the agent..."
        bind:value={draft}
        disabled={isSending()}
      />
      <button type="submit" disabled={isSending()}>Send</button>
    </form>
  </section>
{:else}
  <section class="empty">
    <p>Select or create a session to start chatting.</p>
  </section>
{/if}

<style>
  .chat,
  .empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    padding: var(--space-lg);
    padding-top: 2rem;
    box-sizing: border-box;
  }

  .empty {
    align-items: center;
    justify-content: center;
    color: var(--color-text-muted);
  }

  .attach-affordance {
    display: flex;
    align-items: center;
    gap: var(--space-sm);
    margin-bottom: var(--space-md);
    padding-bottom: var(--space-sm);
    border-bottom: 1px solid var(--color-border);
    font-size: 0.75rem;
    color: var(--color-text-faint);
  }

  .attach-text code {
    font-family: "SF Mono", Menlo, monospace;
    background: var(--color-surface-hover);
    padding: 0.1em 0.4em;
    border-radius: 4px;
  }

  .copy-button {
    font-size: 0.7rem;
    padding: 0.15rem 0.5rem;
    border: 1px solid var(--color-border);
    border-radius: var(--radius-sm);
    background: none;
    color: var(--color-text-muted);
    cursor: pointer;
  }

  .copy-button:hover {
    border-color: var(--color-accent);
    color: var(--color-accent);
  }

  .messages {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: var(--space-sm);
  }

  .message {
    max-width: 70%;
    padding: var(--space-sm) var(--space-md);
    border-radius: var(--radius-md);
    background: var(--color-surface-hover);
  }

  .message.user {
    align-self: flex-end;
    background: var(--color-accent-soft);
  }

  .message.agent {
    align-self: flex-start;
  }

  .message.system {
    align-self: center;
    background: var(--color-error-bg);
    color: var(--color-error-text);
    font-size: 0.85rem;
  }

  /* Cancelling is something the user chose to do, not a failure — keep it
     visually neutral instead of reusing the error styling above. */
  .message.system.cancelled {
    background: var(--color-surface-hover);
    color: var(--color-text-muted);
  }

  .message.thinking {
    display: flex;
    align-items: center;
    gap: var(--space-xs);
    color: var(--color-text-faint);
    font-style: italic;
  }

  .cancel-link {
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    font-style: italic;
    text-decoration: underline;
    color: var(--color-text-faint);
    cursor: pointer;
  }

  .cancel-link:hover:not(:disabled) {
    color: var(--color-accent);
  }

  .cancel-link:disabled {
    cursor: default;
    opacity: 0.7;
  }

  .role {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--color-text-faint);
  }

  .content {
    margin-top: 0.15rem;
  }

  .content :global(p) {
    margin: 0.3rem 0;
  }

  .content :global(p:first-child) {
    margin-top: 0;
  }

  .content :global(p:last-child) {
    margin-bottom: 0;
  }

  .content :global(pre) {
    background: rgba(0, 0, 0, 0.06);
    padding: var(--space-sm);
    border-radius: var(--radius-sm);
    overflow-x: auto;
    font-size: 0.8rem;
  }

  .content :global(code) {
    font-family: "SF Mono", Menlo, monospace;
    font-size: 0.85em;
  }

  .content :global(pre code) {
    background: none;
    padding: 0;
  }

  .content :global(:not(pre) > code) {
    background: rgba(0, 0, 0, 0.06);
    padding: 0.1em 0.3em;
    border-radius: 4px;
  }

  .content :global(ul),
  .content :global(ol) {
    margin: 0.3rem 0;
    padding-left: 1.2rem;
  }

  .content :global(a) {
    color: var(--color-accent);
  }

  .content :global(a:visited) {
    color: #6a4fc9;
  }

  form {
    display: flex;
    gap: var(--space-sm);
    margin-top: var(--space-md);
  }

  input {
    flex: 1;
    padding: var(--space-sm);
    font-size: 0.9rem;
  }
</style>
