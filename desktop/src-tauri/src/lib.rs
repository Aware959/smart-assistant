use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
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
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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

struct StatusState {
    phase: String,
    pid: Option<u32>,
    message: String,
    restart_attempts: u32,
}

struct ServiceInner {
    child: Mutex<Option<Child>>,
    status: Mutex<StatusState>,
    auto_restart: AtomicBool,
    next_restart: Mutex<Option<Instant>>,
    service_path: PathBuf,
    working_dir: PathBuf,
    env_file: PathBuf,
    log_file: PathBuf,
    pid_file: PathBuf,
    control_state_file: PathBuf,
    control_token_file: PathBuf,
}

#[derive(Clone)]
struct ServiceManager {
    inner: Arc<ServiceInner>,
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

        let manager = Self {
            inner: Arc::new(ServiceInner {
                child: Mutex::new(None),
                status: Mutex::new(StatusState {
                    phase: "stopped".to_string(),
                    pid: None,
                    message: "服务尚未启动".to_string(),
                    restart_attempts: 0,
                }),
                auto_restart: AtomicBool::new(true),
                next_restart: Mutex::new(None),
                service_path,
                env_file: working_dir.join(".env"),
                log_file: logs_dir.join("zhiwei-service.log"),
                pid_file: logs_dir.join("zhiwei-service.pid"),
                control_state_file: logs_dir.join("control.json"),
                control_token_file: logs_dir.join("control.token"),
                working_dir,
            }),
        };
        manager.refresh_existing_process();
        Ok(manager)
    }

    fn snapshot(&self) -> ServiceStatus {
        let status = self
            .inner
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        ServiceStatus {
            phase: status.phase.clone(),
            pid: status.pid,
            message: status.message.clone(),
            auto_restart: self.inner.auto_restart.load(Ordering::Relaxed),
            restart_attempts: status.restart_attempts,
            service_path: self.inner.service_path.display().to_string(),
            working_dir: self.inner.working_dir.display().to_string(),
            env_file: self.inner.env_file.display().to_string(),
            log_file: self.inner.log_file.display().to_string(),
            pid_file: self.inner.pid_file.display().to_string(),
            control_state_file: self.inner.control_state_file.display().to_string(),
        }
    }

    fn start(&self) -> Result<ServiceStatus, String> {
        self.inner.auto_restart.store(true, Ordering::Relaxed);
        *self
            .inner
            .next_restart
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        {
            let mut status = self
                .inner
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            status.restart_attempts = 0;
        }
        self.start_process()?;
        Ok(self.snapshot())
    }

    fn stop(&self) -> Result<ServiceStatus, String> {
        self.inner.auto_restart.store(false, Ordering::Relaxed);
        *self
            .inner
            .next_restart
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        self.set_status("starting", None, "正在停止通道守护……", None);

        let child = self
            .inner
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        } else if let Some(pid) = read_pid(&self.inner.pid_file) {
            if process_matches(pid, &self.inner.service_path) {
                terminate_process(pid);
            }
        }

        remove_pid_file(&self.inner.pid_file);
        self.set_status("stopped", None, "通道守护已停止", Some(0));
        Ok(self.snapshot())
    }

    fn restart(&self) -> Result<ServiceStatus, String> {
        self.stop()?;
        thread::sleep(Duration::from_millis(350));
        self.start()
    }

    fn tick(&self) -> ServiceStatus {
        let exited = {
            let mut child = self
                .inner
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match child.as_mut() {
                Some(process) => match process.try_wait() {
                    Ok(Some(exit_status)) => {
                        *child = None;
                        Some(exit_status.code())
                    }
                    Ok(None) => None,
                    Err(error) => {
                        *child = None;
                        self.set_status(
                            "error",
                            None,
                            &format!("无法读取通道守护状态：{error}"),
                            None,
                        );
                        None
                    }
                },
                None => None,
            }
        };

        if let Some(code) = exited {
            remove_pid_file(&self.inner.pid_file);
            if self.inner.auto_restart.load(Ordering::Relaxed) {
                self.schedule_restart(code);
            } else {
                let message = exit_message(code);
                self.set_status("stopped", None, &message, Some(0));
            }
        } else if self
            .inner
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_none()
        {
            if let Some(pid) =
                active_external_process(&self.inner.pid_file, &self.inner.service_path)
            {
                let status = self
                    .inner
                    .status
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if status.pid != Some(pid) || status.phase != "running" {
                    drop(status);
                    self.set_status("running", Some(pid), "通道守护正在运行（外部进程）", None);
                }
            } else if self.inner.auto_restart.load(Ordering::Relaxed) {
                let was_running = {
                    let status = self
                        .inner
                        .status
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    status.phase == "running"
                };
                if was_running {
                    self.schedule_restart(None);
                }
            }
        }

        let restart_due = {
            let next = self
                .inner
                .next_restart
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            next.is_some_and(|deadline| Instant::now() >= deadline)
        };
        if restart_due && self.inner.auto_restart.load(Ordering::Relaxed) {
            *self
                .inner
                .next_restart
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
            if let Err(error) = self.start_process() {
                self.schedule_restart_after_error(&error);
            }
        }

        self.snapshot()
    }

    fn start_process(&self) -> Result<(), String> {
        if read_pid(&self.inner.pid_file)
            .is_some_and(|pid| process_matches(pid, &self.inner.service_path))
        {
            let pid = read_pid(&self.inner.pid_file);
            self.set_status("running", pid, "通道守护已在运行", None);
            return Ok(());
        }

        {
            let mut child = self
                .inner
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(process) = child.as_mut() {
                match process.try_wait() {
                    Ok(None) => return Ok(()),
                    Ok(Some(_)) => *child = None,
                    Err(error) => {
                        return Err(format!("无法读取现有通道守护状态：{error}"));
                    }
                }
            }
        }

        if !self.inner.service_path.is_file() {
            let message = format!(
                "未找到 zhiwei-service.exe：{}",
                self.inner.service_path.display()
            );
            self.set_status("error", None, &message, None);
            return Err(message);
        }

        self.set_status("starting", None, "正在启动通道守护……", None);
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inner.log_file)
            .map_err(|error| {
                format!(
                    "无法打开日志文件 {}：{error}",
                    self.inner.log_file.display()
                )
            })?;
        let mut header = log
            .try_clone()
            .map_err(|error| format!("无法复制日志文件句柄：{error}"))?;
        let _ = writeln!(header, "\n--- zhiwei-desktop starting zhiwei-service ---");

        let mut command = Command::new(&self.inner.service_path);
        command
            .current_dir(&self.inner.working_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(|error| {
                format!("无法连接服务标准输出到日志：{error}")
            })?))
            .stderr(Stdio::from(log));
        hide_console(&mut command);

        let child = command.spawn().map_err(|error| {
            let message = format!(
                "无法启动 zhiwei-service.exe：{error}。如果系统提示 Application Control，请在 Windows 安全策略中信任该程序。"
            );
            self.set_status("error", None, &message, None);
            message
        })?;
        let pid = child.id();
        if let Err(error) = fs::write(&self.inner.pid_file, pid.to_string()) {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            let message = format!(
                "无法写入 PID 文件 {}：{error}",
                self.inner.pid_file.display()
            );
            self.set_status("error", None, &message, None);
            return Err(message);
        }
        *self
            .inner
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(child);
        self.set_status("running", Some(pid), "通道守护正在运行", Some(0));
        Ok(())
    }

    fn refresh_existing_process(&self) {
        if let Some(pid) = active_external_process(&self.inner.pid_file, &self.inner.service_path) {
            self.set_status("running", Some(pid), "通道守护已在运行", None);
        } else if !self.inner.service_path.is_file() {
            self.set_status(
                "error",
                None,
                &format!(
                    "未找到 zhiwei-service.exe：{}",
                    self.inner.service_path.display()
                ),
                None,
            );
        } else {
            self.set_status("stopped", None, "服务尚未启动", Some(0));
        }
    }

    fn schedule_restart(&self, exit_code: Option<i32>) {
        let attempts = {
            let mut status = self
                .inner
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            status.restart_attempts = status.restart_attempts.saturating_add(1);
            status.restart_attempts
        };
        self.schedule_restart_at(attempts, exit_code);
    }

    fn schedule_restart_after_error(&self, error: &str) {
        let attempts = {
            let mut status = self
                .inner
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            status.restart_attempts = status.restart_attempts.saturating_add(1);
            status.restart_attempts
        };
        let delay = restart_delay(attempts);
        *self
            .inner
            .next_restart
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some(Instant::now() + Duration::from_secs(delay));
        self.set_status(
            "error",
            None,
            &format!("{error}；{delay} 秒后重试"),
            Some(attempts),
        );
    }

    fn schedule_restart_at(&self, attempts: u32, exit_code: Option<i32>) {
        let delay = restart_delay(attempts);
        *self
            .inner
            .next_restart
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some(Instant::now() + Duration::from_secs(delay));
        self.set_status(
            "error",
            None,
            &format!("{}；{} 秒后自动恢复", exit_message(exit_code), delay),
            Some(attempts),
        );
    }

    fn set_status(
        &self,
        phase: &str,
        pid: Option<u32>,
        message: &str,
        restart_attempts: Option<u32>,
    ) {
        let mut status = self
            .inner
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        status.phase = phase.to_string();
        status.pid = pid;
        status.message = message.to_string();
        if let Some(attempts) = restart_attempts {
            status.restart_attempts = attempts;
        }
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

struct GatewayStatusState {
    phase: String,
    pid: Option<u32>,
    message: String,
    restart_attempts: u32,
    health: Option<GatewayHealth>,
}

struct GatewayInner {
    child: Mutex<Option<Child>>,
    status: Mutex<GatewayStatusState>,
    auto_restart: AtomicBool,
    next_restart: Mutex<Option<Instant>>,
    executable_path: PathBuf,
    working_dir: PathBuf,
    env_file: PathBuf,
    log_file: PathBuf,
    pid_file: PathBuf,
    desktop_dir: PathBuf,
    installed: bool,
}

#[derive(Clone)]
struct GatewayManager {
    inner: Arc<GatewayInner>,
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
        let message = if installed {
            "媒体网关尚未启动".to_string()
        } else {
            format!(
                "未找到 media-gateway.exe（可选组件）：{}",
                executable_path.display()
            )
        };
        let manager = Self {
            inner: Arc::new(GatewayInner {
                child: Mutex::new(None),
                status: Mutex::new(GatewayStatusState {
                    phase: if installed { "stopped" } else { "unavailable" }.to_string(),
                    pid: None,
                    message,
                    restart_attempts: 0,
                    health: None,
                }),
                auto_restart: AtomicBool::new(installed),
                next_restart: Mutex::new(None),
                executable_path,
                working_dir,
                env_file,
                log_file: logs_dir.join("media-gateway.log"),
                pid_file: logs_dir.join("media-gateway.pid"),
                desktop_dir: executable_dir,
                installed,
            }),
        };
        manager.refresh_existing_process();
        manager
    }

    fn snapshot(&self) -> GatewayStatus {
        let url = resolve_gateway_url(&self.inner.env_file, &self.inner.desktop_dir);
        let service_url = resolve_service_gateway_url(&self.inner.desktop_dir);
        let status = self
            .inner
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        GatewayStatus {
            phase: status.phase.clone(),
            pid: status.pid,
            message: status.message.clone(),
            auto_restart: self.inner.auto_restart.load(Ordering::Relaxed),
            restart_attempts: status.restart_attempts,
            installed: self.inner.installed,
            executable_path: self.inner.executable_path.display().to_string(),
            working_dir: self.inner.working_dir.display().to_string(),
            env_file: self.inner.env_file.display().to_string(),
            log_file: self.inner.log_file.display().to_string(),
            url: url.clone(),
            service_url: service_url.clone(),
            url_matches_service: urls_match(&url, &service_url),
            health: status.health.clone(),
        }
    }

    fn start(&self) -> Result<GatewayStatus, String> {
        if !self.inner.installed {
            let message = format!(
                "未找到 media-gateway.exe：{}",
                self.inner.executable_path.display()
            );
            self.set_status("unavailable", None, &message, None);
            return Err(message);
        }
        self.inner.auto_restart.store(true, Ordering::Relaxed);
        *self
            .inner
            .next_restart
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        {
            let mut status = self
                .inner
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            status.restart_attempts = 0;
        }
        self.start_process()?;
        Ok(self.snapshot())
    }

    fn stop(&self) -> Result<GatewayStatus, String> {
        self.inner.auto_restart.store(false, Ordering::Relaxed);
        *self
            .inner
            .next_restart
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        self.set_status("starting", None, "正在停止媒体网关……", None);

        let child = self
            .inner
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(mut child) = child {
            let pid = child.id();
            terminate_process_tree(pid);
            let _ = child.kill();
            let _ = child.wait();
        } else if let Some(pid) = read_pid(&self.inner.pid_file) {
            if process_matches(pid, &self.inner.executable_path) {
                terminate_process_tree(pid);
            }
        }

        remove_pid_file(&self.inner.pid_file);
        self.set_status("stopped", None, "媒体网关已停止", Some(0));
        Ok(self.snapshot())
    }

    fn restart(&self) -> Result<GatewayStatus, String> {
        self.stop()?;
        thread::sleep(Duration::from_millis(350));
        self.start()
    }

    fn tick(&self) -> GatewayStatus {
        let exited = {
            let mut child = self
                .inner
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match child.as_mut() {
                Some(process) => match process.try_wait() {
                    Ok(Some(exit_status)) => {
                        *child = None;
                        Some(exit_status.code())
                    }
                    Ok(None) => None,
                    Err(error) => {
                        *child = None;
                        self.set_status(
                            "error",
                            None,
                            &format!("无法读取媒体网关状态：{error}"),
                            None,
                        );
                        None
                    }
                },
                None => None,
            }
        };

        if let Some(code) = exited {
            remove_pid_file(&self.inner.pid_file);
            if self.inner.auto_restart.load(Ordering::Relaxed) {
                self.schedule_restart(code);
            } else {
                let message = gateway_exit_message(code);
                self.set_status("stopped", None, &message, Some(0));
            }
        } else if self
            .inner
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_none()
        {
            if let Some(pid) =
                active_external_process(&self.inner.pid_file, &self.inner.executable_path)
            {
                let status = self
                    .inner
                    .status
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if status.pid != Some(pid) || status.phase != "running" {
                    drop(status);
                    self.set_status("running", Some(pid), "媒体网关正在运行（外部进程）", None);
                }
            } else if self.inner.auto_restart.load(Ordering::Relaxed) {
                let was_running = {
                    let status = self
                        .inner
                        .status
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    status.phase == "running"
                };
                if was_running {
                    self.schedule_restart(None);
                }
            }
        }

        let restart_due = {
            let next = self
                .inner
                .next_restart
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            next.is_some_and(|deadline| Instant::now() >= deadline)
        };
        if restart_due && self.inner.auto_restart.load(Ordering::Relaxed) {
            *self
                .inner
                .next_restart
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
            if let Err(error) = self.start_process() {
                self.schedule_restart_after_error(&error);
            }
        }

        self.refresh_health();
        self.snapshot()
    }

    fn start_process(&self) -> Result<(), String> {
        if read_pid(&self.inner.pid_file)
            .is_some_and(|pid| process_matches(pid, &self.inner.executable_path))
        {
            let pid = read_pid(&self.inner.pid_file);
            self.set_status("running", pid, "媒体网关已在运行", None);
            return Ok(());
        }

        {
            let mut child = self
                .inner
                .child
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(process) = child.as_mut() {
                match process.try_wait() {
                    Ok(None) => return Ok(()),
                    Ok(Some(_)) => *child = None,
                    Err(error) => {
                        return Err(format!("无法读取现有媒体网关状态：{error}"));
                    }
                }
            }
        }

        if !self.inner.executable_path.is_file() {
            let message = format!(
                "未找到 media-gateway.exe：{}",
                self.inner.executable_path.display()
            );
            self.set_status("unavailable", None, &message, None);
            return Err(message);
        }

        self.set_status("starting", None, "正在启动媒体网关……", None);
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inner.log_file)
            .map_err(|error| {
                format!(
                    "无法打开媒体网关日志 {}：{error}",
                    self.inner.log_file.display()
                )
            })?;
        let mut header = log
            .try_clone()
            .map_err(|error| format!("无法复制媒体网关日志句柄：{error}"))?;
        let _ = writeln!(header, "\n--- zhiwei-desktop starting media-gateway ---");

        let mut command = Command::new(&self.inner.executable_path);
        command
            .current_dir(&self.inner.working_dir)
            .env("MEDIA_GATEWAY_ENV", &self.inner.env_file)
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(|error| {
                format!("无法连接媒体网关标准输出到日志：{error}")
            })?))
            .stderr(Stdio::from(log));
        hide_console(&mut command);

        let child = command.spawn().map_err(|error| {
            let message = format!("无法启动 media-gateway.exe：{error}");
            self.set_status("error", None, &message, None);
            message
        })?;
        let pid = child.id();
        if let Err(error) = fs::write(&self.inner.pid_file, pid.to_string()) {
            let mut child = child;
            terminate_process_tree(pid);
            let _ = child.kill();
            let _ = child.wait();
            let message = format!(
                "无法写入媒体网关 PID 文件 {}：{error}",
                self.inner.pid_file.display()
            );
            self.set_status("error", None, &message, None);
            return Err(message);
        }
        *self
            .inner
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(child);
        self.set_status("running", Some(pid), "媒体网关正在运行", Some(0));
        Ok(())
    }

    fn refresh_existing_process(&self) {
        if !self.inner.installed {
            return;
        }
        if let Some(pid) =
            active_external_process(&self.inner.pid_file, &self.inner.executable_path)
        {
            self.set_status("running", Some(pid), "媒体网关已在运行", None);
        }
    }

    fn refresh_health(&self) {
        let phase = {
            let status = self
                .inner
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            status.phase.clone()
        };
        let health = if phase == "running" {
            let url = resolve_gateway_url(&self.inner.env_file, &self.inner.desktop_dir);
            fetch_gateway_health(&url)
        } else {
            None
        };
        let mut status = self
            .inner
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        status.health = health;
    }

    fn schedule_restart(&self, exit_code: Option<i32>) {
        let attempts = {
            let mut status = self
                .inner
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            status.restart_attempts = status.restart_attempts.saturating_add(1);
            status.restart_attempts
        };
        let delay = restart_delay(attempts);
        *self
            .inner
            .next_restart
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some(Instant::now() + Duration::from_secs(delay));
        self.set_status(
            "error",
            None,
            &format!(
                "{}；{} 秒后自动恢复",
                gateway_exit_message(exit_code),
                delay
            ),
            Some(attempts),
        );
    }

    fn schedule_restart_after_error(&self, error: &str) {
        let attempts = {
            let mut status = self
                .inner
                .status
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            status.restart_attempts = status.restart_attempts.saturating_add(1);
            status.restart_attempts
        };
        let delay = restart_delay(attempts);
        *self
            .inner
            .next_restart
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some(Instant::now() + Duration::from_secs(delay));
        self.set_status(
            "error",
            None,
            &format!("{error}；{delay} 秒后重试"),
            Some(attempts),
        );
    }

    fn set_status(
        &self,
        phase: &str,
        pid: Option<u32>,
        message: &str,
        restart_attempts: Option<u32>,
    ) {
        let mut status = self
            .inner
            .status
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        status.phase = phase.to_string();
        status.pid = pid;
        status.message = message.to_string();
        if let Some(attempts) = restart_attempts {
            status.restart_attempts = attempts;
        }
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

    for ancestor in executable_dir.ancestors() {
        for profile in ["release", "debug"] {
            let candidate = ancestor
                .join("backend")
                .join("target")
                .join(profile)
                .join("zhiwei-service.exe");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    adjacent
}

fn resolve_gateway(executable_dir: &Path) -> PathBuf {
    if let Some(configured) = env::var_os("ZHIWEI_MEDIA_GATEWAY") {
        if !configured.is_empty() {
            return absolutize_from(executable_dir, Path::new(&configured));
        }
    }

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
        let adjacent = working_dir.join(".env");
        if adjacent.is_file() {
            return adjacent;
        }
    }
    if working_dir
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("dist"))
    {
        if let Some(parent) = working_dir.parent() {
            let development_env = parent.join(".env");
            if development_env.is_file() {
                return development_env;
            }
        }
    }
    packaged
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
    read_env_map(&desktop_dir.join(".env"))
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

fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn remove_pid_file(path: &Path) {
    let _ = fs::remove_file(path);
}

fn active_external_process(pid_file: &Path, expected_program: &Path) -> Option<u32> {
    let pid = read_pid(pid_file)?;
    if process_matches(pid, expected_program) {
        Some(pid)
    } else {
        remove_pid_file(pid_file);
        None
    }
}

fn process_matches(pid: u32, expected_program: &Path) -> bool {
    process_image_path(pid).is_some_and(|path| paths_equal(&path, expected_program))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn normalize_path(path: &Path) -> String {
    let path = fs::canonicalize(path).unwrap_or_else(|_| absolutize(path));
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn restart_delay(attempts: u32) -> u64 {
    match attempts {
        0 | 1 => 5,
        2 => 15,
        3 => 30,
        _ => 60,
    }
}

fn exit_message(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("通道守护意外退出（代码 {code}）"),
        None => "通道守护意外退出".to_string(),
    }
}

fn gateway_exit_message(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("媒体网关意外退出（代码 {code}）"),
        None => "媒体网关意外退出".to_string(),
    }
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(windows)]
fn process_image_path(pid: u32) -> Option<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }

    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length) };
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 {
        return None;
    }

    buffer.truncate(length as usize);
    Some(PathBuf::from(OsString::from_wide(&buffer)))
}

