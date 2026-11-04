mod supervisor;

use crate::supervisor::{
    normalize_path, pid_matches, spawn_process, CommandPlan, ImageMatcher, ProcessPlan,
    ProcessSupervisor,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder,
};

const SERVICE_STATUS_EVENT: &str = "service-status";
const GATEWAY_STATUS_EVENT: &str = "gateway-status";
const INSTANCE_FORWARD_TIMEOUT: Duration = Duration::from_secs(2);

struct SingleInstance {
    guard: InstanceGuard,
    listener: TcpListener,
}

struct InstanceGuard {
    native_mutex: isize,
    port_file: PathBuf,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        close_native_mutex(self.native_mutex);
        let _ = fs::remove_file(&self.port_file);
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatus {
    phase: String,
    pid: Option<u32>,
    message: String,
    auto_restart: bool,
    restart_attempts: u32,
    service_path: String,
    working_dir: String,
    env_file: String,
    log_file: String,
    pid_file: String,
    control_state_file: String,
}

#[derive(Clone)]
struct ServiceManager {
    supervisor: Arc<ProcessSupervisor>,
    service_path: PathBuf,
    working_dir: PathBuf,
    env_file: PathBuf,
    control_state_file: PathBuf,
    control_token_file: PathBuf,
}

impl ServiceManager {
    fn discover() -> Result<Self, String> {
        let executable =
            env::current_exe().map_err(|error| format!("无法确定桌面程序路径：{error}"))?;
        let executable_dir = executable
            .parent()
            .ok_or_else(|| "桌面程序路径没有父目录".to_string())?;
        let service_path = resolve_service(executable_dir);
        let working_dir = env::var_os("ZHIWEI_HOME")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| absolutize(&path))
            .unwrap_or_else(|| {
                infer_backend_dir(&service_path).unwrap_or_else(|| executable_dir.to_path_buf())
            });
        let logs_dir = working_dir.join("logs");

        fs::create_dir_all(&logs_dir)
            .map_err(|error| format!("无法创建日志目录 {}：{error}", logs_dir.display()))?;

        let log_file = logs_dir.join("zhiwei-service.log");
        let pid_file = logs_dir.join("zhiwei-service.pid");
        let plan = ProcessPlan {
            name: "通道守护",
            program_label: "zhiwei-service.exe",
            log_header: "--- zhiwei-desktop starting zhiwei-service ---",
            log_file: log_file.clone(),
            pid_file,
            working_dir: working_dir.clone(),
            command: CommandPlan::Exe(service_path.clone()),
            image_matcher: ImageMatcher::Path(service_path.clone()),
            // 显式转发日志级别：中间层（npm / tauri / cargo）可能重整环境，
            // 子进程拿不到 RUST_LOG 就只剩 WARN，服务启动过程将完全不可见。
            env: vec![(
                "RUST_LOG".to_string(),
                env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            )],
            terminate_tree: false,
            spawn_error_hint:
                "。如果系统提示 Application Control，请在 Windows 安全策略中信任该程序。",
        };
        let supervisor = Arc::new(ProcessSupervisor::new(
            plan,
            Box::new(spawn_process),
            true,
            "stopped",
            "服务尚未启动".to_string(),
        ));
        if !supervisor.refresh_external() {
            if !supervisor.program_exists() {
                supervisor.mark_error(&format!(
                    "未找到 zhiwei-service.exe：{}",
                    service_path.display()
                ));
            } else {
                supervisor.reset_stopped("服务尚未启动");
            }
        }
        Ok(Self {
            supervisor,
            service_path,
            working_dir: working_dir.clone(),
            env_file: resolve_env_file(&working_dir),
            control_state_file: logs_dir.join("control.json"),
            control_token_file: logs_dir.join("control.token"),
        })
    }

    fn snapshot(&self) -> ServiceStatus {
        let mut status = self.supervisor.snapshot();
        if status.phase != "running" {
            if let Some(pid) = self.external_pid() {
                status.phase = "running".to_string();
                status.pid = Some(pid);
                status.message = "服务已在运行（外部启动）".to_string();
                status.auto_restart = false;
            }
        }
        ServiceStatus {
            phase: status.phase,
            pid: status.pid,
            message: status.message,
            auto_restart: status.auto_restart,
            restart_attempts: status.restart_attempts,
            service_path: self.service_path.display().to_string(),
            working_dir: self.working_dir.display().to_string(),
            env_file: self.env_file.display().to_string(),
            log_file: self.supervisor.plan().log_file.display().to_string(),
            pid_file: self.supervisor.plan().pid_file.display().to_string(),
            control_state_file: self.control_state_file.display().to_string(),
        }
    }

    fn start(&self) -> Result<ServiceStatus, String> {
        if self.external_pid().is_some() {
            return Ok(self.snapshot());
        }
        self.supervisor.start()?;
        Ok(self.snapshot())
    }

    fn stop(&self) -> Result<ServiceStatus, String> {
        if self.external_pid().is_some() {
            return Err("服务由外部启动，桌面壳不管理它；请在启动它的终端里停止".to_string());
        }
        self.supervisor.stop()?;
        Ok(self.snapshot())
    }

    fn restart(&self) -> Result<ServiceStatus, String> {
        if self.external_pid().is_some() {
            return Err("服务由外部启动，桌面壳不管理它；请在启动它的终端里重启".to_string());
        }
        self.supervisor.restart()?;
        Ok(self.snapshot())
    }

    fn tick(&self) -> ServiceStatus {
        self.supervisor.tick();
        self.snapshot()
    }

    /// 识别外部启动的 service：运行中的服务会把 pid 写进 control.json，
    /// 校验进程镜像后即认为同一份服务，避免桌面壳再拉起第二份。
    fn external_pid(&self) -> Option<u32> {
        // supervisor 已经托管（自行启动或按 pid 文件接管）时不做外部识别，
        // 否则 stop / restart 会被误判成“外部进程”而拒绝执行。
        if self.supervisor.snapshot().phase == "running" {
            return None;
        }
        let text = fs::read_to_string(&self.control_state_file).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let pid = u32::try_from(value.get("pid")?.as_u64()?).ok()?;
        if service_image_paths(&self.service_path)
            .iter()
            .any(|path| pid_matches(&ImageMatcher::Path(path.clone()), pid))
        {
            return Some(pid);
        }
        // 进程镜像查询可能被 Windows 权限挡住（尤其是开发态跨终端启动），
        // 再用服务自己的控制接口确认一次 pid，避免把活着的服务显示成已停止。
        control_health_pid(&self.control_state_file, &self.control_token_file)
            .filter(|health_pid| *health_pid == pid)
            .map(|_| pid)
    }
}

