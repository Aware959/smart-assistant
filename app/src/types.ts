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
  kind: "boolean" | "integer" | "number" | "string";
  description: string;
  default: boolean | number | string;
  current: boolean | number | string;
  hot_reload: boolean;
  restart_required: boolean;
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
