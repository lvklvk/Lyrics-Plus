// @lyrics-plus/theme-sdk v1
// This file is also served by the Laboratory Runtime as a browser ESM module.

const listeners = new Set();
const errorListeners = new Set();
const lifecycleListeners = new Set();
let latestState = null;
let connectionState = "connecting";

function post(message) {
  window.parent.postMessage({ source: "lyrics-plus-theme", ...message }, "*");
}

function notify(set, value) {
  for (const listener of set) {
    try {
      listener(value);
    } catch {
      // A theme listener must not interrupt delivery to other listeners.
    }
  }
}

function onMessage(event) {
  if (event.source !== window.parent || !event.data || event.data.source !== "lyrics-plus-host") return;
  if (event.data.kind === "state") {
    latestState = event.data.state;
    notify(listeners, latestState);
  } else if (event.data.kind === "command-result" && event.data.result && !event.data.result.ok) {
    notify(errorListeners, event.data.result);
  } else if (event.data.kind === "connection") {
    connectionState = event.data.state || "error";
    notify(lifecycleListeners, connectionState);
  }
}

window.addEventListener("message", onMessage);

function tokenizedResourceUrl(path) {
  const token = new URLSearchParams(window.location.search).get("token");
  const query = token ? `?token=${encodeURIComponent(token)}` : "";
  return `${path}${query}`;
}

export function createThemeSdk() {
  const ownListeners = new Set();
  const ownErrorListeners = new Set();
  const ownLifecycleListeners = new Set();
  post({ kind: "ready" });
  return {
    version: 1,
    getState: () => latestState,
    getConnectionState: () => connectionState,
    subscribe: (listener) => {
      ownListeners.add(listener);
      listeners.add(listener);
      post({ kind: "subscribe" });
      if (latestState) listener(latestState);
      return () => {
        ownListeners.delete(listener);
        listeners.delete(listener);
      };
    },
    onError: (listener) => {
      ownErrorListeners.add(listener);
      errorListeners.add(listener);
      return () => {
        ownErrorListeners.delete(listener);
        errorListeners.delete(listener);
      };
    },
    onConnectionChange: (listener) => {
      ownLifecycleListeners.add(listener);
      lifecycleListeners.add(listener);
      listener(connectionState);
      return () => {
        ownLifecycleListeners.delete(listener);
        lifecycleListeners.delete(listener);
      };
    },
    getArtworkUrl: (artworkId = latestState?.playback?.artworkId) => artworkId
      ? tokenizedResourceUrl(`/artwork/${encodeURIComponent(artworkId)}`)
      : null,
    control: (action, positionMs) => post({ kind: "command", action, positionMs }),
    play: () => post({ kind: "command", action: "play" }),
    pause: () => post({ kind: "command", action: "pause" }),
    togglePlayPause: () => post({ kind: "command", action: "toggle_play_pause" }),
    previousTrack: () => post({ kind: "command", action: "previous" }),
    nextTrack: () => post({ kind: "command", action: "next" }),
    seek: (positionMs) => post({ kind: "command", action: "play", positionMs }),
    dispose: () => {
      for (const listener of ownListeners) listeners.delete(listener);
      for (const listener of ownErrorListeners) errorListeners.delete(listener);
      for (const listener of ownLifecycleListeners) lifecycleListeners.delete(listener);
      ownListeners.clear();
      ownErrorListeners.clear();
      ownLifecycleListeners.clear();
    },
  };
}

export const sdk = createThemeSdk();
export default sdk;