#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct GatewayHealth {
    ok: bool,
    diffusion: Option<String>,
    tts: Option<String>,
    asr_engine: Option<String>,
    asr_importable: Option<bool>,
    asr_model_loaded: Option<bool>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewayStatus {
    phase: String,
    pid: Option<u32>,
    message: String,
    auto_restart: bool,
    restart_attempts: u32,
    installed: bool,
    executable_path: String,
    working_dir: String,
    env_file: String,
    log_file: String,
    url: String,
    service_url: String,
    url_matches_service: bool,
    health: Option<GatewayHealth>,
}

#[derive(Clone)]
struct GatewayManager {
    supervisor: Arc<ProcessSupervisor>,
    desktop_dir: PathBuf,
    executable_path: PathBuf,
    working_dir: PathBuf,
    env_file: PathBuf,
    installed: bool,
}

impl GatewayManager {
    fn discover() -> Self {
        let executable = env::current_exe().unwrap_or_else(|_| PathBuf::from("zhiwei-desktop.exe"));
        let executable_dir = executable
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let executable_path = resolve_gateway(&executable_dir);
        let working_dir = env::var_os("ZHIWEI_MEDIA_GATEWAY_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|path| absolutize_from(&executable_dir, &path))
            .or_else(|| executable_path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| executable_dir.clone());
        let env_file = resolve_gateway_env(&working_dir, &executable_dir);
        let python_program =
            resolve_python_interpreter(&executable_path, &working_dir, &executable_dir);
        let host_dir = if working_dir
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("media-gateway"))
        {
            working_dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| working_dir.clone())
        } else {
            working_dir.clone()
        };
        let logs_dir = host_dir.join("logs");
        let _ = fs::create_dir_all(&logs_dir);

        let installed = executable_path.is_file();
        let command = match &python_program {
            Some(program) => CommandPlan::Interpreter {
                program: program.clone(),
                entry: executable_path.clone(),
            },
            None => CommandPlan::Exe(executable_path.clone()),
        };
        let image_matcher = match &python_program {
            Some(program) => ImageMatcher::Interpreter(program.clone()),
            None => ImageMatcher::Path(executable_path.clone()),
        };
        let plan = ProcessPlan {
            name: "媒体网关",
            program_label: "媒体网关",
            log_header: "--- zhiwei-desktop starting media-gateway ---",
            log_file: logs_dir.join("media-gateway.log"),
            pid_file: logs_dir.join("media-gateway.pid"),
            working_dir: working_dir.clone(),
            command,
            image_matcher,
            env: vec![
                (
                    "MEDIA_GATEWAY_ENV".to_string(),
                    env_file.display().to_string(),
                ),
                ("PYTHONUNBUFFERED".to_string(), "1".to_string()),
            ],
            terminate_tree: true,
            spawn_error_hint: "",
        };
        let initial_message = if installed {
            "媒体网关尚未启动".to_string()
        } else {
            format!("未找到媒体网关（可选组件）：{}", executable_path.display())
        };
        let manager = Self {
            supervisor: Arc::new(ProcessSupervisor::new(
                plan,
                Box::new(spawn_process),
                installed,
                if installed { "stopped" } else { "unavailable" },
                initial_message,
            )),
            desktop_dir: executable_dir,
            executable_path,
            working_dir,
            env_file,
            installed,
        };
        if installed {
            manager.supervisor.refresh_external();
        }
        manager
    }

    fn snapshot(&self) -> GatewayStatus {
        let url = resolve_gateway_url(&self.env_file, &self.desktop_dir);
        let service_url = resolve_service_gateway_url(&self.desktop_dir);
        let mut status = self.supervisor.snapshot();
        let probe = self.probe_health();
        let external = status.phase != "running" && probe.as_ref().is_some_and(|health| health.ok);
        if external {
            status.phase = "running".to_string();
            status.pid = None;
            status.message = "媒体网关已在运行（外部启动）".to_string();
            status.auto_restart = false;
        }
        let health = if status.phase == "running" {
            probe
        } else {
            None
        };
        GatewayStatus {
            phase: status.phase,
            pid: status.pid,
            message: status.message,
            auto_restart: status.auto_restart,
            restart_attempts: status.restart_attempts,
            installed: self.installed,
            executable_path: self.executable_path.display().to_string(),
            working_dir: self.working_dir.display().to_string(),
            env_file: self.env_file.display().to_string(),
            log_file: self.supervisor.plan().log_file.display().to_string(),
            url: url.clone(),
            service_url: service_url.clone(),
            url_matches_service: urls_match(&url, &service_url),
            health,
        }
    }

    fn start(&self) -> Result<GatewayStatus, String> {
        if !self.installed {
            let message = format!("未找到媒体网关：{}", self.executable_path.display());
            self.supervisor.mark_unavailable(&message);
            return Err(message);
        }
        if self.external_running() {
            return Ok(self.snapshot());
        }
        self.supervisor.start()?;
        Ok(self.snapshot())
    }

    fn stop(&self) -> Result<GatewayStatus, String> {
        if self.external_running() {
            return Err("媒体网关由外部启动，桌面壳不管理它；请在启动它的终端里停止".to_string());
        }
        self.supervisor.stop()?;
        Ok(self.snapshot())
    }

    fn restart(&self) -> Result<GatewayStatus, String> {
        if self.external_running() {
            return Err("媒体网关由外部启动，桌面壳不管理它；请在启动它的终端里重启".to_string());
        }
        self.supervisor.restart()?;
        Ok(self.snapshot())
    }

    fn tick(&self) -> GatewayStatus {
        self.supervisor.tick();
        self.snapshot()
    }

    /// 只要端口上有健康响应就算在运行：外部手动 `python main.py` 时
    /// supervisor 没有子进程，靠这个判断避免重复拉起。
    fn external_running(&self) -> bool {
        self.supervisor.snapshot().phase != "running"
            && self.probe_health().is_some_and(|health| health.ok)
    }

    fn probe_health(&self) -> Option<GatewayHealth> {
        let url = resolve_gateway_url(&self.env_file, &self.desktop_dir);
        fetch_gateway_health(&url)
    }
}

