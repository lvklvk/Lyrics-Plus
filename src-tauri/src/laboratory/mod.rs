//! Laboratory Runtime
//!
//! 实验室把本机播放状态包装成一个可被 App 客户端和网页主题消费的统一服务。
//! 这里刻意只使用标准库实现 HTTP/WebSocket/mDNS 的最小协议层，避免引入第二套播放器
//! 或歌词运行时；现有的 `playerService`、歌词运行时和频谱服务仍然是唯一数据源。

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{
    Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream, ToSocketAddrs, UdpSocket,
};
use std::path::{Component, Path, PathBuf};
#[cfg(unix)]
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tauri::{AppHandle, Emitter, Manager};

use crate::config::{AppConfig, LaboratoryRole};
use crate::player::{
    control_playback, seek_playback, PlaybackAction, PlaybackArtwork, PlaybackSnapshot,
    PlaybackSpectrumFrame, PlaybackSpectrumState,
};
use crate::state::AppState;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MDNS_SERVICE: &str = "_lyrics-plus._tcp.local.";
const CLIENT_RECORDS_PREFERENCE: &str = "laboratory.clients";
const SERVER_RECORDS_PREFERENCE: &str = "laboratory.servers";
const MAX_HTTP_HEADER_BYTES: usize = 64 * 1024;
const CLIENT_RETRY_LIMIT: usize = 3;
const BASIC_THEME_ID: &str = "basic-demo";
const BASIC_THEME_INITIALIZED_MARKER: &str = ".basic-demo-initialized-v1";
const BASIC_THEME_MANIFEST: &str = include_str!("../../../themes/basic-demo/manifest.json");
const BASIC_THEME_HTML: &str = include_str!("../../../themes/basic-demo/index.html");
const BASIC_THEME_CSS: &str = include_str!("../../../themes/basic-demo/index.css");
const BASIC_THEME_JS: &str = include_str!("../../../themes/basic-demo/index.js");
// 本机仍按约 30 FPS 采集频谱；这里只限制每个外部 WebSocket 连接的发送频率。
const SPECTRUM_PUSH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryStateSnapshot {
    pub playback: PlaybackSnapshot,
    pub lyrics: crate::lyrics::LyricsRuntimeSnapshot,
    pub spectrum_state: PlaybackSpectrumState,
    pub spectrum_frame: PlaybackSpectrumFrame,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaboratoryPhase {
    Stopped,
    Starting,
    Running,
    Connecting,
    Reconnecting,
    Error,
}

fn laboratory_phase_is_active(phase: LaboratoryPhase) -> bool {
    matches!(
        phase,
        LaboratoryPhase::Starting
            | LaboratoryPhase::Running
            | LaboratoryPhase::Connecting
            | LaboratoryPhase::Reconnecting
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryServerRecord {
    pub server_id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub protocol_version: u16,
    pub requires_password: bool,
    pub web_available: bool,
    pub last_connected_at_ms: Option<u64>,
    pub discovered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryClientRecord {
    pub client_id: String,
    pub name: String,
    pub online: bool,
    pub last_connected_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryThemeInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    pub sdk_version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryWebAddress {
    pub address: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryStatus {
    pub role: LaboratoryRole,
    pub phase: LaboratoryPhase,
    pub running: bool,
    pub message: Option<String>,
    pub server_id: String,
    pub client_id: String,
    pub server_address: Option<String>,
    pub web_addresses: Vec<LaboratoryWebAddress>,
    pub server_password_enabled: bool,
    pub clients: Vec<LaboratoryClientRecord>,
    pub recent_servers: Vec<LaboratoryServerRecord>,
    pub themes: Vec<LaboratoryThemeInfo>,
    pub remote_state: Option<LaboratoryStateSnapshot>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryServerSettingsInput {
    pub name: String,
    pub port: u16,
    pub discovery_enabled: bool,
    pub web_enabled: bool,
    pub debounce_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryClientSettingsInput {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaboratoryConnectInput {
    pub server_id: Option<String>,
    pub name: String,
    pub address: String,
    pub port: u16,
    pub requires_password: bool,
    pub web_available: bool,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Clone)]
struct RuntimeView {
    role: LaboratoryRole,
    phase: LaboratoryPhase,
    message: Option<String>,
    server_address: Option<String>,
    web_addresses: Vec<LaboratoryWebAddress>,
    clients: Vec<LaboratoryClientRecord>,
    recent_servers: Vec<LaboratoryServerRecord>,
}

impl RuntimeView {
    fn new(role: LaboratoryRole) -> Self {
        Self {
            role,
            phase: LaboratoryPhase::Stopped,
            message: None,
            server_address: None,
            web_addresses: Vec::new(),
            clients: Vec::new(),
            recent_servers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CredentialFile {
    server_id: String,
    client_id: String,
    server_password: Option<String>,
    web_token: Option<String>,
    #[serde(default)]
    client_passwords: HashMap<String, String>,
}

impl CredentialFile {
    fn load(path: &Path) -> Result<Self, String> {
        let mut credentials = if path.exists() {
            let raw =
                fs::read_to_string(path).map_err(|error| format!("读取实验室凭据失败：{error}"))?;
            serde_json::from_str::<Self>(&raw)
                .map_err(|error| format!("解析实验室凭据失败：{error}"))?
        } else {
            Self::default()
        };
        let mut changed = false;
        if credentials.server_id.trim().is_empty() {
            credentials.server_id = random_id("server");
            changed = true;
        }
        if credentials.client_id.trim().is_empty() {
            credentials.client_id = random_id("client");
            changed = true;
        }
        if changed || !path.exists() {
            write_credentials(path, &credentials)?;
        } else {
            set_credentials_permissions(path)?;
        }
        Ok(credentials)
    }

    fn ensure_web_token(&mut self, path: &Path) -> Result<String, String> {
        if let Some(token) = self
            .web_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            return Ok(token.to_owned());
        }
        let token = random_token();
        self.web_token = Some(token.clone());
        write_credentials(path, self)?;
        Ok(token)
    }
}

fn write_credentials(path: &Path, credentials: &CredentialFile) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "实验室凭据目录无效".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建实验室凭据目录失败：{error}"))?;
    let temporary = path.with_extension("json.tmp");
    let raw = serde_json::to_string_pretty(credentials)
        .map_err(|error| format!("序列化实验室凭据失败：{error}"))?;
    fs::write(&temporary, raw).map_err(|error| format!("写入实验室凭据失败：{error}"))?;
    set_credentials_permissions(&temporary)?;
    fs::rename(&temporary, path).map_err(|error| format!("替换实验室凭据失败：{error}"))
}

#[cfg(unix)]
fn set_credentials_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("限制实验室凭据权限失败：{error}"))
}

#[cfg(not(unix))]
fn set_credentials_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        if file.read_exact(&mut bytes).is_ok() {
            return bytes;
        }
    }
    let mut seed = now_ms() ^ (std::process::id() as u64);
    for byte in &mut bytes {
        seed ^= seed << 7;
        seed ^= seed >> 9;
        seed ^= seed << 8;
        *byte = seed as u8;
    }
    bytes
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

fn random_id(prefix: &str) -> String {
    format!("{prefix}-{}", URL_SAFE_NO_PAD.encode(random_bytes::<12>()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn install_basic_theme_once(app_dir: &Path, themes_dir: &Path) -> Result<(), String> {
    // 首次启动只写入一次；标记存在后尊重用户对主题目录的删除或修改。
    let marker = app_dir.join(BASIC_THEME_INITIALIZED_MARKER);
    if marker.exists() {
        return Ok(());
    }

    let theme_dir = themes_dir.join(BASIC_THEME_ID);
    if !theme_dir.exists() {
        fs::create_dir_all(&theme_dir)
            .map_err(|error| format!("创建基础 Demo 主题目录失败：{error}"))?;
        fs::write(theme_dir.join("manifest.json"), BASIC_THEME_MANIFEST)
            .map_err(|error| format!("写入基础 Demo manifest 失败：{error}"))?;
        fs::write(theme_dir.join("index.html"), BASIC_THEME_HTML)
            .map_err(|error| format!("写入基础 Demo HTML 失败：{error}"))?;
        fs::write(theme_dir.join("index.css"), BASIC_THEME_CSS)
            .map_err(|error| format!("写入基础 Demo CSS 失败：{error}"))?;
        fs::write(theme_dir.join("index.js"), BASIC_THEME_JS)
            .map_err(|error| format!("写入基础 Demo JavaScript 失败：{error}"))?;
    }

    fs::write(marker, b"initialized\n")
        .map_err(|error| format!("记录基础 Demo 初始化状态失败：{error}"))
}

pub struct LaboratoryRuntime {
    app_dir: PathBuf,
    credentials_path: PathBuf,
    credentials: Mutex<CredentialFile>,
    view: Mutex<RuntimeView>,
    stop_flag: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
    server_thread: Mutex<Option<thread::JoinHandle<()>>>,
    client_thread: Mutex<Option<thread::JoinHandle<()>>>,
    remote_state: RwLock<Option<LaboratoryStateSnapshot>>,
    client_socket: Mutex<Option<Arc<Mutex<TcpStream>>>>,
    server_connections: Mutex<HashMap<String, Arc<Mutex<TcpStream>>>>,
    app_auth_generation: AtomicU64,
    web_auth_generation: AtomicU64,
    command_generation: AtomicU64,
    pending_command: Mutex<Option<PendingCommand>>,
}

#[derive(Debug, Clone)]
struct PendingCommand {
    request_id: String,
    action: PlaybackAction,
    position_ms: Option<u64>,
}

impl LaboratoryRuntime {
    pub fn new(app_dir: &Path, role: LaboratoryRole) -> Result<Self, String> {
        let credentials_path = app_dir.join("laboratory-credentials.json");
        let credentials = CredentialFile::load(&credentials_path)?;
        let themes_dir = app_dir.join("themes");
        fs::create_dir_all(&themes_dir)
            .map_err(|error| format!("创建实验室主题目录失败：{error}"))?;
        install_basic_theme_once(app_dir, &themes_dir)?;
        let view = RuntimeView::new(role);
        Ok(Self {
            app_dir: app_dir.to_owned(),
            credentials_path,
            credentials: Mutex::new(credentials),
            view: Mutex::new(view),
            stop_flag: Mutex::new(None),
            server_thread: Mutex::new(None),
            client_thread: Mutex::new(None),
            remote_state: RwLock::new(None),
            client_socket: Mutex::new(None),
            server_connections: Mutex::new(HashMap::new()),
            app_auth_generation: AtomicU64::new(0),
            web_auth_generation: AtomicU64::new(0),
            command_generation: AtomicU64::new(0),
            pending_command: Mutex::new(None),
        })
    }

    pub fn server_id(&self) -> String {
        self.credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .server_id
            .clone()
    }

    pub fn client_id(&self) -> String {
        self.credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .client_id
            .clone()
    }

    pub fn is_running(&self) -> bool {
        laboratory_phase_is_active(
            self.view
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .phase,
        )
    }

    pub fn is_client(&self) -> bool {
        self.view
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .role
            == LaboratoryRole::Client
    }

    pub fn remote_snapshot(&self) -> Option<LaboratoryStateSnapshot> {
        self.remote_state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn status(&self, app: &AppHandle) -> LaboratoryStatus {
        let config = app.state::<AppState>().config.snapshot();
        let mut view = self
            .view
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        view.role = config.laboratory.role;
        view.clients = load_client_records(app);
        view.recent_servers = load_server_records(app);
        if view.role == LaboratoryRole::Server && laboratory_phase_is_active(view.phase) {
            if config.laboratory.server.web_enabled {
                let token = self
                    .credentials
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .web_token
                    .clone();
                view.web_addresses = token
                    .as_deref()
                    .map(|token| web_addresses(config.laboratory.server.port, token))
                    .unwrap_or_default();
            } else {
                view.web_addresses.clear();
            }
        }
        if let Some(remote) = self.remote_snapshot() {
            let mut result = laboratory_status_from_view(&view, self);
            result.remote_state = Some(remote);
            result.themes = self.scan_themes();
            return result;
        }
        let mut result = laboratory_status_from_view(&view, self);
        result.themes = self.scan_themes();
        result
    }

    pub fn emit_status(&self, app: &AppHandle) {
        let status = self.status(app);
        let _ = app.emit("laboratory://status", &status);
    }

    pub fn auto_start_if_enabled(self: &Arc<Self>, app: &AppHandle) {
        let should_start = app
            .state::<AppState>()
            .config
            .snapshot()
            .laboratory
            .auto_start;
        if !should_start {
            return;
        }
        let runtime = Arc::clone(self);
        let app = app.clone();
        thread::spawn(move || {
            if let Err(error) = runtime.start(&app) {
                runtime.set_phase(&app, LaboratoryPhase::Error, Some(error));
            }
        });
    }

    pub fn start(self: &Arc<Self>, app: &AppHandle) -> Result<(), String> {
        self.stop(app);
        let config = app.state::<AppState>().config.snapshot();
        let role = config.laboratory.role;
        self.view
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .role = role;
        self.set_phase(app, LaboratoryPhase::Starting, None);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *self
            .stop_flag
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(stop.clone());
        let result = match role {
            LaboratoryRole::Server => self.start_server(app, &config, stop),
            LaboratoryRole::Client => self.start_client(app, &config, stop),
        };
        if let Err(error) = &result {
            self.set_phase(app, LaboratoryPhase::Error, Some(error.clone()));
        }
        result
    }

    pub fn stop(&self, app: &AppHandle) {
        let was_client = self.is_client();
        if let Some(flag) = self
            .stop_flag
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            flag.store(true, Ordering::Release);
        }
        if let Some(socket) = self
            .client_socket
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = socket
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .shutdown(Shutdown::Both);
        }
        for socket in self
            .server_connections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
            .map(|(_, socket)| socket)
        {
            let _ = socket
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .shutdown(Shutdown::Both);
        }
        if let Some(thread) = self
            .server_thread
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = thread.join();
        }
        if let Some(thread) = self
            .client_thread
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = thread.join();
        }
        let _ = mark_clients_offline(app);
        self.app_auth_generation.fetch_add(1, Ordering::SeqCst);
        self.web_auth_generation.fetch_add(1, Ordering::SeqCst);
        self.command_generation.fetch_add(1, Ordering::SeqCst);
        *self
            .pending_command
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        *self
            .remote_state
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
        self.set_phase(app, LaboratoryPhase::Stopped, None);
        if was_client {
            apply_remote_disconnect(self, app);
        }
    }

    pub fn update_role(&self, app: &AppHandle, role: LaboratoryRole) -> Result<(), String> {
        self.stop(app);
        let config = app
            .state::<AppState>()
            .config
            .update(|config| config.laboratory.role = role)?;
        self.view
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .role = role;
        self.set_phase(app, LaboratoryPhase::Stopped, None);
        if role == LaboratoryRole::Client {
            app.state::<AppState>().spectrum.suspend_capture(app);
            apply_remote_disconnect(self, app);
        }
        let _ = app.emit("config://changed", &config);
        self.emit_status(app);
        Ok(())
    }

    pub fn update_server_settings(
        self: &Arc<Self>,
        app: &AppHandle,
        input: LaboratoryServerSettingsInput,
    ) -> Result<(), String> {
        if !(1_024..=65_535).contains(&input.port) {
            return Err("实验室服务端端口必须在 1024 到 65535 之间".into());
        }
        let previous = app.state::<AppState>().config.snapshot();
        let was_running = self.is_running();
        let debounce_ms = input.debounce_ms.clamp(50, 10_000);
        let config = app.state::<AppState>().config.update(|config| {
            config.laboratory.server.name = input.name.trim().to_owned();
            config.laboratory.server.port = input.port;
            config.laboratory.server.discovery_enabled = input.discovery_enabled;
            config.laboratory.server.web_enabled = input.web_enabled;
            config.laboratory.server.debounce_ms = debounce_ms;
        })?;
        let port_changed = previous.laboratory.server.port != config.laboratory.server.port;
        let web_setting_changed =
            previous.laboratory.server.web_enabled != config.laboratory.server.web_enabled;
        if was_running && config.laboratory.role == LaboratoryRole::Server && port_changed {
            self.start(app)?;
        } else if was_running && config.laboratory.role == LaboratoryRole::Server {
            if web_setting_changed {
                self.web_auth_generation.fetch_add(1, Ordering::SeqCst);
            }
            let address = format!("{}:{}", local_ip(), config.laboratory.server.port);
            let web_addresses = if config.laboratory.server.web_enabled {
                let mut credentials = self
                    .credentials
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let token = credentials.ensure_web_token(&self.credentials_path)?;
                web_addresses(config.laboratory.server.port, &token)
            } else {
                Vec::new()
            };
            let mut view = self.view.lock().unwrap_or_else(|error| error.into_inner());
            view.server_address = Some(address);
            view.web_addresses = web_addresses;
        }
        let _ = app.emit("config://changed", &config);
        self.emit_status(app);
        Ok(())
    }

    pub fn update_client_settings(
        &self,
        app: &AppHandle,
        input: LaboratoryClientSettingsInput,
    ) -> Result<(), String> {
        let config = app.state::<AppState>().config.update(|config| {
            config.laboratory.client.name = input.name.trim().to_owned();
        })?;
        let _ = app.emit("config://changed", &config);
        self.emit_status(app);
        Ok(())
    }

    pub fn set_auto_start(&self, app: &AppHandle, enabled: bool) -> Result<(), String> {
        let config = app
            .state::<AppState>()
            .config
            .update(|config| config.laboratory.auto_start = enabled)?;
        let _ = app.emit("config://changed", &config);
        self.emit_status(app);
        Ok(())
    }

    pub fn set_server_password(&self, app: &AppHandle, password: String) -> Result<(), String> {
        let password = password.trim().to_owned();
        let mut credentials = self
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        credentials.server_password = (!password.is_empty()).then_some(password);
        write_credentials(&self.credentials_path, &credentials)?;
        drop(credentials);
        self.app_auth_generation.fetch_add(1, Ordering::SeqCst);
        self.emit_status(app);
        Ok(())
    }

    pub fn reset_web_token(&self, app: &AppHandle) -> Result<String, String> {
        let mut credentials = self
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        credentials.web_token = Some(random_token());
        let token = credentials.web_token.clone().expect("token just inserted");
        write_credentials(&self.credentials_path, &credentials)?;
        drop(credentials);
        self.web_auth_generation.fetch_add(1, Ordering::SeqCst);
        if self.is_running() {
            let config = app.state::<AppState>().config.snapshot();
            if config.laboratory.role == LaboratoryRole::Server
                && config.laboratory.server.web_enabled
            {
                let next_web_addresses = web_addresses(config.laboratory.server.port, &token);
                self.view
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .web_addresses = next_web_addresses;
            }
        }
        self.emit_status(app);
        Ok(token)
    }

    pub fn server_password_enabled(&self) -> bool {
        self.credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .server_password
            .as_deref()
            .is_some_and(|password| !password.is_empty())
    }

    pub fn save_client_password(&self, server_id: &str, password: &str) -> Result<(), String> {
        let password = password.trim();
        let mut credentials = self
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if password.is_empty() {
            credentials.client_passwords.remove(server_id);
        } else {
            credentials
                .client_passwords
                .insert(server_id.to_owned(), password.to_owned());
        }
        write_credentials(&self.credentials_path, &credentials)
    }

    fn client_password(&self, server_id: &str) -> Option<String> {
        self.credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .client_passwords
            .get(server_id)
            .cloned()
    }

    fn set_phase(&self, app: &AppHandle, phase: LaboratoryPhase, message: Option<String>) {
        let mut view = self.view.lock().unwrap_or_else(|error| error.into_inner());
        view.phase = phase;
        view.message = message;
        if phase == LaboratoryPhase::Stopped {
            view.server_address = None;
            view.web_addresses.clear();
            view.clients
                .iter_mut()
                .for_each(|client| client.online = false);
        }
        drop(view);
        self.emit_status(app);
    }

    fn start_server(
        self: &Arc<Self>,
        app: &AppHandle,
        config: &AppConfig,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        self.view
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .role = LaboratoryRole::Server;
        let listener = TcpListener::bind(("0.0.0.0", config.laboratory.server.port))
            .map_err(|error| format!("启动实验室服务端失败：{error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("设置实验室服务端失败：{error}"))?;
        let address = format!("{}:{}", local_ip(), config.laboratory.server.port);
        let web_addresses = if config.laboratory.server.web_enabled {
            let mut credentials = self
                .credentials
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let token = credentials.ensure_web_token(&self.credentials_path)?;
            web_addresses(config.laboratory.server.port, &token)
        } else {
            Vec::new()
        };
        {
            let mut view = self.view.lock().unwrap_or_else(|error| error.into_inner());
            view.role = LaboratoryRole::Server;
            view.phase = LaboratoryPhase::Running;
            view.message = None;
            view.server_address = Some(address);
            view.web_addresses = web_addresses;
        }
        self.emit_status(app);

        let runtime = Arc::clone(self);
        let app_handle = app.clone();
        let server_thread =
            thread::spawn(move || server_accept_loop(runtime, app_handle, listener, stop));
        *self
            .server_thread
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(server_thread);
        if config.laboratory.server.discovery_enabled {
            let runtime = Arc::clone(self);
            let app_handle = app.clone();
            let discovery_stop = self
                .stop_flag
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_ref()
                .cloned()
                .expect("server stop flag set before discovery");
            thread::spawn(move || discovery_advertiser_loop(runtime, app_handle, discovery_stop));
        }
        Ok(())
    }

    fn start_client(
        self: &Arc<Self>,
        app: &AppHandle,
        config: &AppConfig,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), String> {
        self.view
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .role = LaboratoryRole::Client;
        let last_server_id = config.laboratory.client.last_server_id.clone();
        let mut servers = load_server_records(app);
        let mut record = last_server_id
            .as_deref()
            .and_then(|id| servers.iter().find(|server| server.server_id == id))
            .cloned();
        if record.is_none() {
            for discovered in discover_servers(Duration::from_millis(700)) {
                let _ = save_server_record(app, discovered);
            }
            servers = load_server_records(app);
            record = last_server_id
                .as_deref()
                .and_then(|id| servers.iter().find(|server| server.server_id == id))
                .cloned();
            if let Ok(mut view) = self.view.lock() {
                view.recent_servers = servers;
            }
            self.emit_status(app);
        }
        let Some(record) = record else {
            self.set_phase(
                app,
                LaboratoryPhase::Error,
                Some("没有可自动连接的最近服务端，请先扫描或手动连接".into()),
            );
            return Ok(());
        };
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let password = self.client_password(&record.server_id).unwrap_or_default();
        let runtime = Arc::clone(self);
        let app_handle = app.clone();
        let client_thread = thread::spawn(move || {
            client_connection_loop(runtime, app_handle, record, password, stop)
        });
        *self
            .client_thread
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(client_thread);
        Ok(())
    }

    pub fn connect_to_server(
        self: &Arc<Self>,
        app: &AppHandle,
        input: LaboratoryConnectInput,
    ) -> Result<(), String> {
        let LaboratoryConnectInput {
            server_id,
            name,
            address,
            port,
            requires_password,
            web_available,
            password,
        } = input;
        let address = address.trim().to_owned();
        let name = name.trim().to_owned();
        if address.is_empty() {
            return Err("服务端地址不能为空".into());
        }
        if !(1_024..=65_535).contains(&port) {
            return Err("服务端端口必须在 1024 到 65535 之间".into());
        }
        let server_id = server_id.unwrap_or_else(|| format!("manual:{address}:{port}"));
        let password = if requires_password && password.trim().is_empty() {
            self.client_password(&server_id).unwrap_or_default()
        } else {
            password
        };
        let record = LaboratoryServerRecord {
            server_id: server_id.clone(),
            name: if name.is_empty() {
                address.clone()
            } else {
                name
            },
            address,
            port,
            protocol_version: PROTOCOL_VERSION,
            requires_password,
            web_available,
            last_connected_at_ms: Some(now_ms()),
            discovered: false,
        };
        save_server_record(app, record.clone())?;
        self.save_client_password(&server_id, &password)?;
        let config = app.state::<AppState>().config.update(|config| {
            config.laboratory.role = LaboratoryRole::Client;
            config.laboratory.client.last_server_id = Some(server_id);
        })?;
        let _ = app.emit("config://changed", &config);
        self.stop(app);
        self.view
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .role = LaboratoryRole::Client;
        app.state::<AppState>().spectrum.suspend_capture(app);
        apply_remote_disconnect(self, app);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        *self
            .stop_flag
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(stop.clone());
        self.set_phase(app, LaboratoryPhase::Connecting, None);
        let runtime = Arc::clone(self);
        let app_handle = app.clone();
        let client_thread = thread::spawn(move || {
            client_connection_loop(runtime, app_handle, record, password, stop)
        });
        *self
            .client_thread
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(client_thread);
        Ok(())
    }

    pub fn retry_connection(self: &Arc<Self>, app: &AppHandle) -> Result<(), String> {
        let config = app.state::<AppState>().config.snapshot();
        let server_id = config
            .laboratory
            .client
            .last_server_id
            .ok_or_else(|| "没有最近连接的服务端".to_string())?;
        let record = load_server_records(app)
            .into_iter()
            .find(|record| record.server_id == server_id)
            .ok_or_else(|| "最近服务端记录不存在".to_string())?;
        let password = self.client_password(&server_id).unwrap_or_default();
        self.connect_to_server(
            app,
            LaboratoryConnectInput {
                server_id: Some(record.server_id),
                name: record.name,
                address: record.address,
                port: record.port,
                requires_password: record.requires_password,
                web_available: record.web_available,
                password,
            },
        )
    }

    pub fn scan_servers(self: &Arc<Self>, app: &AppHandle) -> Vec<LaboratoryServerRecord> {
        let config = app.state::<AppState>().config.snapshot();
        let active_server_id = (config.laboratory.role == LaboratoryRole::Client)
            .then(|| config.laboratory.client.last_server_id)
            .flatten();
        let previous_records = load_server_records(app);
        let mut reconnect_record = None;
        let discovered = discover_servers(Duration::from_millis(700));
        for record in discovered {
            if active_server_id.as_deref() == Some(record.server_id.as_str())
                && previous_records
                    .iter()
                    .find(|item| item.server_id == record.server_id)
                    .is_some_and(|previous| {
                        previous.address != record.address || previous.port != record.port
                    })
            {
                reconnect_record = Some(record.clone());
            }
            let _ = save_server_record(app, record);
        }
        if self.is_running() {
            if let Some(record) = reconnect_record {
                let password = self.client_password(&record.server_id).unwrap_or_default();
                let _ = self.connect_to_server(
                    app,
                    LaboratoryConnectInput {
                        server_id: Some(record.server_id.clone()),
                        name: record.name.clone(),
                        address: record.address.clone(),
                        port: record.port,
                        requires_password: record.requires_password,
                        web_available: record.web_available,
                        password,
                    },
                );
            }
        }
        let records = load_server_records(app);
        if let Ok(mut view) = self.view.lock() {
            view.recent_servers = records.clone();
        }
        self.emit_status(app);
        records
    }

    pub fn kick_client(&self, app: &AppHandle, client_id: &str) -> Result<(), String> {
        if let Some(socket) = self
            .server_connections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(client_id)
        {
            let _ = socket
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .shutdown(Shutdown::Both);
        }
        update_client_record(app, client_id, false, None)?;
        self.emit_status(app);
        Ok(())
    }

    pub fn forget_client(&self, app: &AppHandle, client_id: &str) -> Result<(), String> {
        let state = app.state::<AppState>();
        let records = load_client_records(app)
            .into_iter()
            .filter(|record| record.client_id != client_id || record.online)
            .collect::<Vec<_>>();
        state.storage.set_preference(
            CLIENT_RECORDS_PREFERENCE,
            &serde_json::to_string(&records).unwrap_or_else(|_| "[]".into()),
        )?;
        self.emit_status(app);
        Ok(())
    }

    pub fn send_playback_command(
        &self,
        action: PlaybackAction,
        position_ms: Option<u64>,
    ) -> Result<(), String> {
        if !self.is_client() {
            return Err("当前不是实验室客户端".into());
        }
        let socket = self
            .client_socket
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
            .ok_or_else(|| "实验室客户端尚未连接".to_string())?;
        let payload = json!({
            "action": action,
            "positionMs": position_ms,
        });
        let envelope = envelope("playback.command", Some(random_id("command")), payload);
        let mut socket = socket.lock().unwrap_or_else(|error| error.into_inner());
        send_ws_text(
            &mut socket,
            &envelope,
            true,
        )
        .map_err(|error| format!("发送远程播放指令失败：{error}"))
    }

    pub fn fetch_artwork(
        &self,
        app: &AppHandle,
        artwork_id: &str,
    ) -> Result<Option<PlaybackArtwork>, String> {
        let server_id = app
            .state::<AppState>()
            .config
            .snapshot()
            .laboratory
            .client
            .last_server_id;
        let record = load_server_records(app)
            .into_iter()
            .find(|record| Some(record.server_id.as_str()) == server_id.as_deref())
            .ok_or_else(|| "没有可用的实验室服务端".to_string())?;
        let password = self.client_password(&record.server_id).unwrap_or_default();
        fetch_remote_artwork(&record, &self.client_id(), &password, artwork_id)
    }

    fn submit_server_command(
        self: &Arc<Self>,
        app: &AppHandle,
        request_id: String,
        action: PlaybackAction,
        position_ms: Option<u64>,
    ) -> Result<(), String> {
        let debounce_ms = app
            .state::<AppState>()
            .config
            .snapshot()
            .laboratory
            .server
            .debounce_ms
            .clamp(50, 10_000);
        let generation = self.command_generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self
            .pending_command
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(PendingCommand {
            request_id,
            action,
            position_ms,
        });
        let runtime = Arc::clone(self);
        let app = app.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(debounce_ms));
            if runtime.command_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let pending = runtime
                .pending_command
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            let Some(pending) = pending else {
                return;
            };
            let request_id = pending.request_id.clone();
            let result = execute_server_command(&app, pending.action, pending.position_ms);
            let payload = match result {
                Ok(()) => json!({ "requestId": request_id.clone(), "ok": true }),
                Err(error) => {
                    json!({ "requestId": request_id.clone(), "ok": false, "error": error })
                }
            };
            runtime.broadcast_app_message(&envelope("command.result", Some(request_id), payload));
        });
        Ok(())
    }

    fn broadcast_app_message(&self, message: &str) {
        let mut failed = Vec::new();
        if let Ok(connections) = self.server_connections.lock() {
            for (id, socket) in connections.iter() {
                let result = send_ws_text(
                    &mut *socket.lock().unwrap_or_else(|error| error.into_inner()),
                    message,
                    false,
                );
                if result.is_err() {
                    failed.push(id.clone());
                }
            }
        }
        if let Ok(mut connections) = self.server_connections.lock() {
            for id in failed {
                connections.remove(&id);
            }
        }
    }

    fn scan_themes(&self) -> Vec<LaboratoryThemeInfo> {
        scan_theme_directories(&self.app_dir.join("themes"))
    }
}

fn laboratory_status_from_view(
    view: &RuntimeView,
    runtime: &LaboratoryRuntime,
) -> LaboratoryStatus {
    let credentials = runtime
        .credentials
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    LaboratoryStatus {
        role: view.role,
        phase: view.phase,
        running: laboratory_phase_is_active(view.phase),
        message: view.message.clone(),
        server_id: credentials.server_id.clone(),
        client_id: credentials.client_id.clone(),
        server_address: view.server_address.clone(),
        web_addresses: view.web_addresses.clone(),
        server_password_enabled: credentials
            .server_password
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        clients: view.clients.clone(),
        recent_servers: view.recent_servers.clone(),
        themes: Vec::new(),
        remote_state: None,
    }
}

fn execute_server_command(
    app: &AppHandle,
    action: PlaybackAction,
    position_ms: Option<u64>,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let snapshot = state
        .last_snapshot
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let selection = *state
        .selection
        .read()
        .unwrap_or_else(|error| error.into_inner());
    match position_ms {
        Some(position_ms) => seek_playback(position_ms, selection, &snapshot, &state.system_media),
        None => control_playback(action, selection, &snapshot, &state.system_media),
    }
}

pub fn current_state_snapshot(app: &AppHandle) -> LaboratoryStateSnapshot {
    let state = app.state::<AppState>();
    let playback = state
        .last_snapshot
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let lyrics = state
        .lyrics_runtime
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    LaboratoryStateSnapshot {
        playback,
        lyrics,
        spectrum_state: state.spectrum.state(),
        spectrum_frame: state.spectrum.frame(),
        observed_at_ms: now_ms(),
    }
}

fn load_client_records(app: &AppHandle) -> Vec<LaboratoryClientRecord> {
    app.state::<AppState>()
        .storage
        .get_preference(CLIENT_RECORDS_PREFERENCE)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn load_server_records(app: &AppHandle) -> Vec<LaboratoryServerRecord> {
    app.state::<AppState>()
        .storage
        .get_preference(SERVER_RECORDS_PREFERENCE)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_server_record(app: &AppHandle, record: LaboratoryServerRecord) -> Result<(), String> {
    let mut records = load_server_records(app);
    if let Some(existing) = records
        .iter_mut()
        .find(|item| item.server_id == record.server_id)
    {
        let last_connected_at_ms = record
            .last_connected_at_ms
            .or(existing.last_connected_at_ms);
        *existing = record;
        existing.last_connected_at_ms = last_connected_at_ms;
    } else {
        records.push(record);
    }
    records.sort_by(|left, right| right.last_connected_at_ms.cmp(&left.last_connected_at_ms));
    records.truncate(32);
    app.state::<AppState>().storage.set_preference(
        SERVER_RECORDS_PREFERENCE,
        &serde_json::to_string(&records).map_err(|error| error.to_string())?,
    )
}

fn update_client_record(
    app: &AppHandle,
    client_id: &str,
    online: bool,
    name: Option<&str>,
) -> Result<(), String> {
    let mut records = load_client_records(app);
    if let Some(existing) = records.iter_mut().find(|item| item.client_id == client_id) {
        existing.online = online;
        if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
            existing.name = name.trim().to_owned();
        }
        if online {
            existing.last_connected_at_ms = Some(now_ms());
        }
    } else {
        records.push(LaboratoryClientRecord {
            client_id: client_id.to_owned(),
            name: name.unwrap_or("Lyrics Plus 客户端").to_owned(),
            online,
            last_connected_at_ms: online.then_some(now_ms()),
        });
    }
    app.state::<AppState>().storage.set_preference(
        CLIENT_RECORDS_PREFERENCE,
        &serde_json::to_string(&records).map_err(|error| error.to_string())?,
    )
}

fn mark_clients_offline(app: &AppHandle) -> Result<(), String> {
    let mut records = load_client_records(app);
    let mut changed = false;
    for record in &mut records {
        if record.online {
            record.online = false;
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    app.state::<AppState>().storage.set_preference(
        CLIENT_RECORDS_PREFERENCE,
        &serde_json::to_string(&records).map_err(|error| error.to_string())?,
    )
}

fn server_accept_loop(
    runtime: Arc<LaboratoryRuntime>,
    app: AppHandle,
    listener: TcpListener,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = stream.set_nonblocking(false) {
                    log::debug!("实验室连接结束：设置连接为阻塞模式失败：{error}");
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                }
                let runtime = Arc::clone(&runtime);
                let app = app.clone();
                thread::spawn(move || {
                    if let Err(error) = handle_server_connection(runtime, app, stream) {
                        log::debug!("实验室连接结束：{error}");
                    }
                });
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                thread::sleep(Duration::from_millis(40));
            }
            Err(error) => {
                log::warn!("实验室服务端接收连接失败：{error}");
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

fn read_http_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    let mut buffer = Vec::with_capacity(2048);
    let mut chunk = [0_u8; 1024];
    while buffer.len() < MAX_HTTP_HEADER_BYTES {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let raw = String::from_utf8_lossy(&buffer);
    let mut lines = raw.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "空 HTTP 请求"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    if method.is_empty() || path.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP 请求行无效",
        ));
    }
    let mut headers = HashMap::new();
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
    })
}

fn handle_server_connection(
    runtime: Arc<LaboratoryRuntime>,
    app: AppHandle,
    mut stream: TcpStream,
) -> Result<(), String> {
    let request =
        read_http_request(&mut stream).map_err(|error| format!("读取实验室请求失败：{error}"))?;
    if request.method != "GET" {
        write_http_response(
            &mut stream,
            405,
            "Method Not Allowed",
            "text/plain; charset=utf-8",
            b"method not allowed",
        )
        .map_err(|error| error.to_string())?;
        return Ok(());
    }
    let (path, query) = split_path_query(&request.path);
    if path == "/ws" {
        let web = query.get("token").is_some();
        if web {
            if !runtime.web_enabled(&app) {
                write_http_response(
                    &mut stream,
                    404,
                    "Not Found",
                    "text/plain; charset=utf-8",
                    b"web service disabled",
                )
                .map_err(|error| error.to_string())?;
                return Ok(());
            }
            let token = query.get("token").map(String::as_str).unwrap_or_default();
            if !runtime.valid_web_token(token) {
                write_http_response(
                    &mut stream,
                    401,
                    "Unauthorized",
                    "text/plain; charset=utf-8",
                    b"invalid token",
                )
                .map_err(|error| error.to_string())?;
                return Ok(());
            }
        }
        websocket_handshake(&mut stream, &request.headers)
            .map_err(|error| format!("WebSocket 握手失败：{error}"))?;
        stream
            .set_read_timeout(Some(Duration::from_millis(5)))
            .map_err(|error| error.to_string())?;
        return server_websocket_loop(runtime, app, stream, web);
    }
    handle_http_request(&runtime, &app, &mut stream, &request, path, query)
}

fn handle_http_request(
    runtime: &LaboratoryRuntime,
    app: &AppHandle,
    stream: &mut TcpStream,
    request: &HttpRequest,
    path: &str,
    query: HashMap<String, String>,
) -> Result<(), String> {
    let token_valid = runtime.web_enabled(app)
        && query
            .get("token")
            .is_some_and(|token| runtime.valid_web_token(token));
    let referer_token_valid = runtime.web_enabled(app)
        && request.headers.get("referer").is_some_and(|referer| {
            let (_, referer_query) = split_path_query(referer);
            referer_query
                .get("token")
                .is_some_and(|token| runtime.valid_web_token(token))
        });
    let web_access_valid = token_valid || referer_token_valid;
    let app_valid = runtime.valid_http_client(&request.headers);
    let is_artwork = path.starts_with("/artwork/");
    if (is_artwork && !web_access_valid && !app_valid) || (!is_artwork && !web_access_valid) {
        write_http_response(
            stream,
            401,
            "Unauthorized",
            "text/plain; charset=utf-8",
            b"web access token required",
        )
        .map_err(|error| error.to_string())?;
        return Ok(());
    }
    match path {
        "/" => {
            let token = query.get("token").cloned().unwrap_or_default();
            let body = web_shell_html(&token);
            write_http_response(
                stream,
                200,
                "OK",
                "text/html; charset=utf-8",
                body.as_bytes(),
            )
            .map_err(|error| error.to_string())?;
        }
        "/sdk.js" => {
            write_http_response(
                stream,
                200,
                "OK",
                "text/javascript; charset=utf-8",
                THEME_SDK_JS.as_bytes(),
            )
            .map_err(|error| error.to_string())?;
        }
        "/themes" => {
            let body =
                serde_json::to_vec(&runtime.scan_themes()).map_err(|error| error.to_string())?;
            write_http_response(stream, 200, "OK", "application/json; charset=utf-8", &body)
                .map_err(|error| error.to_string())?;
        }
        _ if path.starts_with("/theme/") => {
            let relative = path.trim_start_matches("/theme/");
            let mut parts = relative.splitn(2, '/');
            let theme_id = percent_decode(parts.next().unwrap_or_default());
            let entry = percent_decode(parts.next().unwrap_or_default());
            let Some(file) = runtime.theme_file(&theme_id, &entry) else {
                write_http_response(
                    stream,
                    404,
                    "Not Found",
                    "text/plain; charset=utf-8",
                    b"theme file not found",
                )
                .map_err(|error| error.to_string())?;
                return Ok(());
            };
            let body = fs::read(&file).map_err(|error| format!("读取主题资源失败：{error}"))?;
            write_http_response(stream, 200, "OK", content_type(&file), &body)
                .map_err(|error| error.to_string())?;
        }
        _ if path.starts_with("/artwork/") => {
            let artwork_id = percent_decode(path.trim_start_matches("/artwork/"));
            let artwork = app.state::<AppState>().system_media.artwork(&artwork_id)?;
            let Some(artwork) = artwork else {
                write_http_response(
                    stream,
                    404,
                    "Not Found",
                    "text/plain; charset=utf-8",
                    b"artwork not found",
                )
                .map_err(|error| error.to_string())?;
                return Ok(());
            };
            if web_access_valid && !app_valid {
                let body = STANDARD
                    .decode(&artwork.data_base64)
                    .map_err(|error| format!("解码网页封面失败：{error}"))?;
                write_http_response(stream, 200, "OK", &artwork.mime_type, &body)
                    .map_err(|error| error.to_string())?;
            } else {
                let body = serde_json::to_vec(&artwork).map_err(|error| error.to_string())?;
                write_http_response(stream, 200, "OK", "application/json; charset=utf-8", &body)
                    .map_err(|error| error.to_string())?;
            }
        }
        _ => {
            write_http_response(
                stream,
                404,
                "Not Found",
                "text/plain; charset=utf-8",
                b"not found",
            )
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn split_path_query(path: &str) -> (&str, HashMap<String, String>) {
    let Some((path, raw_query)) = path.split_once('?') else {
        return (path, HashMap::new());
    };
    let query = raw_query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect();
    (path, query)
}

fn websocket_handshake(
    stream: &mut TcpStream,
    headers: &HashMap<String, String>,
) -> io::Result<()> {
    let key = headers
        .get("sec-websocket-key")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "缺少 Sec-WebSocket-Key"))?;
    let accept = STANDARD.encode(sha1_digest(
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    ));
    write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    )
}

fn envelope(message_type: &str, request_id: Option<String>, payload: Value) -> String {
    let mut value = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "type": message_type,
        "timestampMs": now_ms(),
        "payload": payload,
    });
    if let Some(request_id) = request_id {
        value["requestId"] = Value::String(request_id);
    }
    value.to_string()
}

fn send_ws_text(stream: &mut TcpStream, text: &str, masked: bool) -> io::Result<()> {
    let payload = text.as_bytes();
    let mut header = Vec::with_capacity(10);
    header.push(0x81);
    let mask_bit = if masked { 0x80 } else { 0 };
    match payload.len() {
        0..=125 => header.push(mask_bit | payload.len() as u8),
        126..=65_535 => {
            header.push(mask_bit | 126);
            header.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        _ => {
            header.push(mask_bit | 127);
            header.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
    }
    if masked {
        let mask = random_bytes::<4>();
        header.extend_from_slice(&mask);
        stream.write_all(&header)?;
        let mut encoded = payload.to_vec();
        for (index, byte) in encoded.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        stream.write_all(&encoded)
    } else {
        stream.write_all(&header)?;
        stream.write_all(payload)
    }
}

fn send_shared_ws_text(stream: &Arc<Mutex<TcpStream>>, text: &str, masked: bool) -> io::Result<()> {
    let mut stream = stream
        .lock()
        .map_err(|_| io::Error::other("WebSocket 写入锁不可用"))?;
    send_ws_text(&mut stream, text, masked)
}

fn read_ws_message(stream: &mut TcpStream) -> io::Result<Option<String>> {
    let mut header = [0_u8; 2];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if is_timeout(&error) => return Ok(None),
        Err(error) => return Err(error),
    }
    let opcode = header[0] & 0x0f;
    let masked = header[1] & 0x80 != 0;
    let mut length = u64::from(header[1] & 0x7f);
    if length == 126 {
        let mut bytes = [0_u8; 2];
        stream.read_exact(&mut bytes)?;
        length = u64::from(u16::from_be_bytes(bytes));
    } else if length == 127 {
        let mut bytes = [0_u8; 8];
        stream.read_exact(&mut bytes)?;
        length = u64::from_be_bytes(bytes);
    }
    if length > MAX_HTTP_HEADER_BYTES as u64 * 16 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "WebSocket 帧过大",
        ));
    }
    let mut mask = [0_u8; 4];
    if masked {
        stream.read_exact(&mut mask)?;
    }
    let mut payload = vec![0_u8; length as usize];
    stream.read_exact(&mut payload)?;
    if masked {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    }
    match opcode {
        0x1 => Ok(Some(String::from_utf8(payload).map_err(|error| {
            io::Error::new(io::ErrorKind::InvalidData, error)
        })?)),
        0x8 => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "WebSocket 已关闭",
        )),
        0x9 => {
            send_ws_control(stream, 0xA, &payload, false)?;
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn send_ws_control(
    stream: &mut TcpStream,
    opcode: u8,
    payload: &[u8],
    masked: bool,
) -> io::Result<()> {
    let mut header = vec![0x80 | (opcode & 0x0f)];
    let mask_bit = if masked { 0x80 } else { 0 };
    header.push(mask_bit | payload.len() as u8);
    if masked {
        let mask = random_bytes::<4>();
        header.extend_from_slice(&mask);
        stream.write_all(&header)?;
        let mut encoded = payload.to_vec();
        for (index, byte) in encoded.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
        stream.write_all(&encoded)
    } else {
        stream.write_all(&header)?;
        stream.write_all(payload)
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

impl LaboratoryRuntime {
    fn web_enabled(&self, app: &AppHandle) -> bool {
        let config = app.state::<AppState>().config.snapshot();
        config.laboratory.role == LaboratoryRole::Server
            && config.laboratory.server.web_enabled
            && self.is_running()
    }

    fn valid_web_token(&self, token: &str) -> bool {
        self.credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .web_token
            .as_deref()
            .is_some_and(|current| !current.is_empty() && current == token)
    }

    fn valid_http_client(&self, headers: &HashMap<String, String>) -> bool {
        let Some(client_id) = headers.get("x-lyrics-plus-client") else {
            return false;
        };
        let password = headers
            .get("x-lyrics-plus-password")
            .map(String::as_str)
            .unwrap_or_default();
        let current = self
            .credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .server_password
            .clone();
        if current.as_deref().is_some_and(|value| value != password) {
            return false;
        }
        !client_id.trim().is_empty()
    }

    fn server_password_matches(&self, password: &str) -> bool {
        self.credentials
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .server_password
            .as_deref()
            .map(|current| current == password)
            .unwrap_or(password.is_empty())
    }

    fn theme_file(&self, theme_id: &str, entry: &str) -> Option<PathBuf> {
        if !is_safe_theme_id(theme_id) || !is_safe_relative_path(Path::new(entry)) {
            return None;
        }
        let themes_dir = fs::canonicalize(self.app_dir.join("themes")).ok()?;
        let theme_dir = fs::canonicalize(themes_dir.join(theme_id)).ok()?;
        if !theme_dir.starts_with(&themes_dir) || !theme_dir.is_dir() {
            return None;
        }
        let file = fs::canonicalize(theme_dir.join(entry)).ok()?;
        file.starts_with(&theme_dir)
            .then_some(file)
            .filter(|path| path.is_file())
    }
}

fn server_websocket_loop(
    runtime: Arc<LaboratoryRuntime>,
    app: AppHandle,
    mut stream: TcpStream,
    web: bool,
) -> Result<(), String> {
    let connection_key = random_id(if web { "web" } else { "client" });
    let spectrum_subscription = format!("laboratory:{connection_key}");
    let cloned_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(error) => return Err(format!("复制实验室连接失败：{error}")),
    };
    let shared_stream = Arc::new(Mutex::new(cloned_stream));
    runtime
        .server_connections
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(connection_key.clone(), Arc::clone(&shared_stream));
    let auth_generation = if web {
        runtime.web_auth_generation.load(Ordering::SeqCst)
    } else {
        runtime.app_auth_generation.load(Ordering::SeqCst)
    };
    let mut client_id = None;
    if web {
        let state = current_state_snapshot(&app);
        let ready = envelope(
            "session.ready",
            None,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverId": runtime.server_id(),
                "state": state,
            }),
        );
        if let Err(error) = send_shared_ws_text(&shared_stream, &ready, false) {
            remove_server_connection(&runtime, &connection_key, &shared_stream);
            return Err(error.to_string());
        }
    } else {
        let first = match read_client_message(&mut stream) {
            Ok(Some(first)) => first,
            Ok(None) => {
                remove_server_connection(&runtime, &connection_key, &shared_stream);
                return Err("客户端未发送鉴权消息".into());
            }
            Err(error) => {
                remove_server_connection(&runtime, &connection_key, &shared_stream);
                return Err(error);
            }
        };
        let value: Value = match serde_json::from_str(&first) {
            Ok(value) => value,
            Err(error) => {
                remove_server_connection(&runtime, &connection_key, &shared_stream);
                return Err(format!("客户端消息无效：{error}"));
            }
        };
        if value.get("type").and_then(Value::as_str) != Some("client.hello") {
            let error = envelope(
                "session.error",
                None,
                json!({ "code": "client_hello_required", "message": "需要先发送 client.hello" }),
            );
            let _ = send_shared_ws_text(&shared_stream, &error, false);
            runtime
                .server_connections
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&connection_key);
            return Err("客户端缺少 client.hello".into());
        }
        let payload = value.get("payload").cloned().unwrap_or_default();
        if payload.get("protocolVersion").and_then(Value::as_u64) != Some(PROTOCOL_VERSION as u64) {
            let error = envelope(
                "session.error",
                value
                    .get("requestId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                json!({
                    "code": "unsupported_protocol",
                    "message": format!("仅支持实验室协议 v{PROTOCOL_VERSION}")
                }),
            );
            let _ = send_shared_ws_text(&shared_stream, &error, false);
            runtime
                .server_connections
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&connection_key);
            return Err("实验室协议版本不兼容".into());
        }
        let id = payload
            .get("clientId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let name = payload
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Lyrics Plus 客户端");
        let password = payload
            .get("password")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if id.is_empty() || !runtime.server_password_matches(password) {
            let error = envelope(
                "session.error",
                value
                    .get("requestId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                json!({ "code": "unauthorized", "message": "服务端密码错误" }),
            );
            let _ = send_shared_ws_text(&shared_stream, &error, false);
            runtime
                .server_connections
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&connection_key);
            return Err("客户端鉴权失败".into());
        }
        client_id = Some(id.to_owned());
        runtime
            .server_connections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&connection_key);
        let previous = runtime
            .server_connections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(id.to_owned(), Arc::clone(&shared_stream));
        if let Some(previous) = previous {
            let _ = previous
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .shutdown(Shutdown::Both);
        }
        if let Err(error) = update_client_record(&app, id, true, Some(name)) {
            remove_server_connection(&runtime, id, &shared_stream);
            return Err(error.to_string());
        }
        runtime.emit_status(&app);
        let state = current_state_snapshot(&app);
        let ready = envelope(
            "session.ready",
            None,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "serverId": runtime.server_id(),
                "state": state,
            }),
        );
        if let Err(error) = send_shared_ws_text(&shared_stream, &ready, false) {
            remove_server_connection(&runtime, id, &shared_stream);
            return Err(error.to_string());
        }
    }

    if let Err(error) = stream.set_read_timeout(Some(Duration::from_millis(5))) {
        if let Some(client_id) = client_id.as_deref() {
            if remove_server_connection(&runtime, client_id, &shared_stream) {
                let _ = update_client_record(&app, client_id, false, None);
            }
        } else {
            remove_server_connection(&runtime, &connection_key, &shared_stream);
        }
        return Err(error.to_string());
    }
    let ready_state = current_state_snapshot(&app);
    if let Some(state) = app.try_state::<AppState>() {
        state
            .spectrum
            .subscribe(&app, &spectrum_subscription, &ready_state.playback);
    }
    let mut state_cursor = StateUpdateCursor {
        previous: Some(ready_state),
        last_spectrum_sent_at: Some(Instant::now()),
    };
    let mut loop_error = None;
    loop {
        if let Some(flag) = runtime
            .stop_flag
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .cloned()
        {
            if flag.load(Ordering::Acquire) {
                break;
            }
        }
        let current_auth_generation = if web {
            runtime.web_auth_generation.load(Ordering::SeqCst)
        } else {
            runtime.app_auth_generation.load(Ordering::SeqCst)
        };
        if current_auth_generation != auth_generation {
            let error = envelope(
                "session.error",
                None,
                json!({ "code": "reauthenticate", "message": "凭据已变化，请重新连接" }),
            );
            let _ = send_shared_ws_text(&shared_stream, &error, false);
            break;
        }
        if let Err(error) = send_state_updates(&shared_stream, &app, &mut state_cursor) {
            loop_error = Some(format!("发送实验室状态失败：{error}"));
            break;
        }
        match read_ws_message(&mut stream) {
            Ok(Some(message)) => {
                if let Err(error) = handle_server_message(&runtime, &app, &message) {
                    let request_id =
                        serde_json::from_str::<Value>(&message)
                            .ok()
                            .and_then(|value| {
                                value
                                    .get("requestId")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                            });
                    let result_request_id = request_id.clone().unwrap_or_default();
                    let result = envelope(
                        "command.result",
                        request_id,
                        json!({
                            "requestId": result_request_id,
                            "ok": false,
                            "error": error,
                        }),
                    );
                    if let Err(write_error) = send_shared_ws_text(&shared_stream, &result, false) {
                        loop_error = Some(format!("发送实验室指令错误失败：{write_error}"));
                        break;
                    }
                }
            }
            Ok(None) => {}
            Err(error) if is_timeout(&error) => {}
            Err(error) => {
                loop_error = Some(format!("读取实验室消息失败：{error}"));
                break;
            }
        }
    }
    if let Some(client_id) = client_id {
        if remove_server_connection(&runtime, &client_id, &shared_stream) {
            let _ = update_client_record(&app, &client_id, false, None);
            runtime.emit_status(&app);
        }
    } else {
        remove_server_connection(&runtime, &connection_key, &shared_stream);
    }
    if let Some(state) = app.try_state::<AppState>() {
        state.spectrum.unsubscribe(&app, &spectrum_subscription);
    }
    loop_error.map_or(Ok(()), Err)
}

