import { invoke } from "@tauri-apps/api/core";
import type {
  ControlResult,
  GatewaySettingsResult,
  GatewaySettingsView,
  GatewayStatus,
  ServiceStatus,
  SettingsSchemaItem,
  SettingsView,
} from "./types";

// 桌面壳专属命令的单一入口：换宿主（如未来 Android 壳）时只需替换这一层。
export const api = {
  serviceStatus: () => invoke<ServiceStatus>("service_status"),
  serviceCommand: (command: string) => invoke<ServiceStatus>(command),
  gatewayStatus: () => invoke<GatewayStatus>("gateway_status"),
  gatewayCommand: (command: string) => invoke<GatewayStatus>(command),
  settingsView: () => invoke<ControlResult<SettingsView>>("settings_view"),
  settingsSchema: () => invoke<ControlResult<SettingsSchemaItem[]>>("settings_schema"),
  updateSettings: (
    expectedRevision: number,
    changes: Record<string, boolean | number | string>,
  ) =>
    invoke<ControlResult<SettingsView>>("update_settings", {
      expectedRevision,
      changes,
    }),
  gatewaySettings: () => invoke<GatewaySettingsView>("gateway_settings"),
  updateGatewaySettings: (changes: Record<string, string>) =>
    invoke<GatewaySettingsResult>("update_gateway_settings", { changes }),
  openFile: (command: string) => invoke(command),
};