fn resolve_service(executable_dir: &Path) -> PathBuf {
    if let Some(configured) = env::var_os("ZHIWEI_SERVICE") {
        if !configured.is_empty() {
            return absolutize(Path::new(&configured));
        }
    }

    let adjacent = executable_dir.join("zhiwei-service.exe");
    if adjacent.is_file() {
        return adjacent;
    }

    if let Some(candidate) = find_service_in_profiles(executable_dir, &preferred_service_profiles())
    {
        return candidate;
    }

    adjacent
}

/// 桌面壳自己是什么构建，就优先用同构建的 service。
///
/// 仓库里同时存在 `backend/target/debug` 与 `backend/target/release` 时，
/// 固定先找 release 会让开发态把手动启动的 debug 服务判定成“已停止”。
fn preferred_service_profiles() -> [&'static str; 2] {
    if cfg!(debug_assertions) {
        ["debug", "release"]
    } else {
        ["release", "debug"]
    }
}

fn find_service_in_profiles(executable_dir: &Path, profiles: &[&str]) -> Option<PathBuf> {
    for ancestor in executable_dir.ancestors() {
        for profile in profiles {
            let candidate = ancestor
                .join("backend")
                .join("target")
                .join(profile)
                .join("zhiwei-service.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 外部启动的服务可能来自同仓库的另一构建档（debug / release），
/// 识别存活时两个都认，避免状态显示和实际进程不一致。
fn service_image_paths(service_path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![service_path.to_path_buf()];
    let Some(profile_dir) = service_path.parent() else {
        return paths;
    };
    let Some(file_name) = service_path.file_name() else {
        return paths;
    };
    let is_profile_dir = matches!(
        profile_dir.file_name().and_then(|name| name.to_str()),
        Some("debug" | "release")
    );
    let Some(target_dir) = profile_dir.parent().filter(|_| is_profile_dir) else {
        return paths;
    };
    for profile in ["debug", "release"] {
        let candidate = target_dir.join(profile).join(file_name);
        if candidate.is_file() {
            paths.push(candidate);
        }
    }
    paths
}

fn resolve_gateway(executable_dir: &Path) -> PathBuf {
    if let Some(configured) = env::var_os("ZHIWEI_MEDIA_GATEWAY") {
        if !configured.is_empty() {
            return absolutize_from(executable_dir, Path::new(&configured));
        }
    }

    resolve_gateway_from(executable_dir)
}

fn resolve_gateway_from(executable_dir: &Path) -> PathBuf {
    let adjacent = executable_dir.join("media-gateway.exe");
    if adjacent.is_file() {
        return adjacent;
    }
    let nested = executable_dir
        .join("media-gateway")
        .join("media-gateway.exe");
    if nested.is_file() {
        return nested;
    }

    for ancestor in executable_dir.ancestors() {
        let source_entry = ancestor.join("media-gateway").join("main.py");
        if source_entry.is_file() {
            return source_entry;
        }
    }

    for ancestor in executable_dir.ancestors() {
        for candidate in [
            ancestor
                .join("media-gateway")
                .join("dist")
                .join("media-gateway.exe"),
            ancestor
                .join("media-gateway")
                .join("dist")
                .join("media-gateway")
                .join("media-gateway.exe"),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    adjacent
}

fn resolve_python_interpreter(
    entry: &Path,
    working_dir: &Path,
    base_dir: &Path,
) -> Option<PathBuf> {
    if !entry
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
    {
        return None;
    }
    if let Some(configured) = env::var_os("ZHIWEI_MEDIA_GATEWAY_PYTHON") {
        if !configured.is_empty() {
            return Some(absolutize_from(base_dir, Path::new(&configured)));
        }
    }
    let venv_dir = working_dir.join(".venv");
    let venv_python = venv_dir.join("Scripts").join("python.exe");
    if venv_python.is_file() && venv_interpreter_is_usable(&venv_dir) {
        return Some(venv_python);
    }
    Some(PathBuf::from("python"))
}

fn venv_interpreter_is_usable(venv_dir: &Path) -> bool {
    let config = fs::read_to_string(venv_dir.join("pyvenv.cfg")).unwrap_or_default();
    for line in config.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("home") {
            let home = value.trim().trim_matches('"').trim_matches('\'');
            return !home.is_empty() && Path::new(home).is_dir();
        }
    }
    false
}

fn resolve_gateway_env(working_dir: &Path, desktop_dir: &Path) -> PathBuf {
    if let Some(configured) = env::var_os("ZHIWEI_MEDIA_GATEWAY_ENV") {
        if !configured.is_empty() {
            return absolutize_from(desktop_dir, Path::new(&configured));
        }
    }

    let packaged = working_dir.join("media-gateway.env");
    if packaged.is_file() {
        return packaged;
    }
    if working_dir != desktop_dir {
        let adjacent = resolve_env_file(working_dir);
        if adjacent.is_file() {
            return adjacent;
        }
    }
    if working_dir
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("dist"))
    {
        if let Some(parent) = working_dir.parent() {
            let development_env = resolve_env_file(parent);
            if development_env.is_file() {
                return development_env;
            }
        }
    }
    packaged
}

/// 当前进程的环境档位：`ZHIWEI_ENV` 优先，否则按构建档位推断（debug=dev / release=prod）。
fn env_profile() -> String {
    env::var("ZHIWEI_ENV")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if cfg!(debug_assertions) {
                "dev".to_string()
            } else {
                "prod".to_string()
            }
        })
}

/// 在工作目录里找 `.env.<档位>`，找不到再回退旧约定 `.env`。
fn resolve_env_file(working_dir: &Path) -> PathBuf {
    let profile_file = working_dir.join(format!(".env.{}", env_profile()));
    if profile_file.is_file() {
        return profile_file;
    }
    let legacy = working_dir.join(".env");
    if legacy.is_file() {
        return legacy;
    }
    profile_file
}

fn resolve_gateway_url(env_file: &Path, desktop_dir: &Path) -> String {
    if let Some(port) = read_env_map(env_file)
        .get("PORT")
        .and_then(|value| value.trim().parse::<u16>().ok())
    {
        return format!("http://127.0.0.1:{port}");
    }
    let service_url = resolve_service_gateway_url(desktop_dir);
    if !service_url.is_empty() {
        return service_url;
    }
    "http://127.0.0.1:8787".to_string()
}

fn resolve_service_gateway_url(desktop_dir: &Path) -> String {
    read_env_map(&resolve_env_file(desktop_dir))
        .get("MEDIA_GATEWAY_URL")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

fn read_env_map(path: &Path) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return values;
    };
    for line in text.lines() {
        if let Some((key, value)) = parse_env_line(line) {
            values.insert(key, value);
        }
    }
    values
}

fn parse_env_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, value) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let mut value = value.trim().to_string();
    if value.len() >= 2
        && (value.starts_with('"') && value.ends_with('"')
            || value.starts_with('\'') && value.ends_with('\''))
    {
        value = value[1..value.len() - 1].to_string();
    }
    Some((key.to_ascii_uppercase(), value))
}

