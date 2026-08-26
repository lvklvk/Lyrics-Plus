export type PlaybackAction = "play" | "pause" | "toggle_play_pause" | "previous" | "next";
export type LaboratoryConnectionState = "connecting" | "connected" | "disconnected" | "error";

export type LaboratorySdk = {
  readonly version: 1;
  getState(): unknown;
  getConnectionState(): LaboratoryConnectionState | string;
  subscribe(listener: (state: unknown) => void): () => void;
  onError(listener: (result: { requestId?: string; ok: false; error?: string }) => void): () => void;
  onConnectionChange(listener: (state: LaboratoryConnectionState | string) => void): () => void;
  getArtworkUrl(artworkId?: string | null): string | null;
  control(action: PlaybackAction, positionMs?: number): void;
  play(): void;
  pause(): void;
  togglePlayPause(): void;
  previousTrack(): void;
  nextTrack(): void;
  seek(positionMs: number): void;
  dispose(): void;
};

export function createThemeSdk(): LaboratorySdk;
export const sdk: LaboratorySdk;
export default sdk;