fn remove_server_connection(
    runtime: &LaboratoryRuntime,
    key: &str,
    stream: &Arc<Mutex<TcpStream>>,
) -> bool {
    let mut connections = runtime
        .server_connections
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if connections
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, stream))
    {
        connections.remove(key);
        true
    } else {
        false
    }
}

fn read_client_message(stream: &mut TcpStream) -> Result<Option<String>, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|error| error.to_string())?;
    read_ws_message(stream).map_err(|error| error.to_string())
}

fn handle_server_message(
    runtime: &Arc<LaboratoryRuntime>,
    app: &AppHandle,
    raw: &str,
) -> Result<(), String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("实验室消息格式错误：{error}"))?;
    let message_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match message_type {
        "playback.command" => {
            let payload = value.get("payload").cloned().unwrap_or_default();
            let action = serde_json::from_value::<PlaybackAction>(
                payload.get("action").cloned().unwrap_or(Value::Null),
            )
            .map_err(|error| format!("播放指令无效：{error}"))?;
            let position_ms = payload.get("positionMs").and_then(Value::as_u64);
            let request_id = value
                .get("requestId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| random_id("request"));
            runtime.submit_server_command(app, request_id, action, position_ms)
        }
        "web.hello" => Ok(()),
        "artwork.get" => {
            // 目前网页主题不需要主动请求封面；保留协议分支，App 客户端走 HTTP 资源接口。
            Ok(())
        }
        _ => Ok(()),
    }
}

