export type Json =
  null | boolean | number | string | Json[] | { [key: string]: Json };
export interface Conversation {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  claude_session_id: string | null;
  parent_conversation_id?: string | null;
  favorite: number;
  no_memory: number;
  project_id: string | null;
  summary: string;
}
export interface Message {
  id: string;
  role: string;
  content_text: string;
  status: string;
  created_at: string;
}
export interface Module {
  id: string;
  name: string;
  kind: string;
  enabled: boolean;
  version: string;
  tools: number;
  live: string;
}
export interface Memory {
  id: string;
  title: string;
  content: string;
  type: string;
  explicit: number;
  pinned: number;
  disabled: number;
  superseded_by: string | null;
  source_conversation_id: string | null;
  project_id: string | null;
  confidence?: number;
  importance?: number;
  use_count?: number;
  last_used_at?: string | null;
}
export interface Permission {
  rememberable?: boolean;
  id: string;
  tool_id: string;
  risk: string;
  input_json_redacted: string;
}
export interface Task {
  id: string;
  title: string;
  status: string;
  due_at: string | null;
  description: string;
  revision: number;
  recurrence: {
    version: number;
    frequency: "daily" | "weekly" | "monthly";
    interval: number;
    anchor: string;
  } | null;
  recurrence_error: string | null;
  project_id: string | null;
  conversation_id: string | null;
  project_name: string | null;
  conversation_title: string | null;
  updated_at: string;
}
export interface Activity {
  id: string;
  timestamp: string;
  tool_id: string;
  risk: string;
  input_summary: string;
  output_summary: string;
  status: string;
  duration_ms: number;
  undo_token?: string | null;
  undo_state?: string | null;
}
export interface Tool {
  id: string;
  name: string;
  description: string;
  inputSchema: { required: string[]; properties: Record<string, Json> };
}
export interface Watch {
  id: string;
  owner_module: string;
  kind: string;
  enabled: number;
  effective_enabled: boolean;
  status: string;
  error_code?: string | null;
  config: { path: string; condition: string; recursive: boolean };
}
export interface Project {
  id: string;
  name: string;
  root_path: string;
}
export interface AgentEvent {
  type: string;
  conversation_id: string;
  run_id: string;
  text?: string;
  tool?: string;
  error?: string;
  message?: Message;
}
export type PetState =
  | "idle"
  | "hover"
  | "dragging"
  | "falling"
  | "thinking"
  | "working"
  | "searching_web"
  | "looking_at_screen"
  | "using_pc"
  | "waiting_permission"
  | "waiting_user"
  | "success"
  | "error"
  | "privacy"
  | "dnd"
  | "sleeping";
