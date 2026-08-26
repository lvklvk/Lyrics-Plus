import type { LyricsRuntimeSnapshot } from "./lyrics";
import type { PlaybackAction, PlaybackSnapshot, PlaybackSpectrumFrame, PlaybackSpectrumState } from "./player";

export type LaboratoryRole = "server" | "client";
export type LaboratoryPhase =
  | "stopped"
  | "starting"
  | "running"
  | "connecting"
  | "reconnecting"
  | "error";

export type LaboratoryServerPreferences = {
  name: string;
  port: number;
  discoveryEnabled: boolean;
  webEnabled: boolean;
  debounceMs: number;
};

export type LaboratoryClientPreferences = {
  name: string;
  lastServerId: string | null;
};

export type LaboratoryPreferences = {
  role: LaboratoryRole;
  autoStart: boolean;
  server: LaboratoryServerPreferences;
  client: LaboratoryClientPreferences;
};

export type LaboratoryServerRecord = {
  serverId: string;
  name: string;
  address: string;
  port: number;
  protocolVersion: number;
  requiresPassword: boolean;
  webAvailable: boolean;
  lastConnectedAtMs: number | null;
  discovered: boolean;
};

export type LaboratoryClientRecord = {
  clientId: string;
  name: string;
  online: boolean;
  lastConnectedAtMs: number | null;
};

export type LaboratoryThemeInfo = {
  id: string;
  name: string;
  version: string;
  entry: string;
  sdkVersion: string;
};

export type LaboratoryWebAddress = {
  address: string;
  url: string;
};

export type LaboratoryStateSnapshot = {
  playback: PlaybackSnapshot;
  lyrics: LyricsRuntimeSnapshot;
  spectrumState: PlaybackSpectrumState;
  spectrumFrame: PlaybackSpectrumFrame;
  observedAtMs: number;
};

export type LaboratoryStatus = {
  role: LaboratoryRole;
  phase: LaboratoryPhase;
  running: boolean;
  message: string | null;
  serverId: string;
  clientId: string;
  serverAddress: string | null;
  webAddresses: LaboratoryWebAddress[];
  serverPasswordEnabled: boolean;
  clients: LaboratoryClientRecord[];
  recentServers: LaboratoryServerRecord[];
  themes: LaboratoryThemeInfo[];
  remoteState: LaboratoryStateSnapshot | null;
};

export type LaboratoryServerSettingsInput = LaboratoryServerPreferences;

export type LaboratoryClientSettingsInput = {
  name: string;
};

export type LaboratoryConnectInput = {
  serverId: string | null;
  name: string;
  address: string;
  port: number;
  requiresPassword: boolean;
  webAvailable: boolean;
  password: string;
};

export type LaboratoryCommandResult = {
  requestId: string;
  ok: boolean;
  error?: string;
};

export type LaboratoryRemoteCommand = {
  action: PlaybackAction;
  positionMs?: number;
};