struct StateUpdateCursor {
    previous: Option<LaboratoryStateSnapshot>,
    last_spectrum_sent_at: Option<Instant>,
}

fn send_state_updates(
    stream: &Arc<Mutex<TcpStream>>,
    app: &AppHandle,
    cursor: &mut StateUpdateCursor,
) -> io::Result<()> {
    let current = current_state_snapshot(app);
    let Some(previous_state) = cursor.previous.as_ref() else {
        send_shared_ws_text(
            stream,
            &envelope(
                "state.snapshot",
                None,
                serde_json::to_value(&current).unwrap_or_default(),
            ),
            false,
        )?;
        cursor.previous = Some(current);
        cursor.last_spectrum_sent_at = Some(Instant::now());
        return Ok(());
    };
    if previous_state.playback != current.playback {
        send_shared_ws_text(
            stream,
            &envelope(
                "playback.changed",
                None,
                serde_json::to_value(&current.playback).unwrap_or_default(),
            ),
            false,
        )?;
    }
    if previous_state.lyrics != current.lyrics {
        send_shared_ws_text(
            stream,
            &envelope(
                "lyrics.changed",
                None,
                serde_json::to_value(&current.lyrics).unwrap_or_default(),
            ),
            false,
        )?;
    }
    if previous_state.spectrum_state != current.spectrum_state {
        send_shared_ws_text(
            stream,
            &envelope(
                "spectrum.state",
                None,
                serde_json::to_value(&current.spectrum_state).unwrap_or_default(),
            ),
            false,
        )?;
    }
    let spectrum_due = cursor
        .last_spectrum_sent_at
        .map_or(true, |last| last.elapsed() >= SPECTRUM_PUSH_INTERVAL);
    let spectrum_changed = previous_state.spectrum_frame != current.spectrum_frame;
    if spectrum_changed && spectrum_due {
        send_shared_ws_text(
            stream,
            &envelope(
                "spectrum.frame",
                None,
                serde_json::to_value(&current.spectrum_frame).unwrap_or_default(),
            ),
            false,
        )?;
        cursor.last_spectrum_sent_at = Some(Instant::now());
    }

    // 更新非频谱字段时保留尚未发送的最新频谱帧，让下一个节流窗口发送最新值。
    if let Some(previous_state) = cursor.previous.as_mut() {
        previous_state.playback = current.playback;
        previous_state.lyrics = current.lyrics;
        previous_state.spectrum_state = current.spectrum_state;
        if spectrum_changed && spectrum_due {
            previous_state.spectrum_frame = current.spectrum_frame;
        }
        previous_state.observed_at_ms = current.observed_at_ms;
    }
    Ok(())
}

