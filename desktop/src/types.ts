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

export type GatewayHealth = {
  ok: boolean;
  diffusion: string | null;
  tts: string | null;
  asrEngine: string | null;
  asrImportable: boolean | null;
  asrModelLoaded: boolean | null;
};

export type GatewayStatus = {
  phase: ProcessPhase;
  pid: number | null;
  message: string;
  autoRestart: boolean;
  restartAttempts: number;
  installed: boolean;
  executablePath: string;
  workingDir: string;
  envFile: string;
  logFile: string;
  url: string;
  serviceUrl: string;
  urlMatchesService: boolean;
  health: GatewayHealth | null;
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

export type GatewaySettingItem = {
  key: string;
  description: string;
  kind: "boolean" | "integer" | "choice" | "string";
  default: string;
  choices: string[];
  restartRequired: boolean;
};

export type GatewaySettingsView = {
  envFile: string;
  values: Record<string, string>;
  schema: GatewaySettingItem[];
};

export type GatewaySettingsResult = {
  view: GatewaySettingsView;
  restarted: boolean;
  warning: string | null;
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
