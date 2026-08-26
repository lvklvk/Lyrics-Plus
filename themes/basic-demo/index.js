import sdk from "/sdk.js";

const $ = (selector) => document.querySelector(selector);
const stateJson = $("#state-json");
const errorJson = $("#error-json");
const artwork = $("#artwork");
const meter = $("#spectrum-meter");

for (let index = 0; index < 16; index += 1) {
  meter.append(document.createElement("i"));
}

$("#sdk-version").textContent = String(sdk.version);
$("#connection-state").textContent = sdk.getConnectionState();

const formatMs = (value) => {
  if (!Number.isFinite(value)) return "-";
  const totalSeconds = Math.max(0, Math.floor(value / 1000));
  return `${Math.floor(totalSeconds / 60)}:${String(totalSeconds % 60).padStart(2, "0")}`;
};

const currentLyric = (state) => {
  const lines = state?.lyrics?.document?.tracks?.original?.lines || [];
  const position = state?.playback?.positionMs || 0;
  return lines.filter((line) => line.startMs <= position).at(-1)?.text || "暂无歌词";
};

const render = (state) => {
  const playback = state?.playback || {};
  const spectrum = state?.spectrumFrame || {};
  const spectrumState = state?.spectrumState || {};
  const duration = Number.isFinite(playback.durationMs) ? playback.durationMs : 1;
  const position = Number.isFinite(playback.positionMs) ? playback.positionMs : 0;

  $("#title").textContent = playback.title || "等待播放";
  $("#artist").textContent = playback.artist || "-";
  $("#album").textContent = playback.album || "-";
  $("#player").textContent = playback.player || "-";
  $("#source-app").textContent = playback.sourceAppName || playback.sourceAppBundleId || "-";
  $("#position").textContent = formatMs(playback.positionMs);
  $("#duration").textContent = formatMs(playback.durationMs);
  $("#playback-status").textContent = playback.isPlaying ? "正在播放" : (playback.isRunning ? "已暂停" : "未运行");
  $("#progress").max = String(duration);
  $("#progress").value = String(Math.min(duration, position));
  $("#seek").max = String(duration);
  $("#seek").value = String(Math.min(duration, position));

  const artworkUrl = sdk.getArtworkUrl(playback.artworkId);
  artwork.hidden = !artworkUrl;
  artwork.style.display = artworkUrl ? "block" : "none";
  if (artworkUrl && artwork.src !== new URL(artworkUrl, window.location.href).href) artwork.src = artworkUrl;
  if (!artworkUrl) artwork.removeAttribute("src");

  $("#lyrics-status").textContent = state?.lyrics?.status || "idle";
  $("#lyrics-error").textContent = state?.lyrics?.error || "";
  $("#current-lyric").textContent = currentLyric(state);
  $("#spectrum-status").textContent = spectrumState.status || "idle";
  $("#spectrum-source").textContent = `来源应用：${spectrum.sourceAppBundleId || "-"}`;
  [...meter.children].forEach((bar, index) => {
    const value = Number(spectrum.bands?.[index]) || 0;
    bar.style.height = `${Math.max(4, Math.min(100, value * 100))}%`;
  });
  stateJson.textContent = JSON.stringify(state ?? null, null, 2);
};

render(sdk.getState());
const unsubscribe = sdk.subscribe(render);
const unsubscribeConnection = sdk.onConnectionChange((state) => {
  $("#connection-state").textContent = state;
});
const unsubscribeError = sdk.onError((result) => {
  errorJson.textContent = JSON.stringify(result, null, 2);
});

const methods = {
  previousTrack: () => sdk.previousTrack(),
  play: () => sdk.play(),
  pause: () => sdk.pause(),
  togglePlayPause: () => sdk.togglePlayPause(),
  nextTrack: () => sdk.nextTrack(),
};

document.querySelectorAll("button[data-method]").forEach((button) => {
  button.addEventListener("click", () => methods[button.dataset.method]?.());
});

$("#seek").addEventListener("change", (event) => sdk.seek(Number(event.target.value)));
$("#send-generic-command").addEventListener("click", () => sdk.control($("#generic-action").value));

window.addEventListener("beforeunload", () => {
  unsubscribe();
  unsubscribeConnection();
  unsubscribeError();
  sdk.dispose();
});