fn client_connection_loop(
    runtime: Arc<LaboratoryRuntime>,
    app: AppHandle,
    record: LaboratoryServerRecord,
    password: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut record = record;
    let mut password = password;
    let mut connected_once = false;
    let mut last_error = None;
    for attempt in 0..=CLIENT_RETRY_LIMIT {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if !record.requires_password {
            password.clear();
        }
        if attempt > 0 {
            if let Some(discovered) = discover_servers(Duration::from_millis(900))
                .into_iter()
                .find(|candidate| candidate.server_id == record.server_id)
            {
                record = discovered;
                if !record.requires_password {
                    password.clear();
                }
                let _ = save_server_record(&app, record.clone());
            }
        }
        runtime.set_phase(
            &app,
            if attempt == 0 && !connected_once {
                LaboratoryPhase::Connecting
            } else {
                LaboratoryPhase::Reconnecting
            },
            None,
        );
        match connect_websocket(&record) {
            Ok(mut stream) => {
                if stop.load(Ordering::Acquire) {
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
                let hello = envelope(
                    "client.hello",
                    None,
                    json!({
                        "protocolVersion": PROTOCOL_VERSION,
                        "name": app.state::<AppState>().config.snapshot().laboratory.client.name,
                        "clientId": runtime.client_id(),
                        "password": password,
                    }),
                );
                if let Err(error) = send_ws_text(&mut stream, &hello, true) {
                    log::debug!("发送实验室客户端鉴权失败：{error}");
                    last_error = Some(format!("发送实验室客户端鉴权失败：{error}"));
                    if attempt < CLIENT_RETRY_LIMIT {
                        retry_delay(&stop, attempt);
                        continue;
                    }
                    break;
                }
                let socket = match stream.try_clone() {
                    Ok(stream) => Arc::new(Mutex::new(stream)),
                    Err(error) => {
                        log::debug!("复制实验室客户端连接失败：{error}");
                        last_error = Some(format!("复制实验室客户端连接失败：{error}"));
                        break;
                    }
                };
                if stop.load(Ordering::Acquire) {
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
                *runtime
                    .client_socket
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(socket);
                connected_once = true;
                let _ = save_server_record(
                    &app,
                    LaboratoryServerRecord {
                        last_connected_at_ms: Some(now_ms()),
                        ..record.clone()
                    },
                );
                runtime.set_phase(&app, LaboratoryPhase::Running, None);
                if let Err(error) = stream.set_read_timeout(Some(Duration::from_millis(500))) {
                    log::debug!("设置实验室客户端超时失败：{error}");
                }
                let mut state = None;
                loop {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    match read_ws_message(&mut stream) {
                        Ok(Some(message)) => {
                            match handle_client_message(&runtime, &app, &mut state, &message) {
                                Ok(Some(server_id)) => {
                                    if let Err(error) = promote_server_record(
                                        &runtime,
                                        &app,
                                        &mut record,
                                        &server_id,
                                        &password,
                                    ) {
                                        log::debug!("保存实验室服务端稳定 ID 失败：{error}");
                                    }
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    log::debug!("处理实验室客户端消息失败：{error}");
                                    last_error = Some(error);
                                    break;
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(error) if is_timeout(&error) => {}
                        Err(error) => {
                            log::debug!("实验室客户端连接断开：{error}");
                            last_error = Some(format!("实验室客户端连接断开：{error}"));
                            break;
                        }
                    }
                }
                *runtime
                    .client_socket
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = None;
                if stop.load(Ordering::Acquire) {
                    return;
                }
                apply_remote_disconnect(&runtime, &app);
            }
            Err(error) => {
                log::debug!("连接实验室服务端失败：{error}");
                last_error = Some(error);
            }
        }
        if attempt < CLIENT_RETRY_LIMIT {
            retry_delay(&stop, attempt);
        }
    }
    runtime.set_phase(
        &app,
        LaboratoryPhase::Error,
        Some(
            last_error.unwrap_or_else(|| format!("服务端连接失败，已重试 {CLIENT_RETRY_LIMIT} 次")),
        ),
    );
    apply_remote_disconnect(&runtime, &app);
}

fn retry_delay(stop: &std::sync::atomic::AtomicBool, attempt: usize) {
    let end = Instant::now() + Duration::from_millis(400 + attempt as u64 * 250);
    while Instant::now() < end && !stop.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(40));
    }
}

fn promote_server_record(
    runtime: &LaboratoryRuntime,
    app: &AppHandle,
    record: &mut LaboratoryServerRecord,
    server_id: &str,
    password: &str,
) -> Result<(), String> {
    let server_id = server_id.trim();
    if server_id.is_empty() || record.server_id == server_id {
        return Ok(());
    }

    let previous_id = record.server_id.clone();
    record.server_id = server_id.to_owned();
    record.last_connected_at_ms = Some(now_ms());

    let mut records = load_server_records(app);
    records.retain(|item| item.server_id != previous_id && item.server_id != server_id);
    records.push(record.clone());
    records.sort_by(|left, right| right.last_connected_at_ms.cmp(&left.last_connected_at_ms));
    records.truncate(32);
    app.state::<AppState>().storage.set_preference(
        SERVER_RECORDS_PREFERENCE,
        &serde_json::to_string(&records).map_err(|error| error.to_string())?,
    )?;

    runtime.save_client_password(server_id, password)?;
    runtime.save_client_password(&previous_id, "")?;

    let current = app.state::<AppState>().config.snapshot();
    if current.laboratory.client.last_server_id.as_deref() == Some(previous_id.as_str()) {
        let config = app.state::<AppState>().config.update(|config| {
            config.laboratory.client.last_server_id = Some(server_id.to_owned());
        })?;
        let _ = app.emit("config://changed", &config);
    }
    Ok(())
}

fn connect_websocket(record: &LaboratoryServerRecord) -> Result<TcpStream, String> {
    let address = record
        .address
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("ws://");
    let target =
        if address.contains(':') && !address.starts_with('[') && address.matches(':').count() > 1 {
            format!("[{address}]:{}", record.port)
        } else {
            format!("{address}:{}", record.port)
        };
    let mut last_error = None;
    let mut stream = None;
    for address in target
        .to_socket_addrs()
        .map_err(|error| format!("解析 {target} 失败：{error}"))?
    {
        match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            Ok(candidate) => {
                stream = Some(candidate);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let mut stream = stream.ok_or_else(|| {
        format!(
            "连接 {target} 失败：{}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "没有可用地址".into())
        )
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .map_err(|error| error.to_string())?;
    let key = STANDARD.encode(random_bytes::<16>());
    write!(
        stream,
        "GET /ws HTTP/1.1\r\nHost: {address}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;
    let mut response = Vec::with_capacity(512);
    let mut byte = [0_u8; 1];
    while response.len() < MAX_HTTP_HEADER_BYTES {
        let read = stream
            .read(&mut byte)
            .map_err(|error| format!("读取 WebSocket 握手失败：{error}"))?;
        if read == 0 {
            break;
        }
        response.push(byte[0]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let response = String::from_utf8_lossy(&response);
    if !response.starts_with("HTTP/1.1 101") {
        return Err(format!(
            "服务端拒绝 WebSocket 握手：{}",
            response.lines().next().unwrap_or_default()
        ));
    }
    Ok(stream)
}

fn handle_client_message(
    runtime: &LaboratoryRuntime,
    app: &AppHandle,
    state: &mut Option<LaboratoryStateSnapshot>,
    raw: &str,
) -> Result<Option<String>, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("消息解析失败：{error}"))?;
    let message_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match message_type {
        "session.ready" | "state.snapshot" => {
            let payload = value.get("payload").cloned().unwrap_or_default();
            let server_id = (message_type == "session.ready")
                .then(|| payload.get("serverId").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned);
            let next = payload.get("state").cloned().unwrap_or(payload);
            let next = serde_json::from_value::<LaboratoryStateSnapshot>(next)
                .map_err(|error| format!("统一状态快照无效：{error}"))?;
            *state = Some(next.clone());
            apply_remote_state(runtime, app, next);
            return Ok(server_id);
        }
        "playback.changed" => {
            let Some(current) = state.as_mut() else {
                return Ok(None);
            };
            current.playback =
                serde_json::from_value(value.get("payload").cloned().unwrap_or_default())
                    .map_err(|error| format!("播放状态无效：{error}"))?;
            apply_remote_state(runtime, app, current.clone());
        }
        "lyrics.changed" => {
            let Some(current) = state.as_mut() else {
                return Ok(None);
            };
            current.lyrics =
                serde_json::from_value(value.get("payload").cloned().unwrap_or_default())
                    .map_err(|error| format!("歌词状态无效：{error}"))?;
            apply_remote_state(runtime, app, current.clone());
        }
        "spectrum.state" => {
            let Some(current) = state.as_mut() else {
                return Ok(None);
            };
            current.spectrum_state =
                serde_json::from_value(value.get("payload").cloned().unwrap_or_default())
                    .map_err(|error| format!("频谱状态无效：{error}"))?;
            apply_remote_state(runtime, app, current.clone());
        }
        "spectrum.frame" => {
            let Some(current) = state.as_mut() else {
                return Ok(None);
            };
            current.spectrum_frame =
                serde_json::from_value(value.get("payload").cloned().unwrap_or_default())
                    .map_err(|error| format!("频谱帧无效：{error}"))?;
            apply_remote_state(runtime, app, current.clone());
        }
        "command.result" => {
            let _ = app.emit(
                "laboratory://command-result",
                value.get("payload").cloned().unwrap_or_default(),
            );
        }
        "session.error" => {
            let payload = value.get("payload").cloned().unwrap_or_default();
            let message = payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("实验室会话失败")
                .to_owned();
            let error = message.clone();
            runtime.set_phase(app, LaboratoryPhase::Error, Some(message));
            return Err(error);
        }
        _ => {}
    }
    Ok(None)
}

fn apply_remote_state(
    runtime: &LaboratoryRuntime,
    app: &AppHandle,
    snapshot: LaboratoryStateSnapshot,
) {
    if !runtime.is_client() || !runtime.is_running() {
        return;
    }
    *runtime
        .remote_state
        .write()
        .unwrap_or_else(|error| error.into_inner()) = Some(snapshot.clone());
    if let Some(state) = app.try_state::<AppState>() {
        *state
            .last_snapshot
            .write()
            .unwrap_or_else(|error| error.into_inner()) = snapshot.playback.clone();
        *state
            .lyrics_runtime
            .write()
            .unwrap_or_else(|error| error.into_inner()) = snapshot.lyrics.clone();
    }
    let _ = app.emit("playback://snapshot", &snapshot.playback);
    let _ = app.emit("lyrics://runtime-changed", &snapshot.lyrics);
    let _ = app.emit("playback://spectrum-state", &snapshot.spectrum_state);
    let _ = app.emit("playback://spectrum-frame", &snapshot.spectrum_frame);
    let _ = crate::reconcile_overlay_visibility(app);
    crate::sync_lyrics_surfaces(app);
}

fn apply_remote_disconnect(runtime: &LaboratoryRuntime, app: &AppHandle) {
    *runtime
        .remote_state
        .write()
        .unwrap_or_else(|error| error.into_inner()) = None;
    if let Some(state) = app.try_state::<AppState>() {
        let snapshot = PlaybackSnapshot::unavailable(None, "实验室客户端已断开".into());
        *state
            .last_snapshot
            .write()
            .unwrap_or_else(|error| error.into_inner()) = snapshot.clone();
        *state
            .lyrics_runtime
            .write()
            .unwrap_or_else(|error| error.into_inner()) =
            crate::lyrics::LyricsRuntimeSnapshot::default();
        let _ = app.emit("playback://snapshot", &snapshot);
        let _ = app.emit(
            "lyrics://runtime-changed",
            crate::lyrics::LyricsRuntimeSnapshot::default(),
        );
    }
    let spectrum_state = PlaybackSpectrumState::default();
    let spectrum_frame = PlaybackSpectrumFrame::silent(None);
    let _ = app.emit("playback://spectrum-state", &spectrum_state);
    let _ = app.emit("playback://spectrum-frame", &spectrum_frame);
    let _ = crate::reconcile_overlay_visibility(app);
    crate::sync_lyrics_surfaces(app);
}

fn discovery_advertiser_loop(
    runtime: Arc<LaboratoryRuntime>,
    app: AppHandle,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let Ok(socket) = bind_mdns_socket() else {
        return;
    };
    let _ = socket.set_multicast_loop_v4(true);
    let _ = socket.set_multicast_ttl_v4(1);
    let multicast = Ipv4Addr::new(224, 0, 0, 251);
    let _ = socket.join_multicast_v4(&multicast, &Ipv4Addr::UNSPECIFIED);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));
    let mut next_announcement = Instant::now();
    let mut buffer = [0_u8; 16 * 1024];
    while !stop.load(Ordering::Acquire) {
        let config = app.state::<AppState>().config.snapshot();
        let active_record = if config.laboratory.role == LaboratoryRole::Server
            && config.laboratory.server.discovery_enabled
            && runtime.is_running()
        {
            Some(LaboratoryServerRecord {
                server_id: runtime.server_id(),
                name: config.laboratory.server.name,
                address: local_ip(),
                port: config.laboratory.server.port,
                protocol_version: PROTOCOL_VERSION,
                requires_password: runtime.server_password_enabled(),
                web_available: config.laboratory.server.web_enabled,
                last_connected_at_ms: None,
                discovered: true,
            })
        } else {
            None
        };
        if let Some(record) = active_record.as_ref() {
            if Instant::now() >= next_announcement {
                let packet = build_mdns_announcement(record);
                let _ = socket.send_to(&packet, "224.0.0.251:5353");
                next_announcement = Instant::now() + Duration::from_secs(5);
            }
        }
        match socket.recv_from(&mut buffer) {
            Ok((size, source)) if is_mdns_service_query(&buffer[..size]) => {
                if let Some(record) = active_record.as_ref() {
                    let packet = build_mdns_announcement(record);
                    let _ = socket.send_to(&packet, source);
                    let _ = socket.send_to(&packet, "224.0.0.251:5353");
                }
            }
            Ok(_) => {}
            Err(error) if is_timeout(&error) => {}
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn is_mdns_service_query(packet: &[u8]) -> bool {
    let text = String::from_utf8_lossy(packet);
    text.contains("_lyrics-plus") && text.contains("_tcp") && !text.contains("id=")
}

fn build_mdns_query() -> Vec<u8> {
    let mut packet = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    encode_dns_name(MDNS_SERVICE, &mut packet);
    packet.extend_from_slice(&12_u16.to_be_bytes());
    packet.extend_from_slice(&1_u16.to_be_bytes());
    packet
}

fn build_mdns_announcement(record: &LaboratoryServerRecord) -> Vec<u8> {
    let service_name = format!("{}._lyrics-plus._tcp.local.", record.server_id);
    let host_name = format!("lyrics-plus-{}.local.", record.server_id);
    let mut packet = vec![0, 0, 0x84, 0, 0, 0, 0, 4, 0, 0, 0, 0];
    encode_dns_name(MDNS_SERVICE, &mut packet);
    packet.extend_from_slice(&12_u16.to_be_bytes());
    packet.extend_from_slice(&1_u16.to_be_bytes());
    let mut ptr = Vec::new();
    encode_dns_name(&service_name, &mut ptr);
    dns_record(&mut packet, &ptr, 120);

    encode_dns_name(&service_name, &mut packet);
    packet.extend_from_slice(&33_u16.to_be_bytes());
    packet.extend_from_slice(&1_u16.to_be_bytes());
    packet.extend_from_slice(&120_u32.to_be_bytes());
    let mut srv = Vec::new();
    srv.extend_from_slice(&0_u16.to_be_bytes());
    srv.extend_from_slice(&0_u16.to_be_bytes());
    srv.extend_from_slice(&record.port.to_be_bytes());
    encode_dns_name(&host_name, &mut srv);
    packet.extend_from_slice(&(srv.len() as u16).to_be_bytes());
    packet.extend_from_slice(&srv);

    encode_dns_name(&service_name, &mut packet);
    packet.extend_from_slice(&16_u16.to_be_bytes());
    packet.extend_from_slice(&1_u16.to_be_bytes());
    packet.extend_from_slice(&120_u32.to_be_bytes());
    let txt_values = [
        format!("id={}", record.server_id),
        format!("name={}", record.name),
        format!("address={}", record.address),
        format!("port={}", record.port),
        format!("protocolVersion={}", record.protocol_version),
        format!("requiresPassword={}", record.requires_password),
        format!("webAvailable={}", record.web_available),
    ];
    let mut txt = Vec::new();
    for value in txt_values {
        let value = value.as_bytes();
        if value.len() <= 255 {
            txt.push(value.len() as u8);
            txt.extend_from_slice(value);
        }
    }
    packet.extend_from_slice(&(txt.len() as u16).to_be_bytes());
    packet.extend_from_slice(&txt);

    if let Some(address) = ipv4_octets(&record.address) {
        encode_dns_name(&host_name, &mut packet);
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&120_u32.to_be_bytes());
        packet.extend_from_slice(&4_u16.to_be_bytes());
        packet.extend_from_slice(&address);
    }
    packet
}

fn dns_record(packet: &mut Vec<u8>, data: &[u8], ttl: u32) {
    packet.extend_from_slice(&(data.len() as u16).to_be_bytes());
    packet.extend_from_slice(&ttl.to_be_bytes());
    packet.extend_from_slice(data);
}

fn encode_dns_name(name: &str, output: &mut Vec<u8>) {
    for label in name.trim_end_matches('.').split('.') {
        let bytes = label.as_bytes();
        if bytes.len() > 63 {
            continue;
        }
        output.push(bytes.len() as u8);
        output.extend_from_slice(bytes);
    }
    output.push(0);
}

fn discover_servers(timeout: Duration) -> Vec<LaboratoryServerRecord> {
    let Ok(socket) = bind_mdns_socket() else {
        return Vec::new();
    };
    let multicast = Ipv4Addr::new(224, 0, 0, 251);
    let _ = socket.join_multicast_v4(&multicast, &Ipv4Addr::UNSPECIFIED);
    let _ = socket.set_multicast_loop_v4(true);
    let _ = socket.set_read_timeout(Some(Duration::from_millis(120)));
    let _ = socket.send_to(&build_mdns_query(), "224.0.0.251:5353");
    let deadline = Instant::now() + timeout;
    let mut records = HashMap::new();
    let mut buffer = [0_u8; 16 * 1024];
    while Instant::now() < deadline {
        match socket.recv_from(&mut buffer) {
            Ok((size, _)) => {
                if let Some(record) = parse_mdns_record(&buffer[..size])
                    .filter(|record| record.protocol_version == PROTOCOL_VERSION)
                {
                    records.insert(record.server_id.clone(), record);
                }
            }
            Err(error) if is_timeout(&error) => {}
            Err(_) => break,
        }
    }
    records.into_values().collect()
}

fn bind_mdns_socket() -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353));
    socket.bind(&SockAddr::from(address))?;
    Ok(socket.into())
}

fn parse_mdns_record(packet: &[u8]) -> Option<LaboratoryServerRecord> {
    let text = String::from_utf8_lossy(packet);
    let mut values = HashMap::new();
    for key in [
        "id",
        "name",
        "address",
        "port",
        "protocolVersion",
        "requiresPassword",
        "webAvailable",
    ] {
        let marker = format!("{key}=");
        let start = text.find(&marker)? + marker.len();
        let value = text[start..]
            .split(|character: char| {
                character == '\0' || character.is_control() || character == '&'
            })
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !value.is_empty() {
            values.insert(key, value);
        }
    }
    let server_id = values.get("id")?.clone();
    let address = values.get("address")?.clone();
    let port = values.get("port")?.parse().ok()?;
    Some(LaboratoryServerRecord {
        server_id,
        name: values
            .get("name")
            .cloned()
            .unwrap_or_else(|| "Lyrics Plus 服务端".into()),
        address,
        port,
        protocol_version: values
            .get("protocolVersion")
            .and_then(|value| value.parse().ok())
            .unwrap_or(PROTOCOL_VERSION),
        requires_password: values
            .get("requiresPassword")
            .is_some_and(|value| value == "true"),
        web_available: values
            .get("webAvailable")
            .is_some_and(|value| value == "true"),
        last_connected_at_ms: None,
        discovered: true,
    })
}

fn web_addresses(port: u16, token: &str) -> Vec<LaboratoryWebAddress> {
    local_ipv4_addresses()
        .into_iter()
        .map(|ip| {
            let address = format!("{ip}:{port}");
            LaboratoryWebAddress {
                url: format!("http://{address}/?token={token}"),
                address,
            }
        })
        .collect()
}

fn local_ip() -> String {
    preferred_local_ip()
        .or_else(|| local_ipv4_addresses().into_iter().next())
        .map(|address| address.to_string())
        .unwrap_or_else(|| "127.0.0.1".into())
}

fn preferred_local_ip() -> Option<Ipv4Addr> {
    UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|socket| {
            socket.connect("8.8.8.8:80").ok()?;
            match socket.local_addr().ok()?.ip() {
                std::net::IpAddr::V4(address) if is_usable_ipv4(address) => Some(address),
                _ => None,
            }
        })
}

fn local_ipv4_addresses() -> Vec<Ipv4Addr> {
    let preferred = preferred_local_ip();
    let mut addresses = interface_ipv4_addresses();
    addresses.sort_unstable_by_key(|address| address.octets());
    addresses.dedup();

    if let Some(preferred) = preferred {
        if let Some(index) = addresses.iter().position(|address| *address == preferred) {
            addresses.remove(index);
        }
        addresses.insert(0, preferred);
    }
    addresses
}

fn is_usable_ipv4(address: Ipv4Addr) -> bool {
    !address.is_unspecified()
        && !address.is_loopback()
        && !address.is_multicast()
        && !address.is_broadcast()
}

#[cfg(unix)]
fn interface_ipv4_addresses() -> Vec<Ipv4Addr> {
    let mut result = Vec::new();
    let mut interfaces = ptr::null_mut();

    // SAFETY: getifaddrs initializes the linked list and freeifaddrs releases it below.
    let status = unsafe { libc::getifaddrs(&mut interfaces) };
    if status != 0 {
        return result;
    }

    let mut current = interfaces;
    while !current.is_null() {
        // SAFETY: current points to an entry owned by the list returned by getifaddrs.
        let interface = unsafe { &*current };
        if !interface.ifa_addr.is_null()
            && (interface.ifa_flags & libc::IFF_UP as u32) != 0
        {
            // SAFETY: the family check guarantees this address is a sockaddr_in.
            let address = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_in) };
            if address.sin_family as i32 == libc::AF_INET {
                let ip = Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr));
                if is_usable_ipv4(ip) {
                    result.push(ip);
                }
            }
        }
        // SAFETY: ifa_next is either null or the next entry in the same owned list.
        current = interface.ifa_next;
    }

    // SAFETY: interfaces was initialized by getifaddrs and has not been freed yet.
    unsafe { libc::freeifaddrs(interfaces) };
    result
}