fn fetch_gateway_health(base: &str) -> Option<GatewayHealth> {
    let address = base.trim_end_matches('/').strip_prefix("http://")?;
    let mut stream = TcpStream::connect(address).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(900)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(900)));
    let request = format!("GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (status, body) = parse_http_response(&response).ok()?;
    if status != 200 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(&body).ok()?;
    let asr = value.get("asr");
    Some(GatewayHealth {
        ok: value
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        diffusion: value
            .get("diffusion")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        tts: value
            .get("tts")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        asr_engine: asr
            .and_then(|value| value.get("engine"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        asr_importable: asr
            .and_then(|value| value.get("importable"))
            .and_then(serde_json::Value::as_bool),
        asr_model_loaded: asr
            .and_then(|value| value.get("model_loaded"))
            .and_then(serde_json::Value::as_bool),
    })
}

fn urls_match(left: &str, right: &str) -> bool {
    if right.trim().is_empty() {
        return true;
    }
    left.trim()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(right.trim().trim_end_matches('/'))
}

fn infer_backend_dir(service_path: &Path) -> Option<PathBuf> {
    let profile_dir = service_path.parent()?;
    let target_dir = profile_dir.parent()?;
    if target_dir.file_name()? != "target" {
        return None;
    }
    target_dir.parent().map(Path::to_path_buf)
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn absolutize_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn acquire_single_instance() -> Result<SingleInstance, String> {
    let (instance_name, port_file) = instance_key();
    let (native_mutex, already_running) = create_native_mutex(&instance_name)?;
    if already_running {
        close_native_mutex(native_mutex);
        notify_existing_instance(&port_file);
        std::process::exit(0);
    }

    let guard = InstanceGuard {
        native_mutex,
        port_file: port_file.clone(),
    };
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| format!("无法启动单实例通信端口：{error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("无法读取单实例通信端口：{error}"))?
        .port();
    fs::write(&port_file, port.to_string())
        .map_err(|error| format!("无法写入单实例端口文件 {}：{error}", port_file.display()))?;
    Ok(SingleInstance { guard, listener })
}

fn instance_key() -> (String, PathBuf) {
    let hash = instance_hash();
    (
        format!("Local\\ZhiweiDesktop-{hash:016x}"),
        env::temp_dir().join(format!("zhiwei-desktop-{hash:016x}.port")),
    )
}

fn instance_hash() -> u64 {
    let executable = env::current_exe().unwrap_or_else(|_| PathBuf::from("zhiwei-desktop.exe"));
    let normalized = normalize_path(&executable);
    normalized
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn webview_data_directory() -> PathBuf {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("Zhiwei")
        .join("WebView2")
        .join(format!("{:016x}", instance_hash()))
}

fn notify_existing_instance(port_file: &Path) -> bool {
    let deadline = Instant::now() + INSTANCE_FORWARD_TIMEOUT;
    while Instant::now() < deadline {
        let port = fs::read_to_string(port_file)
            .ok()
            .and_then(|value| value.trim().parse::<u16>().ok())
            .is_some_and(|port| {
                if let Ok(mut stream) = TcpStream::connect((Ipv4Addr::LOCALHOST, port)) {
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                    if stream.write_all(b"show").is_ok() {
                        return true;
                    }
                }
                false
            });
        if port {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn start_instance_listener(listener: TcpListener, app: AppHandle) {
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let mut command = [0u8; 16];
            let Ok(count) = stream.read(&mut command) else {
                continue;
            };
            if &command[..count] == b"show" {
                let app = app.clone();
                let handler_app = app.clone();
                let _ = app.run_on_main_thread(move || show_settings(&handler_app));
            }
        }
    });
}

#[cfg(windows)]
fn create_native_mutex(name: &str) -> Result<(isize, bool), String> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return Err(format!("无法创建单实例互斥体（错误 {}）", unsafe {
            GetLastError()
        }));
    }
    Ok((
        handle as isize,
        unsafe { GetLastError() } == ERROR_ALREADY_EXISTS,
    ))
}

#[cfg(not(windows))]
fn create_native_mutex(_name: &str) -> Result<(isize, bool), String> {
    Ok((0, false))
}

#[cfg(windows)]
fn close_native_mutex(handle: isize) {
    if handle != 0 {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(handle as _);
        }
    }
}

#[cfg(not(windows))]
fn close_native_mutex(_handle: isize) {}

fn install_panic_logger() {
    let log_path = env::temp_dir().join("zhiwei-desktop-panic.log");
    std::panic::set_hook(Box::new(move |panic_info| {
        if let Ok(mut log) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(log, "{panic_info}");
        }
        eprintln!("{panic_info}");
    }));
}

fn show_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("settings") {
        let result = window
            .show()
            .and_then(|_| window.unminimize())
            .and_then(|_| window.set_focus());
        #[cfg(debug_assertions)]
        eprintln!("show_settings result: {result:?}");
        #[cfg(not(debug_assertions))]
        let _ = result;
    } else {
        #[cfg(debug_assertions)]
        eprintln!("show_settings: no window named settings");
    }
}

fn open_directory(path: &Path) -> Result<(), String> {
    Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开目录 {}：{error}", path.display()))
}

fn reveal_file(path: &Path) -> Result<(), String> {
    if path.is_file() {
        Command::new("explorer.exe")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("无法定位文件 {}：{error}", path.display()))
    } else {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        open_directory(parent)
    }
}

fn open_env_file(path: &Path) -> Result<(), String> {
    Command::new("notepad.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法打开配置文件 {}：{error}", path.display()))
}

#[tauri::command]
fn service_status(manager: State<'_, ServiceManager>) -> ServiceStatus {
    manager.snapshot()
}

#[tauri::command]
fn start_service(manager: State<'_, ServiceManager>) -> Result<ServiceStatus, String> {
    manager.start()
}