#[cfg(not(windows))]
fn process_image_path(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(windows)]
fn terminate_process(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        return false;
    }
    let result = unsafe { TerminateProcess(handle, 1) };
    unsafe {
        CloseHandle(handle);
    }
    result != 0
}

#[cfg(not(windows))]
fn terminate_process(_pid: u32) -> bool {
    false
}

fn terminate_process_tree(pid: u32) -> bool {
    let mut command = Command::new("taskkill.exe");
    command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    command
        .status()
        .map(|status| status.success())
        .unwrap_or_else(|_| terminate_process(pid))
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
    open_directory(&manager.inner.working_dir)
}

#[tauri::command]
fn open_env_file_command(manager: State<'_, ServiceManager>) -> Result<(), String> {
    open_env_file(&manager.inner.env_file)
}

#[tauri::command]
fn open_log_file(manager: State<'_, ServiceManager>) -> Result<(), String> {
    reveal_file(&manager.inner.log_file)
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
        env_file: manager.inner.env_file.display().to_string(),
        values: read_env_map(&manager.inner.env_file),
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
    open_directory(&manager.inner.working_dir)
}

#[tauri::command]
fn open_gateway_env_file(manager: State<'_, GatewayManager>) -> Result<(), String> {
    if !manager.inner.env_file.is_file() {
        let defaults = gateway_setting_schema()
            .into_iter()
            .map(|item| (item.key, item.default))
            .collect::<BTreeMap<_, _>>();
        write_gateway_env(&manager.inner.env_file, &defaults)?;
    }
    open_env_file(&manager.inner.env_file)
}

