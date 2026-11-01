import { invoke } from "@tauri-apps/api/core";
import "./styles.css";

type ServiceStatus = {
  phase: "running" | "stopped" | "starting" | "error";
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

type ControlResult<T> = {
  ok: boolean;
  data: T | null;
  error: string | null;
};

type SettingsView = {
  revision: number;
  values: Record<string, boolean | number | string>;
};

type SettingsSchemaItem = {
  key: string;
  kind: "boolean" | "integer" | "number" | "string";
  description: string;
  default: boolean | number | string;
  current: boolean | number | string;
  hot_reload: boolean;
  restart_required: boolean;
};

const phaseText: Record<ServiceStatus["phase"], string> = {
  running: "运行中",
  stopped: "已停止",
  starting: "启动中",
  error: "需要处理",
};

function element<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) {
    throw new Error(`Missing element #${id}`);
  }
  return node as T;
}

function setText(id: string, value: string): void {
  element(id).textContent = value;
}

function render(status: ServiceStatus): void {
  const badge = element("phase-badge");
  badge.dataset.phase = status.phase;
  setText("phase-text", phaseText[status.phase]);
  setText("status-message", status.message);
  setText("pid", status.pid ? String(status.pid) : "—");
  setText("auto-restart", status.autoRestart ? "开启" : "关闭");
  setText("restart-attempts", String(status.restartAttempts));
  setText("zhiwei-service-path", status.servicePath);
  setText("env-file", status.envFile);
  setText("log-file", status.logFile);
  setText("control-state-file", status.controlStateFile);
}

async function refresh(): Promise<void> {
  try {
    render(await invoke<ServiceStatus>("service_status"));
  } catch (error) {
    const badge = element("phase-badge");
    badge.dataset.phase = "error";
    setText("phase-text", "连接失败");
    setText("status-message", String(error));
  }
}

async function runServiceCommand(command: string): Promise<void> {
  setBusy(true);
  try {
    render(await invoke<ServiceStatus>(command));
  } catch (error) {
    setText("status-message", String(error));
  } finally {
    setBusy(false);
  }
}

function setBusy(busy: boolean): void {
  document.querySelectorAll<HTMLButtonElement>("button").forEach((button) => {
    button.disabled = busy;
  });
}

let settingsView: SettingsView | null = null;
let settingsSchema: SettingsSchemaItem[] = [];

function settingsElement(): HTMLElement {
  return element("settings-grid");
}

function renderSettings(): void {
  if (!settingsView) {
    settingsElement().innerHTML = `<p class="settings-empty">控制接口尚未就绪</p>`;
    return;
  }
  settingsElement().innerHTML = "";
  for (const item of settingsSchema) {
    const row = document.createElement("div");
    row.className = "setting-row";
    const label = document.createElement("label");
    label.htmlFor = `setting-${item.key}`;
    label.textContent = item.description;
    const hint = document.createElement("span");
    hint.className = "setting-key";
    hint.textContent = item.key;
    label.append(hint);
    row.append(label);

    let input: HTMLInputElement;
    if (item.kind === "boolean") {
      input = document.createElement("input");
      input.type = "checkbox";
      input.checked = Boolean(settingsView.values[item.key]);
    } else {
      input = document.createElement("input");
      input.type = item.kind === "string" ? "text" : "number";
      if (item.kind === "number") {
        input.step = "0.01";
      }
      input.value = String(settingsView.values[item.key] ?? "");
    }
    input.id = `setting-${item.key}`;
    input.dataset.settingKey = item.key;
    input.dataset.settingKind = item.kind;
    row.append(input);
    settingsElement().append(row);
  }
  setText("settings-revision", `revision ${settingsView.revision}`);
}

async function loadSettings(): Promise<void> {
  const [viewResult, schemaResult] = await Promise.all([
    invoke<ControlResult<SettingsView>>("settings_view"),
    invoke<ControlResult<SettingsSchemaItem[]>>("settings_schema"),
  ]);
  if (!viewResult.ok || !viewResult.data) {
    setText("settings-message", viewResult.error ?? "无法读取运行时配置");
    return;
  }
  if (!schemaResult.ok || !schemaResult.data) {
    setText("settings-message", schemaResult.error ?? "无法读取配置 schema");
    return;
  }
  settingsView = viewResult.data;
  settingsSchema = schemaResult.data;
  setText("settings-message", "运行时配置来自 zhiwei-service 数据库，保存后即时生效。");
  renderSettings();
}

function collectSettingChanges(): Record<string, boolean | number | string> {
  const changes: Record<string, boolean | number | string> = {};
  settingsElement()
    .querySelectorAll<HTMLInputElement>("input[data-setting-key]")
    .forEach((input) => {
      const key = input.dataset.settingKey;
      if (!key) return;
      const kind = input.dataset.settingKind;
      if (kind === "boolean") {
        changes[key] = input.checked;
      } else if (kind === "integer") {
        changes[key] = Number.parseInt(input.value, 10);
      } else if (kind === "number") {
        changes[key] = Number.parseFloat(input.value);
      } else {
        changes[key] = input.value.trim();
      }
    });
  return changes;
}

async function saveSettings(): Promise<void> {
  if (!settingsView) return;
  setBusy(true);
  try {
    const result = await invoke<ControlResult<SettingsView>>("update_settings", {
      expectedRevision: settingsView.revision,
      changes: collectSettingChanges(),
    });
    if (!result.ok || !result.data) {
      setText("settings-message", result.error ?? "保存失败");
      return;
    }
    settingsView = result.data;
    setText("settings-message", `已保存，revision ${settingsView.revision}`);
    renderSettings();
  } catch (error) {
    setText("settings-message", String(error));
  } finally {
    setBusy(false);
  }
}

document.querySelectorAll<HTMLButtonElement>("[data-action]").forEach((button) => {
  button.addEventListener("click", () => {
    const command = button.dataset.action;
    if (command) {
      void runServiceCommand(command);
    }
  });
});

document.querySelectorAll<HTMLButtonElement>("[data-command]").forEach((button) => {
  button.addEventListener("click", () => {
    const command = button.dataset.command;
    if (command) {
      void invoke(command).catch((error) => setText("status-message", String(error)));
    }
  });
});

element("save-settings").addEventListener("click", () => void saveSettings());
element("reload-settings").addEventListener("click", () => void loadSettings());

void refresh();
void loadSettings();
window.setInterval(() => void refresh(), 2_000);
window.setInterval(() => void loadSettings(), 10_000);
