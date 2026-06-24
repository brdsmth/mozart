export interface Task {
  id: string;
  parent_id: string | null;
  name: string;
  working_dir: string;
  backend: string;
  status: string;
  permission_mode: string;
  created_at: string;
  updated_at: string;
}

export interface Message {
  id: number;
  task_id: string;
  role: "user" | "agent" | "system";
  content: string;
  created_at: string;
}
