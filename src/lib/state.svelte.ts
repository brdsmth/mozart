import type { Task, Message } from "./types";
import * as api from "./api";

// Messages and in-flight status live per task, keyed by task id, so switching
// the selected task mid-send can never append a reply to the wrong session's
// history or clear another session's "thinking" indicator.
interface TaskSession {
  messages: Message[];
  sending: boolean;
  cancelling: boolean;
  // True only when this task was already `running` when we selected it —
  // i.e. its run was reconciled from a previous session after a restart, not
  // started by a `sendMessage` call in *this* one. There's no real
  // client-side start time to count up from in that case (see `reconnecting`
  // usage in ChatPane).
  reconnecting: boolean;
}

const sessions = new Map<string, TaskSession>();

export const appState = $state({
  tasks: [] as Task[],
  selectedTaskId: null as string | null,
});

function getSession(taskId: string): TaskSession {
  const existing = sessions.get(taskId);
  if (existing) return existing;

  const session = $state({ messages: [], sending: false, cancelling: false, reconnecting: false });
  sessions.set(taskId, session);
  return session;
}

export function currentMessages(): Message[] {
  return appState.selectedTaskId ? getSession(appState.selectedTaskId).messages : [];
}

export function isSending(): boolean {
  if (!appState.selectedTaskId) return false;
  const session = getSession(appState.selectedTaskId);
  return session.sending || session.reconnecting;
}

export function isCancelling(): boolean {
  return appState.selectedTaskId ? getSession(appState.selectedTaskId).cancelling : false;
}

export function isReconnecting(): boolean {
  return appState.selectedTaskId ? getSession(appState.selectedTaskId).reconnecting : false;
}

export async function refreshTasks() {
  appState.tasks = await api.listTasks();
}

export async function selectTask(taskId: string) {
  appState.selectedTaskId = taskId;
  const session = getSession(taskId);
  session.messages = await api.listMessages(taskId);

  const task = appState.tasks.find((t) => t.id === taskId);
  if (task?.status === "running" && !session.sending && !session.reconnecting) {
    watchReconciledRun(taskId);
  }
}

// Reconnects the UI to a run that was already in flight when this task was
// selected — true only right after a restart, since `sendMessage` below
// marks every run it starts itself as `sending`. Polls until the backend's
// startup-reconciliation polling loop (or a live cancel) resolves the run,
// then refreshes messages so the eventual reply (or cancellation) appears
// without the user having to leave and reopen the task.
async function watchReconciledRun(taskId: string) {
  const session = getSession(taskId);
  session.reconnecting = true;
  try {
    while (true) {
      await new Promise((resolve) => setTimeout(resolve, 1500));
      const tasks = await api.listTasks();
      appState.tasks = tasks;
      const task = tasks.find((t) => t.id === taskId);
      if (!task || task.status !== "running") break;
    }
    if (appState.selectedTaskId === taskId) {
      session.messages = await api.listMessages(taskId);
    }
  } finally {
    session.reconnecting = false;
  }
}

export async function createTask(name: string, workingDir: string, permissionMode: string) {
  const task = await api.createTask(name, workingDir, permissionMode);
  await refreshTasks();
  await selectTask(task.id);
}

export async function sendMessage(taskId: string, content: string) {
  const session = getSession(taskId);
  if (session.sending) return;

  session.messages = [
    ...session.messages,
    {
      id: -1,
      task_id: taskId,
      role: "user",
      content,
      created_at: new Date().toISOString(),
    },
  ];
  session.sending = true;
  try {
    const reply = await api.sendMessage(taskId, content);
    session.messages = [...session.messages, reply];
  } catch (error) {
    // "Cancelled." is the one sentinel error message the backend sends on
    // a deliberate user cancellation (see `cancel_run`) — render it as a
    // neutral system message, not an error, and without the "Error:" prefix.
    const message = String(error);
    session.messages = [
      ...session.messages,
      {
        id: -1,
        task_id: taskId,
        role: "system",
        content: message === "Cancelled." ? message : `Error: ${message}`,
        created_at: new Date().toISOString(),
      },
    ];
  } finally {
    session.sending = false;
    await refreshTasks();
  }
}

export async function cancelMessage(taskId: string) {
  const session = getSession(taskId);
  if (session.cancelling || (!session.sending && !session.reconnecting)) return;
  session.cancelling = true;
  try {
    await api.cancelRun(taskId);
  } finally {
    session.cancelling = false;
  }
}
