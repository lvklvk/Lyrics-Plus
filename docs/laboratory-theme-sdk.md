# Laboratory 主题开发说明

实验室网页服务只加载已经构建完成的主题文件夹，不安装依赖、不解压 ZIP，也不执行主题构建。将主题目录放入应用数据目录的 `themes/` 后，打开网页或刷新主题列表即可发现。全新安装首次启动时会复制仓库中的 `themes/basic-demo`；之后删除它不会自动恢复。

## 最小目录

```text
my-theme/
├── manifest.json
├── index.html
├── index.css
└── index.js
```

`manifest.json` 至少包含以下字段：

```json
{
  "id": "my-theme",
  "name": "My Theme",
  "version": "1.0.0",
  "entry": "index.html",
  "sdkVersion": "1"
}
```

入口必须位于主题目录内部，`id` 必须是单一路径片段且在所有主题中唯一。无效清单、路径越界或入口缺失的主题会被跳过。已选主题失效时网页主壳会尝试回退到 `basic-demo`；如果该主题也不存在，则显示无可用主题提示。

## 引入 SDK

主题运行在 `sandbox="allow-scripts"` 的 iframe 中，不能直接访问网页主壳 DOM。浏览器 ESM 版本由主壳所在服务暴露：

```js
import sdk from "/sdk.js";

sdk.subscribe((state) => {
  console.log(state.playback, state.lyrics, state.spectrumFrame);
});

sdk.onConnectionChange((state) => console.log("connection", state));
sdk.onError((result) => console.error("command failed", result.error));
sdk.togglePlayPause();
```

SDK v1 的稳定入口包括：

- `getState()`：取得最近一次完整状态；状态包含 `playback`、`lyrics`、`spectrumState`、`spectrumFrame` 和 `observedAtMs`。
- `subscribe(listener)`：订阅完整状态快照和增量合并后的更新，并返回取消订阅函数。
- `onConnectionChange(listener)`：接收 `connecting`、`connected`、`disconnected`、`error` 生命周期变化。
- `onError(listener)`：接收实际执行失败的播放指令结果。
- `getArtworkUrl(artworkId?)`：按 `artworkId` 生成带当前网页令牌的封面资源地址。
- `play()`、`pause()`、`togglePlayPause()`、`previousTrack()`、`nextTrack()`、`seek(positionMs)`：调用服务端公共播放控制。

`spectrum.frame` 为了控制网络消息量，单个 WebSocket 连接最多每 100ms 推送一次；窗口内只保留最新帧。播放、歌词和频谱状态事件不受此限制。

SDK 不负责主题扫描、主题选择、主题配置或主题样式。网页令牌只存在于当前网页访问地址中；不要将令牌写入主题持久化配置或发送到其他服务。

## 向下兼容

`sdkVersion` 用于提示兼容关系，普通小版本差异不会阻止加载。v1 已发布字段和方法保持兼容；新能力通过新增字段或方法提供。主题应对新增字段缺失保持容错，并避免依赖未列入 SDK 说明的主壳 DOM 结构。

可直接参考仓库中的 [`themes/basic-demo`](../themes/basic-demo/) 最小示例。复制该目录后，请修改 `manifest.json` 的 `id`、名称和版本。