#[tauri::command]
fn stop_service(manager: State<'_, ServiceManager>) -> Result<ServiceStatus, String> {
    manager.stop()
}

#[tauri::command]
fn restart_service(manager: State<'_, ServiceManager>) -> Result<ServiceStatus, String> {
    manager.restart()
}

#[tauri::command]
fn open_data_folder(manager: State<'_, ServiceManager>) -> Result<(), String> {
    open_directory(&manager.working_dir)
}

#[tauri::command]
fn open_env_file_command(manager: State<'_, ServiceManager>) -> Result<(), String> {
    open_env_file(&manager.env_file)
}

#[tauri::command]
fn open_log_file(manager: State<'_, ServiceManager>) -> Result<(), String> {
    reveal_file(&manager.supervisor.plan().log_file)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewaySettingItem {
    key: String,
    description: String,
    kind: String,
    default: String,
    choices: Vec<String>,
    restart_required: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewaySettingsView {
    env_file: String,
    values: BTreeMap<String, String>,
    schema: Vec<GatewaySettingItem>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GatewaySettingsResult {
    view: GatewaySettingsView,
    restarted: bool,
    warning: Option<String>,
}

fn gateway_setting(
    key: &str,
    description: &str,
    kind: &str,
    default: &str,
    choices: &[&str],
    restart_required: bool,
) -> GatewaySettingItem {
    GatewaySettingItem {
        key: key.to_string(),
        description: description.to_string(),
        kind: kind.to_string(),
        default: default.to_string(),
        choices: choices.iter().map(|value| value.to_string()).collect(),
        restart_required,
    }
}

fn gateway_setting_schema() -> Vec<GatewaySettingItem> {
    vec![
        gateway_setting("PORT", "监听端口", "integer", "8787", &[], true),
        gateway_setting(
            "DIFFUSION_BACKEND_TYPE",
            "出图后端",
            "choice",
            "comfyui",
            &["comfyui", "a1111"],
            true,
        ),
        gateway_setting(
            "DIFFUSION_BACKEND_URL",
            "出图后端地址",
            "string",
            "http://127.0.0.1:8188",
            &[],
            true,
        ),
        gateway_setting(
            "MEDIA_HTTP_TIMEOUT",
            "媒体生成超时（秒）",
            "integer",
            "120",
            &[],
            true,
        ),
        gateway_setting(
            "COMFYUI_WORKFLOW_FILE",
            "ComfyUI 工作流文件（留空用内置）",
            "string",
            "",
            &[],
            true,
        ),
        gateway_setting(
            "TTS_ENGINE",
            "语音合成引擎",
            "choice",
            "sapi",
            &["sapi", "edge-tts"],
            true,
        ),
        gateway_setting(
            "TTS_VOICE",
            "语音音色",
            "string",
            "zh-CN-XiaoxiaoNeural",
            &[],
            true,
        ),
        gateway_setting(
            "SAPI_VOICE",
            "SAPI 音色全名（可选）",
            "string",
            "",
            &[],
            true,
        ),
        gateway_setting("FFMPEG_BIN", "ffmpeg 路径", "string", "ffmpeg", &[], true),
        gateway_setting(
            "ASR_ENGINE",
            "语音识别引擎",
            "choice",
            "faster-whisper",
            &["faster-whisper", "none"],
            true,
        ),
        gateway_setting("ASR_MODEL", "Whisper 模型", "string", "small", &[], true),
        gateway_setting("ASR_LANGUAGE", "识别语种", "string", "auto", &[], true),
        gateway_setting(
            "ASR_DEVICE",
            "识别设备",
            "choice",
            "cpu",
            &["cpu", "cuda"],
            true,
        ),
        gateway_setting("ASR_COMPUTE_TYPE", "识别精度", "string", "int8", &[], true),
        gateway_setting(
            "ASR_MODEL_DIR",
            "模型缓存目录（可选）",
            "string",
            "",
            &[],
            true,
        ),
        gateway_setting("ASR_WARMUP", "启动预热模型", "boolean", "1", &[], true),
        gateway_setting("NO_PROXY", "本机代理绕过", "string", "*", &[], true),
    ]
}

fn validate_gateway_changes(changes: &BTreeMap<String, String>) -> Result<(), String> {
    let schema = gateway_setting_schema();
    for (key, value) in changes {
        if value.contains('\r') || value.contains('\n') {
            return Err(format!("{key} 不能包含换行"));
        }
        let item = schema
            .iter()
            .find(|item| item.key == *key)
            .ok_or_else(|| format!("未知的媒体网关配置键：{key}"))?;
        match item.kind.as_str() {
            "integer" => {
                let parsed = value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| format!("{} 必须是整数", item.description))?;
                let (min, max) = if key == "PORT" {
                    (1, 65_535)
                } else {
                    (1, 3_600)
                };
                if parsed < min || parsed > max {
                    return Err(format!("{} 必须在 {min}..{max} 之间", item.description));
                }
            }
            "boolean" => {
                let normalized = value.trim().to_ascii_lowercase();
                if !matches!(
                    normalized.as_str(),
                    "1" | "0" | "true" | "false" | "yes" | "no" | "on" | "off"
                ) {
                    return Err(format!("{} 只接受 1/0/true/false", item.description));
                }
            }
            "choice" => {
                if !item.choices.iter().any(|choice| choice == value.trim()) {
                    return Err(format!(
                        "{} 只能是 {}",
                        item.description,
                        item.choices.join(" / ")
                    ));
                }
            }
            _ => {
                let trimmed = value.trim();
                if key == "DIFFUSION_BACKEND_URL"
                    && !(trimmed.starts_with("http://") || trimmed.starts_with("https://"))
                {
                    return Err("出图后端地址必须以 http:// 或 https:// 开头".to_string());
                }
                if key == "FFMPEG_BIN" && trimmed.is_empty() {
                    return Err("ffmpeg 路径不能为空".to_string());
                }
            }
        }
    }
    Ok(())
}

fn write_gateway_env(path: &Path, changes: &BTreeMap<String, String>) -> Result<(), String> {
    validate_gateway_changes(changes)?;
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut remaining = changes.clone();
    let mut lines = Vec::new();
    if existing.trim().is_empty() {
        lines.push("# media-gateway 配置（由 zhiwei-desktop 管理）".to_string());
    }
    for line in existing.lines() {
        if let Some((key, _)) = parse_env_line(line) {
            if let Some(value) = remaining.remove(&key) {
                lines.push(format!("{key}={value}"));
                continue;
            }
        }
        lines.push(line.to_string());
    }
    for (key, value) in remaining {
        lines.push(format!("{key}={value}"));
    }
    let mut text = lines.join("\n");
    text.push('\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建媒体网关配置目录 {}：{error}", parent.display()))?;
    }
    fs::write(path, text)
        .map_err(|error| format!("无法写入媒体网关配置 {}：{error}", path.display()))
}

fn gateway_settings_view(manager: &GatewayManager) -> GatewaySettingsView {
    GatewaySettingsView {
        env_file: manager.env_file.display().to_string(),
        values: read_env_map(&manager.env_file),
        schema: gateway_setting_schema(),
    }
}

#[tauri::command]
fn gateway_status(manager: State<'_, GatewayManager>) -> GatewayStatus {
    manager.snapshot()
}

#[tauri::command]
fn start_gateway(manager: State<'_, GatewayManager>) -> Result<GatewayStatus, String> {
    manager.start()
}

#[tauri::command]
fn stop_gateway(manager: State<'_, GatewayManager>) -> Result<GatewayStatus, String> {
    manager.stop()
}

#[tauri::command]
fn restart_gateway(manager: State<'_, GatewayManager>) -> Result<GatewayStatus, String> {
    manager.restart()
}

#[tauri::command]
fn open_gateway_folder(manager: State<'_, GatewayManager>) -> Result<(), String> {
    open_directory(&manager.working_dir)
}

#[tauri::command]
fn open_gateway_env_file(manager: State<'_, GatewayManager>) -> Result<(), String> {
    if !manager.env_file.is_file() {
        let defaults = gateway_setting_schema()
            .into_iter()
            .map(|item| (item.key, item.default))
            .collect::<BTreeMap<_, _>>();
        write_gateway_env(&manager.env_file, &defaults)?;
    }
    open_env_file(&manager.env_file)
}

#[tauri::command]
fn open_gateway_log_file(manager: State<'_, GatewayManager>) -> Result<(), String> {
    reveal_file(&manager.supervisor.plan().log_file)
}

#[tauri::command]
fn gateway_settings(manager: State<'_, GatewayManager>) -> Result<GatewaySettingsView, String> {
    Ok(gateway_settings_view(&manager))
}

#[tauri::command]
fn update_gateway_settings(
    manager: State<'_, GatewayManager>,
    changes: BTreeMap<String, String>,
) -> Result<GatewaySettingsResult, String> {
    let normalized = changes
        .into_iter()
        .map(|(key, value)| (key.trim().to_ascii_uppercase(), value.trim().to_string()))
        .collect::<BTreeMap<_, _>>();
    write_gateway_env(&manager.env_file, &normalized)?;

    let was_running = manager.snapshot().phase == "running";
    let mut warning = None;
    if was_running {
        if let Err(error) = manager.restart() {
            warning = Some(format!("配置已保存，但媒体网关重启失败：{error}"));
        }
    }
    Ok(GatewaySettingsResult {
        view: gateway_settings_view(&manager),
        restarted: was_running && warning.is_none(),
        warning,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlState {
    address: String,
}

#[derive(Debug, Deserialize)]
struct ControlHealth {
    pid: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsPatch {
    expected_revision: i64,
    operator: String,
    changes: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlResult<T> {
    ok: bool,
    data: Option<T>,
    error: Option<String>,
}

fn control_call<T: for<'de> Deserialize<'de>>(
    manager: &ServiceManager,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<T, String> {
    let state: ControlState = serde_json::from_str(
        &fs::read_to_string(&manager.control_state_file)
            .map_err(|error| format!("控制接口尚未就绪：{error}"))?,
    )
    .map_err(|error| format!("控制状态文件无效：{error}"))?;
    let token = fs::read_to_string(&manager.control_token_file)
        .map_err(|error| format!("无法读取控制 token：{error}"))?;
    let response = control_http(&state.address, method, path, token.trim(), body.as_ref())?;
    let (status, body) = parse_http_response(&response)?;
    if !(200..300).contains(&status) {
        return Err(format!("控制接口返回 {status}：{body}"));
    }
    serde_json::from_str(&body).map_err(|error| format!("控制接口响应无效：{error}"))
}

fn control_http(
    base: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&serde_json::Value>,
) -> Result<String, String> {
    control_http_with_timeout(base, method, path, token, body, Duration::from_secs(5))
}

fn control_health_pid(state_file: &Path, token_file: &Path) -> Option<u32> {
    let state: ControlState = serde_json::from_str(&fs::read_to_string(state_file).ok()?).ok()?;
    let token = fs::read_to_string(token_file).ok()?;
    let response = control_http_with_timeout(
        &state.address,
        "GET",
        "/control/v1/health",
        token.trim(),
        None,
        Duration::from_millis(800),
    )
    .ok()?;
    let (status, body) = parse_http_response(&response).ok()?;
    if status != 200 {
        return None;
    }
    serde_json::from_str::<ControlHealth>(&body)
        .ok()
        .map(|health| health.pid)
}

fn control_http_with_timeout(
    base: &str,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&serde_json::Value>,
    timeout: Duration,
) -> Result<String, String> {
    let address = base
        .strip_prefix("http://")
        .ok_or_else(|| "控制接口只支持 loopback HTTP".to_string())?;
    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("无法连接控制接口 {address}：{error}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let body = body
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| format!("控制请求序列化失败：{error}"))?
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("控制请求发送失败：{error}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("控制响应读取失败：{error}"))?;
    Ok(response)
}

fn parse_http_response(response: &str) -> Result<(u16, String), String> {
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "控制响应缺少 HTTP 头".to_string())?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "控制响应状态码无效".to_string())?;
    Ok((status, body.to_string()))
}

#[tauri::command]
fn control_health(manager: State<'_, ServiceManager>) -> ControlResult<serde_json::Value> {
    match control_call::<serde_json::Value>(&manager, "GET", "/control/v1/health", None) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn settings_view(manager: State<'_, ServiceManager>) -> ControlResult<serde_json::Value> {
    match control_call::<serde_json::Value>(&manager, "GET", "/control/v1/settings", None) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn settings_schema(manager: State<'_, ServiceManager>) -> ControlResult<serde_json::Value> {
    match control_call::<serde_json::Value>(&manager, "GET", "/control/v1/settings/schema", None) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn update_settings(
    manager: State<'_, ServiceManager>,
    expected_revision: i64,
    changes: serde_json::Value,
) -> ControlResult<serde_json::Value> {
    let body = serde_json::to_value(SettingsPatch {
        expected_revision,
        operator: "desktop".to_string(),
        changes,
    })
    .unwrap_or(serde_json::Value::Null);
    match control_call::<serde_json::Value>(&manager, "PATCH", "/control/v1/settings", Some(body)) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn identity_overview(manager: State<'_, ServiceManager>) -> ControlResult<serde_json::Value> {
    match control_call::<serde_json::Value>(&manager, "GET", "/control/v1/identity", None) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn identity_proposals(
    manager: State<'_, ServiceManager>,
    limit: Option<i64>,
) -> ControlResult<serde_json::Value> {
    let limit = limit.unwrap_or(50);
    let path = format!("/control/v1/identity/proposals?limit={limit}");
    match control_call::<serde_json::Value>(&manager, "GET", &path, None) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn approve_identity_proposal(
    manager: State<'_, ServiceManager>,
    proposal_id: String,
    explicit_consent: bool,
) -> ControlResult<serde_json::Value> {
    let path = format!("/control/v1/identity/proposals/{proposal_id}/approve");
    let body = serde_json::json!({ "operator": "desktop", "explicitConsent": explicit_consent });
    match control_call::<serde_json::Value>(&manager, "POST", &path, Some(body)) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn reject_identity_proposal(
    manager: State<'_, ServiceManager>,
    proposal_id: String,
) -> ControlResult<serde_json::Value> {
    let path = format!("/control/v1/identity/proposals/{proposal_id}/reject");
    let body = serde_json::json!({ "operator": "desktop" });
    match control_call::<serde_json::Value>(&manager, "POST", &path, Some(body)) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn rollback_identity(
    manager: State<'_, ServiceManager>,
    version: Option<i64>,
) -> ControlResult<serde_json::Value> {
    let body = serde_json::json!({ "operator": "desktop", "version": version });
    match control_call::<serde_json::Value>(
        &manager,
        "POST",
        "/control/v1/identity/rollback",
        Some(body),
    ) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

#[tauri::command]
fn trigger_evolution(manager: State<'_, ServiceManager>) -> ControlResult<serde_json::Value> {
    match control_call::<serde_json::Value>(&manager, "POST", "/control/v1/identity/evolve", None) {
        Ok(data) => ControlResult {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => ControlResult {
            ok: false,
            data: None,
            error: Some(error),
        },
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开知微", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "启动通道", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "重启通道", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "停止通道", true, None::<&str>)?;
    let gateway_start =
        MenuItem::with_id(app, "gateway_start", "启动媒体网关", true, None::<&str>)?;
    let gateway_restart =
        MenuItem::with_id(app, "gateway_restart", "重启媒体网关", true, None::<&str>)?;
    let gateway_stop = MenuItem::with_id(app, "gateway_stop", "停止媒体网关", true, None::<&str>)?;
    let logs = MenuItem::with_id(app, "logs", "查看日志", true, None::<&str>)?;
    let gateway_logs =
        MenuItem::with_id(app, "gateway_logs", "查看媒体网关日志", true, None::<&str>)?;
    let gateway_env =
        MenuItem::with_id(app, "gateway_env", "编辑媒体网关配置", true, None::<&str>)?;
    let folder = MenuItem::with_id(app, "folder", "打开程序目录", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出知微", true, None::<&str>)?;
    let first_separator = PredefinedMenuItem::separator(app)?;
    let second_separator = PredefinedMenuItem::separator(app)?;
    let third_separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &first_separator,
            &start,
            &restart,
            &stop,
            &second_separator,
            &gateway_start,
            &gateway_restart,
            &gateway_stop,
            &gateway_logs,
            &gateway_env,
            &logs,
            &folder,
            &third_separator,
            &quit,
        ],
    )?;

    let mut tray = TrayIconBuilder::with_id("zhiwei")
        .tooltip("知微 Zhiwei")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_settings(app),
            "start" => {
                let manager = app.state::<ServiceManager>();
                let _ = manager.start();
            }
            "restart" => {
                let manager = app.state::<ServiceManager>();
                let _ = manager.restart();
            }
            "stop" => {
                let manager = app.state::<ServiceManager>();
                let _ = manager.stop();
            }
            "gateway_start" => {
                let manager = app.state::<GatewayManager>();
                let _ = manager.start();
            }
            "gateway_restart" => {
                let manager = app.state::<GatewayManager>();
                let _ = manager.restart();
            }
            "gateway_stop" => {
                let manager = app.state::<GatewayManager>();
                let _ = manager.stop();
            }
            "logs" => {
                let manager = app.state::<ServiceManager>();
                let _ = reveal_file(&manager.supervisor.plan().log_file);
            }
            "gateway_logs" => {
                let manager = app.state::<GatewayManager>();
                let _ = reveal_file(&manager.supervisor.plan().log_file);
            }
            "gateway_env" => {
                let manager = app.state::<GatewayManager>();
                let _ = open_env_file(&manager.env_file);
            }
            "folder" => {
                let manager = app.state::<ServiceManager>();
                let _ = open_directory(&manager.working_dir);
            }
            "quit" => {
                let manager = app.state::<ServiceManager>();
                let _ = manager.stop();
                let gateway = app.state::<GatewayManager>();
                let _ = gateway.stop();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_settings(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    install_panic_logger();
    let instance = acquire_single_instance().unwrap_or_else(|error| {
        panic!("failed to initialize single instance: {error}");
    });
    tauri::Builder::default()
        .setup(move |app| {
            let SingleInstance { guard, listener } = instance;
            app.manage(guard);
            start_instance_listener(listener, app.handle().clone());
            let manager = ServiceManager::discover().map_err(std::io::Error::other)?;
            app.manage(manager.clone());
            let gateway = GatewayManager::discover();
            let gateway_installed = gateway.installed;
            app.manage(gateway.clone());
            WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("index.html".into()))
                .title("知微 Zhiwei")
                .inner_size(820.0, 720.0)
                .min_inner_size(640.0, 560.0)
                .center()
                .visible(false)
                .resizable(true)
                .data_directory(webview_data_directory())
                .build()?;
            build_tray(app)?;
            if env::var_os("ZHIWEI_SHOW_ON_START").is_some() {
                show_settings(app.handle());
            }

            // 开发态（tauri dev / debug 构建）不自启：service 与媒体网关由开发者
            // 先手动拉起，桌面壳只当 UI 并识别外部进程。发布版维持托管自启。
            if !cfg!(debug_assertions) {
                let _ = manager.start();
                if gateway_installed {
                    let _ = gateway.start();
                }
            }
            let app_handle = app.handle().clone();
            thread::spawn(move || loop {
                thread::sleep(Duration::from_secs(2));
                let status = manager.tick();
                let _ = app_handle.emit(SERVICE_STATUS_EVENT, status);
                let status = gateway.tick();
                let _ = app_handle.emit(GATEWAY_STATUS_EVENT, status);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "settings" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            service_status,
            start_service,
            stop_service,
            restart_service,
            open_data_folder,
            open_env_file_command,
            open_log_file,
            gateway_status,
            start_gateway,
            stop_gateway,
            restart_gateway,
            open_gateway_folder,
            open_gateway_env_file,
            open_gateway_log_file,
            gateway_settings,
            update_gateway_settings,
            control_health,
            settings_view,
            settings_schema,
            update_settings,
            identity_overview,
            identity_proposals,
            approve_identity_proposal,
            reject_identity_proposal,
            rollback_identity,
            trigger_evolution
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Zhiwei desktop");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_directory_is_inferred_from_cargo_output() {
        let path = Path::new(r"D:\repo\backend\target\release\zhiwei-service.exe");
        assert_eq!(
            infer_backend_dir(path),
            Some(PathBuf::from(r"D:\repo\backend"))
        );
        assert_eq!(
            infer_backend_dir(Path::new(r"D:\Zhiwei\zhiwei-service.exe")),
            None
        );
    }

    #[test]
    fn parses_control_http_status_and_json_body() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}";
        assert_eq!(
            parse_http_response(response).unwrap(),
            (200, "{\"ok\":true}".to_string())
        );
    }

    #[test]
    fn rejects_control_response_without_headers() {
        assert!(parse_http_response("not-http").is_err());
    }

    #[test]
    fn parses_gateway_env_lines_and_quotes() {
        assert_eq!(
            parse_env_line("PORT=8787"),
            Some(("PORT".to_string(), "8787".to_string()))
        );
        assert_eq!(
            parse_env_line("NO_PROXY='*'"),
            Some(("NO_PROXY".to_string(), "*".to_string()))
        );
        assert_eq!(parse_env_line("# comment"), None);
        assert_eq!(parse_env_line("not-an-assignment"), None);
    }

    #[test]
    fn gateway_settings_validation_rejects_bad_values() {
        let mut changes = BTreeMap::new();
        changes.insert("PORT".to_string(), "70000".to_string());
        assert!(validate_gateway_changes(&changes).is_err());

        changes.insert("PORT".to_string(), "8787".to_string());
        changes.insert("TTS_ENGINE".to_string(), "unknown".to_string());
        assert!(validate_gateway_changes(&changes).is_err());

        changes.insert("TTS_ENGINE".to_string(), "sapi".to_string());
        changes.insert("ASR_WARMUP".to_string(), "maybe".to_string());
        assert!(validate_gateway_changes(&changes).is_err());

        changes.insert("ASR_WARMUP".to_string(), "1".to_string());
        assert!(validate_gateway_changes(&changes).is_ok());
    }

    #[test]
    fn compares_gateway_urls_without_trailing_slash() {
        assert!(urls_match(
            "http://127.0.0.1:8787/",
            "http://127.0.0.1:8787"
        ));
        assert!(urls_match("http://127.0.0.1:8787", ""));
        assert!(!urls_match(
            "http://127.0.0.1:8788",
            "http://127.0.0.1:8787"
        ));
    }

    #[test]
    fn gateway_resolution_prefers_source_entry_then_packaged_exe() {
        let root =
            std::env::temp_dir().join(format!("zhiwei-gateway-resolve-{}", std::process::id()));
        let base = root.join("desktop").join("app");
        let gateway_dir = root.join("media-gateway");
        let source_entry = gateway_dir.join("main.py");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&gateway_dir).unwrap();

        let dist = gateway_dir.join("dist");
        fs::create_dir_all(&dist).unwrap();
        let packaged = dist.join("media-gateway.exe");
        fs::write(&packaged, b"exe").unwrap();

        // 只有打包产物时用打包版。
        assert_eq!(resolve_gateway_from(&base), packaged);

        // 仓库里有源码入口时优先跑源码（开发态改动立即生效）。
        fs::write(&source_entry, "# gateway entry\n").unwrap();
        assert_eq!(resolve_gateway_from(&base), source_entry);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn service_resolution_prefers_matching_build_profile() {
        let root =
            std::env::temp_dir().join(format!("zhiwei-service-resolve-{}", std::process::id()));
        let base = root
            .join("desktop")
            .join("src-tauri")
            .join("target")
            .join("debug");
        let debug = root
            .join("backend")
            .join("target")
            .join("debug")
            .join("zhiwei-service.exe");
        let release = root
            .join("backend")
            .join("target")
            .join("release")
            .join("zhiwei-service.exe");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(debug.parent().unwrap()).unwrap();
        fs::create_dir_all(release.parent().unwrap()).unwrap();
        fs::write(&debug, b"debug").unwrap();
        fs::write(&release, b"release").unwrap();

        assert_eq!(
            find_service_in_profiles(&base, &["debug", "release"]),
            Some(debug.clone())
        );
        assert_eq!(
            find_service_in_profiles(&base, &["release", "debug"]),
            Some(release.clone())
        );

        let paths = service_image_paths(&debug);
        assert!(paths.contains(&debug));
        assert!(paths.contains(&release));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn control_health_reports_running_service_pid() {
        let root =
            std::env::temp_dir().join(format!("zhiwei-control-health-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let pid = std::process::id();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let body = format!("{{\"status\":\"ok\",\"pid\":{pid},\"version\":\"test\"}}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let state_file = root.join("control.json");
        let token_file = root.join("control.token");
        fs::write(&state_file, format!("{{\"address\":\"{address}\"}}")).unwrap();
        fs::write(&token_file, "test-token").unwrap();
        assert_eq!(control_health_pid(&state_file, &token_file), Some(pid));

        server.join().unwrap();
        fs::remove_dir_all(&root).unwrap();
    }
}
