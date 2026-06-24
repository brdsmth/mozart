import { invoke } from "@tauri-apps/api/core";
import type { Task, Message } from "./types";

export function createTask(
  name: string,
  workingDir: string,
  permissionMode: string,
): Promise<Task> {
  return invoke("create_task", { name, workingDir, parentId: null, permissionMode });
}

export function listTasks(): Promise<Task[]> {
  return invoke("list_tasks");
}

export function listMessages(taskId: string): Promise<Message[]> {
  return invoke("list_messages", { taskId });
}

export function sendMessage(taskId: string, content: string): Promise<Message> {
  return invoke("send_message", { taskId, content });
}

export function cancelRun(taskId: string): Promise<void> {
  return invoke("cancel_run", { taskId });
}