#[cfg(not(unix))]
fn interface_ipv4_addresses() -> Vec<Ipv4Addr> {
    Vec::new()
}

fn ipv4_octets(address: &str) -> Option<[u8; 4]> {
    let values = address.parse::<std::net::Ipv4Addr>().ok()?.octets();
    Some(values)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThemeManifest {
    id: String,
    name: String,
    version: String,
    entry: String,
    sdk_version: String,
}

fn scan_theme_directories(themes_dir: &Path) -> Vec<LaboratoryThemeInfo> {
    let Ok(themes_root) = fs::canonicalize(themes_dir) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(themes_dir) else {
        return Vec::new();
    };
    let mut themes = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(theme_root) = fs::canonicalize(&path) else {
            continue;
        };
        if !theme_root.starts_with(&themes_root) {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        let Ok(raw) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<ThemeManifest>(&raw) else {
            continue;
        };
        let id = manifest.id.trim();
        let entry = manifest.entry.trim();
        if id.is_empty()
            || manifest.name.trim().is_empty()
            || manifest.version.trim().is_empty()
            || manifest.sdk_version.trim().is_empty()
            || !ids.insert(id.to_owned())
            || !is_safe_theme_id(id)
            || !is_safe_relative_path(Path::new(entry))
        {
            continue;
        }
        let Ok(entry_path) = fs::canonicalize(theme_root.join(entry)) else {
            continue;
        };
        if !entry_path.starts_with(&theme_root) || !entry_path.is_file() {
            continue;
        }
        themes.push(LaboratoryThemeInfo {
            id: id.to_owned(),
            name: if manifest.name.trim().is_empty() {
                id.to_owned()
            } else {
                manifest.name
            },
            version: manifest.version,
            entry: entry.to_owned(),
            sdk_version: manifest.sdk_version,
        });
    }
    themes.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    themes
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn is_safe_theme_id(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
            {
                output.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn fetch_remote_artwork(
    record: &LaboratoryServerRecord,
    client_id: &str,
    password: &str,
    artwork_id: &str,
) -> Result<Option<PlaybackArtwork>, String> {
    let address = record.address.trim().trim_start_matches("http://");
    let target = format!("{address}:{}", record.port);
    let mut stream =
        TcpStream::connect(&target).map_err(|error| format!("连接远程封面服务失败：{error}"))?;
    let path = format!("/artwork/{}", percent_encode(artwork_id));
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nX-Lyrics-Plus-Client: {client_id}\r\nX-Lyrics-Plus-Password: {password}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "远程封面响应无效".to_string())?;
    let headers = String::from_utf8_lossy(&response[..header_end]);
    if !headers.starts_with("HTTP/1.1 200") {
        if headers.starts_with("HTTP/1.1 404") {
            return Ok(None);
        }
        return Err(format!(
            "远程封面请求失败：{}",
            headers.lines().next().unwrap_or_default()
        ));
    }
    serde_json::from_slice(&response[header_end + 4..])
        .map(Some)
        .map_err(|error| format!("解析远程封面失败：{error}"))
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn web_shell_html(token: &str) -> String {
    r###"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Lyrics Plus 实验室</title>
<style>
:root { color-scheme: dark; font-family: -apple-system,BlinkMacSystemFont,"SF Pro Text",sans-serif; background:#111318; color:#f4f4f5; }
* { box-sizing:border-box; }
body { margin:0; min-height:100vh; background:radial-gradient(circle at 15% 0%,#273527 0,#111318 42%); }
header { display:flex; align-items:center; gap:18px; padding:18px 24px; border-bottom:1px solid #2b2d35; background:#111318cc; backdrop-filter:blur(16px); position:sticky; top:0; z-index:2; }
h1 { font-size:18px; margin:0; white-space:nowrap; }
header span { color:#a1a1aa; font-size:13px; }
select { margin-left:auto; max-width:240px; border:1px solid #3f414b; border-radius:9px; padding:8px 10px; background:#1b1d23; color:inherit; }
select:disabled { opacity:.55; }
main { min-height:calc(100vh - 65px); padding:24px; }
iframe { display:block; width:100%; height:calc(100vh - 115px); min-height:430px; border:0; border-radius:16px; background:#17191f; box-shadow:0 18px 60px #0005; }
#status { font-size:12px; color:#a1a1aa; }
#empty-state { display:grid; place-items:center; min-height:calc(100vh - 115px); padding:32px; border:1px dashed #3f414b; border-radius:16px; color:#a1a1aa; text-align:center; }
#empty-state h2 { margin:0 0 8px; color:#f4f4f5; font-size:20px; }
#empty-state p { max-width:520px; margin:0; line-height:1.6; }
</style>
</head>
<body>
<header><h1>Lyrics Plus · 实验室</h1><span id="status">正在连接服务端…</span><select id="theme" aria-label="选择主题" disabled></select></header>
<main><iframe id="theme-frame" title="Lyrics Plus 主题" sandbox="allow-scripts" hidden></iframe><div id="empty-state" hidden><div><h2 id="empty-title">没有可用主题</h2><p id="empty-message">请将主题文件夹放入应用数据目录的 themes/ 后刷新页面。</p></div></div></main>
<script>
const token = __TOKEN__;
const themeSelect = document.getElementById('theme');
let frame = document.getElementById('theme-frame');
const status = document.getElementById('status');
const emptyState = document.getElementById('empty-state');
const emptyTitle = document.getElementById('empty-title');
const emptyMessage = document.getElementById('empty-message');
let themes = [];
let socket;
let lastState;
let connectionState = 'connecting';
let reconnectTimer;
const sendToTheme = (message) => frame.contentWindow?.postMessage({ source:'lyrics-plus-host', ...message }, '*');
const frameUrl = (theme) => { const entry = theme.entry.split('/').map(encodeURIComponent).join('/'); return `/theme/${encodeURIComponent(theme.id)}/${entry}?token=${encodeURIComponent(token)}`; };
const showEmpty = (title = '没有可用主题', message = '请将主题文件夹放入应用数据目录的 themes/ 后刷新页面。') => { frame.hidden=true; emptyState.hidden=false; emptyTitle.textContent=title; emptyMessage.textContent=message; themeSelect.disabled=true; status.textContent=title; };
const replaceThemeFrame = (theme) => {
  if (!theme) { showEmpty(); return; }
  const next = document.createElement('iframe');
  next.id='theme-frame';
  next.title='Lyrics Plus 主题';
  next.setAttribute('sandbox','allow-scripts');
  const url = frameUrl(theme);
  const fallback = () => {
    if (frame !== next || next.dataset.ready === 'true') return;
    clearTimeout(next.dataset.fallbackTimer);
    const basic = themes.find(item => item.id === 'basic-demo');
    if (basic && basic.id !== theme.id) {
      themeSelect.value=basic.id;
      localStorage.setItem('lyrics-plus-theme', basic.id);
      replaceThemeFrame(basic);
    } else {
      showEmpty('主题加载失败', '请检查主题文件夹和 manifest.json，或从 GitHub 重新下载基础 Demo。');
    }
  };
  next.addEventListener('load', () => { clearTimeout(Number(next.dataset.fallbackTimer)); next.hidden=false; emptyState.hidden=true; themeSelect.disabled=false; sendToTheme({ kind:'connection', state:connectionState }); if (lastState) sendToTheme({ kind:'state', state:lastState }); });
  next.addEventListener('error', fallback);
  clearTimeout(Number(frame.dataset.fallbackTimer));
  frame.replaceWith(next);
  frame=next;
  frame.src=url;
  next.dataset.fallbackTimer = String(setTimeout(fallback, 4000));
};
const loadTheme = () => {
  const theme = themes.find(item => item.id === themeSelect.value);
  if (!theme) { showEmpty(); return; }
  localStorage.setItem('lyrics-plus-theme', theme.id);
  replaceThemeFrame(theme);
};
themeSelect.addEventListener('change', loadTheme);
window.addEventListener('message', event => {
  if (event.source !== frame.contentWindow || !event.data || event.data.source !== 'lyrics-plus-theme') return;
  if ((event.data.kind === 'ready' || event.data.kind === 'subscribe') && lastState) sendToTheme({ kind:'state', state:lastState });
  if (event.data.kind === 'ready' || event.data.kind === 'subscribe') { frame.dataset.ready='true'; sendToTheme({ kind:'connection', state:connectionState }); }
  if (event.data.kind === 'command' && socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ protocolVersion:1, type:'playback.command', timestampMs:Date.now(), requestId:`web-${Date.now()}-${Math.random()}`, payload:{ action:event.data.action, positionMs:event.data.positionMs ?? null } }));
});
const setConnectionState = (next) => { connectionState=next; sendToTheme({ kind:'connection', state:next }); };
const applyMessage = message => {
  const payload = message.payload || {};
  if (message.type === 'session.ready' || message.type === 'state.snapshot') lastState = payload.state || payload;
  else if (message.type === 'playback.changed' && lastState) lastState = {...lastState, playback:payload};
  else if (message.type === 'lyrics.changed' && lastState) lastState = {...lastState, lyrics:payload};
  else if (message.type === 'spectrum.state' && lastState) lastState = {...lastState, spectrumState:payload};
  else if (message.type === 'spectrum.frame' && lastState) lastState = {...lastState, spectrumFrame:payload};
  else if (message.type === 'command.result') { sendToTheme({ kind:'command-result', result:payload }); if (!payload.ok) status.textContent = payload.error || '播放指令失败'; }
  else if (message.type === 'session.error') { status.textContent = payload.message || '会话失败'; setConnectionState('error'); }
  if (lastState) sendToTheme({ kind:'state', state:lastState });
};
const connect = () => { setConnectionState('connecting'); const current = new WebSocket(`${location.origin.replace(/^http/,'ws')}/ws?token=${encodeURIComponent(token)}`); socket=current; current.addEventListener('open', () => { status.textContent='已连接'; setConnectionState('connected'); current.send(JSON.stringify({protocolVersion:1,type:'web.hello',timestampMs:Date.now(),payload:{sdkVersion:1,token}})); }); current.addEventListener('message', event => { try { applyMessage(JSON.parse(event.data)); } catch {} }); current.addEventListener('close', () => { if (socket !== current) return; status.textContent='连接已断开，正在重连…'; setConnectionState('disconnected'); clearTimeout(reconnectTimer); reconnectTimer=setTimeout(connect, 1500); }); current.addEventListener('error', () => { if (socket === current) { status.textContent='连接失败'; setConnectionState('error'); } }); };
fetch(`/themes?token=${encodeURIComponent(token)}`).then(response => response.json()).then(items => { themes = Array.isArray(items) ? items : []; themeSelect.replaceChildren(); for (const item of themes) { const option=document.createElement('option'); option.value=item.id; option.textContent=`${item.name} · ${item.version}`; themeSelect.append(option); } themeSelect.disabled=themes.length===0; const saved=localStorage.getItem('lyrics-plus-theme'); const initial=themes.find(item=>item.id===saved) || themes.find(item=>item.id==='basic-demo') || themes[0]; if (initial) { themeSelect.value=initial.id; loadTheme(); } else showEmpty(); }).catch(() => { themes=[]; showEmpty('主题列表加载失败', '无法读取主题列表，请确认实验室服务正在运行。'); });
connect();
</script>
</body>
</html>"###
        .replace("__TOKEN__", &serde_json::to_string(token).unwrap_or_else(|_| "\"\"".into()))
}

const THEME_SDK_JS: &str = include_str!("../../../packages/theme-sdk/index.js");

fn sha1_digest(input: &[u8]) -> [u8; 20] {
    let mut message = input.to_vec();
    let bit_len = (message.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());
    let mut h = [
        0x6745_2301_u32,
        0xefcd_ab89,
        0x98ba_dcfe,
        0x1032_5476,
        0xc3d2_e1f0,
    ];
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words[..16].iter_mut().enumerate() {
            *word = u32::from_be_bytes([
                chunk[index * 4],
                chunk[index * 4 + 1],
                chunk[index * 4 + 2],
                chunk[index * 4 + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (index, word) in words.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut output = [0_u8; 20];
    for (index, value) in h.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    output
}
