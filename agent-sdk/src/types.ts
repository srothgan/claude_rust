export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

export interface PromptChunk {
  kind: string;
  value: Json;
}

export interface ModeInfo {
  id: string;
  name: string;
  description?: string;
}

export interface ModeState {
  current_mode_id: string;
  current_mode_name: string;
  available_modes: ModeInfo[];
}

export interface AvailableCommand {
  name: string;
  description: string;
  input_hint?: string;
}

export interface UsageUpdate {
  input_tokens?: number;
  output_tokens?: number;
  cache_read_tokens?: number;
  cache_write_tokens?: number;
  total_cost_usd?: number;
  turn_cost_usd?: number;
  context_window?: number;
  max_output_tokens?: number;
}

export type ContentBlock =
  | { type: "text"; text: string }
  | { type: "image"; mime_type?: string; uri?: string; data?: string };

export type ToolCallContent =
  | { type: "content"; content: ContentBlock }
  | { type: "diff"; old_path: string; new_path: string; old: string; new: string };

export interface ToolLocation {
  path: string;
  line?: number;
}

export interface ToolCall {
  tool_call_id: string;
  title: string;
  kind: string;
  status: string;
  content: ToolCallContent[];
  raw_input?: Json;
  raw_output?: string;
  locations: ToolLocation[];
  meta?: Json;
}

export interface ToolCallUpdateFields {
  title?: string;
  kind?: string;
  status?: string;
  content?: ToolCallContent[];
  raw_input?: Json;
  raw_output?: string;
  locations?: ToolLocation[];
  meta?: Json;
}

export interface ToolCallUpdate {
  tool_call_id: string;
  fields: ToolCallUpdateFields;
}

export interface PlanEntry {
  content: string;
  status: string;
  active_form: string;
}

export type SessionUpdate =
  | { type: "agent_message_chunk"; content: ContentBlock }
  | { type: "user_message_chunk"; content: ContentBlock }
  | { type: "agent_thought_chunk"; content: ContentBlock }
  | { type: "tool_call"; tool_call: ToolCall }
  | { type: "tool_call_update"; tool_call_update: ToolCallUpdate }
  | { type: "plan"; entries: PlanEntry[] }
  | { type: "available_commands_update"; commands: AvailableCommand[] }
  | { type: "current_mode_update"; current_mode_id: string }
  | { type: "config_option_update"; option_id: string; value: Json }
  | { type: "usage_update"; usage: UsageUpdate }
  | { type: "session_status_update"; status: "compacting" | "idle" }
  | { type: "compaction_boundary"; trigger: "manual" | "auto"; pre_tokens: number };

export interface PermissionOption {
  option_id: string;
  name: string;
  description?: string;
  kind: string;
}

export interface PermissionRequest {
  tool_call: ToolCall;
  options: PermissionOption[];
}

export type PermissionOutcome =
  | { outcome: "selected"; option_id: string }
  | { outcome: "cancelled" };

export interface BridgeCommandEnvelope {
  request_id?: string;
  command: string;
  [key: string]: unknown;
}

export type BridgeCommand =
  | {
      command: "initialize";
      cwd: string;
      metadata?: Record<string, Json>;
    }
  | {
      command: "create_session";
      cwd: string;
      yolo: boolean;
      model?: string;
      resume?: string;
      metadata?: Record<string, Json>;
    }
  | {
      command: "load_session";
      session_id: string;
      metadata?: Record<string, Json>;
    }
  | {
      command: "prompt";
      session_id: string;
      chunks: PromptChunk[];
    }
  | {
      command: "cancel_turn";
      session_id: string;
    }
  | {
      command: "set_model";
      session_id: string;
      model: string;
    }
  | {
      command: "set_mode";
      session_id: string;
      mode: string;
    }
  | {
      command: "new_session";
      cwd: string;
      yolo: boolean;
      model?: string;
    }
  | {
      command: "permission_response";
      session_id: string;
      tool_call_id: string;
      outcome: PermissionOutcome;
    }
  | {
      command: "shutdown";
    };

export interface BridgeEventEnvelope {
  request_id?: string;
  event: string;
  [key: string]: unknown;
}

export interface InitializeResult {
  agent_name: string;
  agent_version: string;
  auth_methods: Array<{ id: string; name: string; description: string }>;
  capabilities: {
    prompt_image: boolean;
    prompt_embedded_context: boolean;
    load_session: boolean;
    supports_list_sessions: boolean;
    supports_resume: boolean;
  };
}

export type BridgeEvent =
  | {
      event: "connected";
      session_id: string;
      cwd: string;
      model_name: string;
      mode: ModeState | null;
      history_updates?: SessionUpdate[];
    }
  | { event: "auth_required"; method_name: string; method_description: string }
  | { event: "connection_failed"; message: string }
  | { event: "session_update"; session_id: string; update: SessionUpdate }
  | { event: "permission_request"; session_id: string; request: PermissionRequest }
  | { event: "turn_complete"; session_id: string }
  | { event: "turn_error"; session_id: string; message: string }
  | { event: "slash_error"; session_id: string; message: string }
  | {
      event: "session_replaced";
      session_id: string;
      cwd: string;
      model_name: string;
      mode: ModeState | null;
      history_updates?: SessionUpdate[];
    }
  | { event: "initialized"; result: InitializeResult }
  | { event: "sessions_listed"; sessions: Array<{ session_id: string; cwd: string; title?: string; updated_at?: string }>; next_cursor?: string };
