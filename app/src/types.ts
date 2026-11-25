export type ProcessPhase = "running" | "stopped" | "starting" | "error" | "unavailable";

export type ServiceStatus = {
  phase: ProcessPhase;
  pid: number | null;
  message: string;
  autoRestart: boolean;
  restartAttempts: number;
  servicePath: string;
  workingDir: string;
  envFile: string;
  logFile: string;
  pidFile: string;
  controlStateFile: string;
};

export type ControlResult<T> = {
  ok: boolean;
  data: T | null;
  error: string | null;
};

export type SettingsView = {
  revision: number;
  values: Record<string, boolean | number | string>;
};

export type SettingsSchemaItem = {
  key: string;
  /** 所属分组（llm / embedding / memory / intent / identity / channel / tool / program）。 */
  group: string;
  /** 分组标题（面板上的卡片标题）。 */
  group_label: string;
  kind: "boolean" | "integer" | "number" | "string" | "secret";
  description: string;
  default: boolean | number | string | null;
  current: boolean | number | string | null;
  hot_reload: boolean;
  restart_required: boolean;
  /** 是否可在面板里改；启动期固定的项为 false——只展示。 */
  editable: boolean;
};

export type IdentityOverview = {
  version: number | null;
  text: string;
  latestCheckAt: string | null;
  gate: "run" | "pending_limit" | "global_cooldown" | "disabled";
  pendingCount: number;
};

export type ProposalView = {
  id: string;
  fieldKey: string;
  beforeValue: string | null;
  afterValue: string;
  reason: string;
  evidenceMessageIds: string[];
  confidence: string;
  riskLevel: "low" | "medium" | "high";
  status: string;
  proposer: string;
  decidedBy: string | null;
  baseVersion: number;
};
