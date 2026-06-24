import { describe, it, expect, vi, beforeEach } from "vitest";
import type { Task, Message } from "./types";
import * as api from "./api";
import { appState, currentMessages, isSending, refreshTasks, selectTask, createTask, sendMessage } from "./state.svelte";

vi.mock("./api");

function task(id: string): Task {
  return {
    id,
    parent_id: null,
    name: id,
    working_dir: "/tmp",
    backend: "claude-cli",
    status: "idle",
    permission_mode: "plan",
    created_at: "t0",
    updated_at: "t0",
  };
}

function agentReply(taskId: string, content: string): Message {
  return { id: 1, task_id: taskId, role: "agent", content, created_at: "t1" };
}

beforeEach(() => {
  vi.resetAllMocks();
  appState.tasks = [];
  appState.selectedTaskId = null;
});

describe("currentMessages / isSending with no task selected", () => {
  it("return empty/false when nothing is selected", () => {
    expect(currentMessages()).toEqual([]);
    expect(isSending()).toBe(false);
  });
});

describe("refreshTasks", () => {
  it("populates appState.tasks from the API", async () => {
    vi.mocked(api.listTasks).mockResolvedValue([task("a"), task("b")]);
    await refreshTasks();
    expect(appState.tasks.map((t) => t.id)).toEqual(["a", "b"]);
  });
});

describe("selectTask", () => {
  it("sets selectedTaskId and loads that task's messages", async () => {
    const messages = [{ id: 1, task_id: "select-1", role: "user" as const, content: "hi", created_at: "t0" }];
    vi.mocked(api.listMessages).mockResolvedValue(messages);

    await selectTask("select-1");

    expect(appState.selectedTaskId).toBe("select-1");
    expect(currentMessages()).toEqual(messages);
    expect(api.listMessages).toHaveBeenCalledWith("select-1");
  });

  it("keeps each task's sending flag independent when switching back and forth", async () => {
    vi.mocked(api.listMessages).mockResolvedValue([]);
    vi.mocked(api.listTasks).mockResolvedValue([]);
    await selectTask("switch-a");
    await selectTask("switch-b");

    // Put switch-a's session mid-flight, then switch away from it.
    let resolveSend!: (m: Message) => void;
    vi.mocked(api.sendMessage).mockReturnValueOnce(
      new Promise<Message>((resolve) => {
        resolveSend = resolve;
      }),
    );
    await selectTask("switch-a");
    const sendPromise = sendMessage("switch-a", "hello");
    expect(isSending()).toBe(true);

    await selectTask("switch-b");
    expect(isSending(), "switch-b never started a send, so it should not appear to be sending").toBe(false);

    await selectTask("switch-a");
    expect(isSending(), "switch-a's in-flight send should still be reflected after switching back").toBe(true);

    resolveSend(agentReply("switch-a", "reply"));
    await sendPromise;
    expect(isSending()).toBe(false);
  });
});

describe("createTask", () => {
  it("creates via the API, refreshes the task list, then selects the new task", async () => {
    const created = task("created-1");
    vi.mocked(api.createTask).mockResolvedValue(created);
    vi.mocked(api.listTasks).mockResolvedValue([created]);
    vi.mocked(api.listMessages).mockResolvedValue([]);

    await createTask("created-1", "/tmp/created-1", "plan");

    expect(api.createTask).toHaveBeenCalledWith("created-1", "/tmp/created-1", "plan");
    expect(appState.tasks.map((t) => t.id)).toEqual(["created-1"]);
    expect(appState.selectedTaskId).toBe("created-1");
  });
});

describe("sendMessage", () => {
  beforeEach(() => {
    vi.mocked(api.listTasks).mockResolvedValue([]);
  });

  it("optimistically appends the user message before the API call resolves", async () => {
    let resolveSend!: (m: Message) => void;
    vi.mocked(api.sendMessage).mockReturnValueOnce(
      new Promise<Message>((resolve) => {
        resolveSend = resolve;
      }),
    );

    // The append happens synchronously, before the first `await` inside
    // sendMessage, so it's already visible the instant the call returns —
    // well before the mocked API call resolves.
    const pending = sendMessage("send-optimistic", "hello");
    appState.selectedTaskId = "send-optimistic";
    expect(currentMessages()).toHaveLength(1);
    expect(currentMessages()[0]).toMatchObject({ role: "user", content: "hello" });

    resolveSend(agentReply("send-optimistic", "hi back"));
    await pending;
  });

  it("appends the agent reply and clears the sending flag on success", async () => {
    vi.mocked(api.sendMessage).mockResolvedValue(agentReply("send-success", "pong"));

    appState.selectedTaskId = "send-success";
    await sendMessage("send-success", "ping");

    expect(isSending()).toBe(false);
    const messages = currentMessages();
    expect(messages).toHaveLength(2);
    expect(messages[1]).toMatchObject({ role: "agent", content: "pong" });
  });

  it("appends a system error message and clears the sending flag on failure", async () => {
    vi.mocked(api.sendMessage).mockRejectedValue(new Error("agent exploded"));

    appState.selectedTaskId = "send-failure";
    await sendMessage("send-failure", "ping");

    expect(isSending()).toBe(false);
    const messages = currentMessages();
    expect(messages).toHaveLength(2);
    expect(messages[1].role).toBe("system");
    expect(messages[1].content).toContain("agent exploded");
  });

  it("ignores a second call while the first is still in flight", async () => {
    let resolveFirst!: (m: Message) => void;
    vi.mocked(api.sendMessage).mockReturnValueOnce(
      new Promise<Message>((resolve) => {
        resolveFirst = resolve;
      }),
    );

    appState.selectedTaskId = "send-guard";
    const first = sendMessage("send-guard", "first");
    expect(isSending()).toBe(true);

    await sendMessage("send-guard", "second");
    expect(api.sendMessage).toHaveBeenCalledTimes(1);
    expect(currentMessages(), "the guarded second call should not append its own message").toHaveLength(1);

    resolveFirst(agentReply("send-guard", "reply"));
    await first;
    expect(isSending()).toBe(false);
  });

  it("refreshes the task list after both success and failure", async () => {
    vi.mocked(api.sendMessage).mockResolvedValueOnce(agentReply("send-refresh", "pong"));
    appState.selectedTaskId = "send-refresh";
    await sendMessage("send-refresh", "ping");
    expect(api.listTasks).toHaveBeenCalledTimes(1);

    vi.mocked(api.sendMessage).mockRejectedValueOnce(new Error("boom"));
    await sendMessage("send-refresh", "ping again");
    expect(api.listTasks).toHaveBeenCalledTimes(2);
  });
});
