import { invoke } from "./core";
import type {
  LaboratoryClientSettingsInput,
  LaboratoryConnectInput,
  LaboratoryServerSettingsInput,
  LaboratoryServerRecord,
  LaboratoryStatus,
  LaboratoryThemeInfo,
  LaboratoryRole,
} from "../types";

export const laboratoryApi = {
  getStatus: () => invoke<LaboratoryStatus>("get_laboratory_status"),
  start: () => invoke<LaboratoryStatus>("start_laboratory"),
  stop: () => invoke<LaboratoryStatus>("stop_laboratory"),
  setRole: (role: LaboratoryRole) => invoke<LaboratoryStatus>("set_laboratory_role", { role }),
  setAutoStart: (enabled: boolean) => invoke<LaboratoryStatus>("set_laboratory_auto_start", { enabled }),
  setServerSettings: (settings: LaboratoryServerSettingsInput) => invoke<LaboratoryStatus>("set_laboratory_server_settings", { settings }),
  setClientSettings: (settings: LaboratoryClientSettingsInput) => invoke<LaboratoryStatus>("set_laboratory_client_settings", { settings }),
  setServerPassword: (password: string) => invoke<LaboratoryStatus>("set_laboratory_server_password", { password }),
  resetWebToken: () => invoke<LaboratoryStatus>("reset_laboratory_web_token"),
  scanServers: () => invoke<LaboratoryServerRecord[]>("scan_laboratory_servers"),
  connect: (input: LaboratoryConnectInput) => invoke<LaboratoryStatus>("connect_laboratory_server", { input }),
  retryConnection: () => invoke<LaboratoryStatus>("retry_laboratory_connection"),
  kickClient: (clientId: string) => invoke<LaboratoryStatus>("kick_laboratory_client", { clientId }),
  forgetClient: (clientId: string) => invoke<LaboratoryStatus>("forget_laboratory_client", { clientId }),
  getThemes: () => invoke<LaboratoryThemeInfo[]>("get_laboratory_themes"),
  revealThemesDirectory: () => invoke<void>("reveal_laboratory_themes_directory"),
};