#[tauri::command]
fn open_gateway_log_file(manager: State<'_, GatewayManager>) -> Result<(), String> {
    reveal_file(&manager.inner.log_file)
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
    write_gateway_env(&manager.inner.env_file, &normalized)?;

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
        &fs::read_to_string(&manager.inner.control_state_file)
            .map_err(|error| format!("控制接口尚未就绪：{error}"))?,
    )
    .map_err(|error| format!("控制状态文件无效：{error}"))?;
    let token = fs::read_to_string(&manager.inner.control_token_file)
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
    let address = base
        .strip_prefix("http://")
        .ok_or_else(|| "控制接口只支持 loopback HTTP".to_string())?;
    let mut stream = TcpStream::connect(address)
        .map_err(|error| format!("无法连接控制接口 {address}：{error}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
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
                let _ = reveal_file(&manager.inner.log_file);
            }
            "gateway_logs" => {
                let manager = app.state::<GatewayManager>();
                let _ = reveal_file(&manager.inner.log_file);
            }
            "gateway_env" => {
                let manager = app.state::<GatewayManager>();
                let _ = open_env_file(&manager.inner.env_file);
            }
            "folder" => {
                let manager = app.state::<ServiceManager>();
                let _ = open_directory(&manager.inner.working_dir);
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
            let gateway_installed = gateway.inner.installed;
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

            let _ = manager.start();
            if gateway_installed {
                let _ = gateway.start();
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
            update_settings
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Zhiwei desktop");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_backoff_is_bounded() {
        assert_eq!(restart_delay(0), 5);
        assert_eq!(restart_delay(1), 5);
        assert_eq!(restart_delay(2), 15);
        assert_eq!(restart_delay(3), 30);
        assert_eq!(restart_delay(99), 60);
    }

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
}
