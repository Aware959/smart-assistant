//! 通用子进程监管：启动 / 停止 / 重启、退出检测、退避重试与 PID 文件归属判断。
//! `zhiwei-service` 与媒体网关共用这一套状态机；差异（程序形态、终止方式、
//! 提示文案）全部收在 [`ProcessPlan`] 里。

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct SupervisorSnapshot {
    pub phase: String,
    pub pid: Option<u32>,
    pub message: String,
    pub auto_restart: bool,
    pub restart_attempts: u32,
}

pub enum CommandPlan {
    Exe(PathBuf),
    Interpreter { program: PathBuf, entry: PathBuf },
}

impl CommandPlan {
    pub fn entry(&self) -> &Path {
        match self {
            CommandPlan::Exe(path) => path,
            CommandPlan::Interpreter { entry, .. } => entry,
        }
    }
}

pub enum ImageMatcher {
    Path(PathBuf),
    Interpreter(PathBuf),
}

pub struct ProcessPlan {
    pub name: &'static str,
    pub program_label: &'static str,
    pub log_header: &'static str,
    pub log_file: PathBuf,
    pub pid_file: PathBuf,
    pub working_dir: PathBuf,
    pub command: CommandPlan,
    pub image_matcher: ImageMatcher,
    pub env: Vec<(String, String)>,
    pub terminate_tree: bool,
    pub spawn_error_hint: &'static str,
}

pub trait SupervisedProcess: Send {
    fn pid(&self) -> u32;
    fn try_wait(&mut self) -> std::io::Result<Option<i32>>;
    fn kill(&mut self);
}

impl SupervisedProcess for Child {
    fn pid(&self) -> u32 {
        self.id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
        Child::try_wait(self).map(|status| status.and_then(|status| status.code()))
    }

    fn kill(&mut self) {
        let _ = Child::kill(self);
        let _ = Child::wait(self);
    }
}

pub type Launcher =
    Box<dyn Fn(&ProcessPlan) -> Result<Box<dyn SupervisedProcess>, String> + Send + Sync>;

struct SupervisorState {
    phase: String,
    pid: Option<u32>,
    message: String,
    auto_restart: bool,
    restart_attempts: u32,
    next_restart: Option<Instant>,
}

pub struct ProcessSupervisor {
    plan: ProcessPlan,
    launcher: Launcher,
    state: Mutex<SupervisorState>,
    child: Mutex<Option<Box<dyn SupervisedProcess>>>,
}

impl ProcessSupervisor {
    pub fn new(
        plan: ProcessPlan,
        launcher: Launcher,
        auto_restart: bool,
        phase: &str,
        message: String,
    ) -> Self {
        Self {
            plan,
            launcher,
            state: Mutex::new(SupervisorState {
                phase: phase.to_string(),
                pid: None,
                message,
                auto_restart,
                restart_attempts: 0,
                next_restart: None,
            }),
            child: Mutex::new(None),
        }
    }

    pub fn plan(&self) -> &ProcessPlan {
        &self.plan
    }

    pub fn snapshot(&self) -> SupervisorSnapshot {
        let state = self.lock_state();
        SupervisorSnapshot {
            phase: state.phase.clone(),
            pid: state.pid,
            message: state.message.clone(),
            auto_restart: state.auto_restart,
            restart_attempts: state.restart_attempts,
        }
    }

    pub fn start(&self) -> Result<SupervisorSnapshot, String> {
        {
            let mut state = self.lock_state();
            state.auto_restart = true;
            state.next_restart = None;
            state.restart_attempts = 0;
        }
        self.start_process()?;
        Ok(self.snapshot())
    }

    pub fn stop(&self) -> Result<SupervisorSnapshot, String> {
        {
            let mut state = self.lock_state();
            state.auto_restart = false;
            state.next_restart = None;
        }
        self.set_status(
            "starting",
            None,
            &format!("正在停止{}……", self.plan.name),
            None,
        );

        let child = self
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(mut process) = child {
            let pid = process.pid();
            if self.plan.terminate_tree {
                terminate_process_tree(pid);
            }
            process.kill();
        } else if let Some(pid) = self.external_pid(&self.plan.pid_file) {
            if self.plan.terminate_tree {
                terminate_process_tree(pid);
            } else {
                terminate_process(pid);
            }
        }

        remove_pid_file(&self.plan.pid_file);
        self.set_status(
            "stopped",
            None,
            &format!("{}已停止", self.plan.name),
            Some(0),
        );
        Ok(self.snapshot())
    }

    pub fn restart(&self) -> Result<SupervisorSnapshot, String> {
        self.stop()?;
        thread::sleep(Duration::from_millis(350));
        self.start()
    }

    pub fn tick(&self) -> SupervisorSnapshot {
        let exited = {
            let mut child = self.child.lock().unwrap_or_else(|error| error.into_inner());
            match child.as_mut() {
                Some(process) => match process.try_wait() {
                    Ok(Some(exit_code)) => {
                        *child = None;
                        Some(exit_code)
                    }
                    Ok(None) => None,
                    Err(error) => {
                        *child = None;
                        self.set_status(
                            "error",
                            None,
                            &format!("无法读取{}状态：{error}", self.plan.name),
                            None,
                        );
                        None
                    }
                },
                None => None,
            }
        };

        if let Some(exit_code) = exited {
            remove_pid_file(&self.plan.pid_file);
            if self.lock_state().auto_restart {
                self.schedule_restart(Some(exit_code));
            } else {
                let message = self.exit_message(Some(exit_code));
                self.set_status("stopped", None, &message, Some(0));
            }
        } else if self
            .child
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_none()
        {
            if let Some(pid) = self.external_pid(&self.plan.pid_file) {
                let state = self.lock_state();
                if state.pid != Some(pid) || state.phase != "running" {
                    drop(state);
                    self.set_status(
                        "running",
                        Some(pid),
                        &format!("{}正在运行（外部进程）", self.plan.name),
                        None,
                    );
                }
            } else if self.lock_state().auto_restart {
                let was_running = self.lock_state().phase == "running";
                if was_running {
                    self.schedule_restart(None);
                }
            }
        }

        let restart_due = self
            .lock_state()
            .next_restart
            .is_some_and(|deadline| Instant::now() >= deadline);
        if restart_due && self.lock_state().auto_restart {
            self.lock_state().next_restart = None;
            if let Err(error) = self.start_process() {
                self.schedule_restart_after_error(&error);
            }
        }

        self.snapshot()
    }

    pub fn program_exists(&self) -> bool {
        self.plan.command.entry().is_file()
    }

    pub fn mark_error(&self, message: &str) {
        self.set_status("error", None, message, None);
    }

    pub fn mark_unavailable(&self, message: &str) {
        self.set_status("unavailable", None, message, None);
    }

    pub fn reset_stopped(&self, message: &str) {
        self.set_status("stopped", None, message, Some(0));
    }

    pub fn refresh_external(&self) -> bool {
        self.external_pid(&self.plan.pid_file)
            .map(|pid| {
                self.set_status(
                    "running",
                    Some(pid),
                    &format!("{}已在运行", self.plan.name),
                    None,
                );
                pid
            })
            .is_some()
    }

    fn start_process(&self) -> Result<(), String> {
        if read_pid(&self.plan.pid_file).is_some_and(|pid| self.matches_pid(pid)) {
            let pid = read_pid(&self.plan.pid_file);
            self.set_status("running", pid, &format!("{}已在运行", self.plan.name), None);
            return Ok(());
        }

        {
            let mut child = self.child.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(process) = child.as_mut() {
                match process.try_wait() {
                    Ok(None) => return Ok(()),
                    Ok(Some(_)) => *child = None,
                    Err(error) => {
                        return Err(format!("无法读取现有{}状态：{error}", self.plan.name));
                    }
                }
            }
        }

        if !self.program_exists() {
            let message = format!(
                "未找到{}：{}",
                self.plan.program_label,
                self.plan.command.entry().display()
            );
            self.set_status("error", None, &message, None);
            return Err(message);
        }

        self.set_status(
            "starting",
            None,
            &format!("正在启动{}……", self.plan.name),
            None,
        );
        let mut process = (self.launcher)(&self.plan).map_err(|message| {
            self.set_status("error", None, &message, None);
            message
        })?;
        let pid = process.pid();
        if let Err(error) = fs::write(&self.plan.pid_file, pid.to_string()) {
            if self.plan.terminate_tree {
                terminate_process_tree(pid);
            }
            process.kill();
            let message = format!(
                "无法写入 PID 文件 {}：{error}",
                self.plan.pid_file.display()
            );
            self.set_status("error", None, &message, None);
            return Err(message);
        }
        *self.child.lock().unwrap_or_else(|error| error.into_inner()) = Some(process);
        self.set_status(
            "running",
            Some(pid),
            &format!("{}正在运行", self.plan.name),
            Some(0),
        );
        Ok(())
    }

    fn external_pid(&self, pid_file: &Path) -> Option<u32> {
        let pid = read_pid(pid_file).filter(|pid| self.matches_pid(*pid));
        if pid.is_none() {
            remove_pid_file(pid_file);
        }
        pid
    }

    fn matches_pid(&self, pid: u32) -> bool {
        pid_matches(&self.plan.image_matcher, pid)
    }

    fn schedule_restart(&self, exit_code: Option<i32>) {
        let attempts = self.bump_restart_attempts();
        let delay = restart_delay(attempts);
        self.lock_state().next_restart = Some(Instant::now() + Duration::from_secs(delay));
        let message = format!("{}；{delay} 秒后自动恢复", self.exit_message(exit_code));
        self.set_status("error", None, &message, Some(attempts));
    }

    fn schedule_restart_after_error(&self, error: &str) {
        let attempts = self.bump_restart_attempts();
        let delay = restart_delay(attempts);
        self.lock_state().next_restart = Some(Instant::now() + Duration::from_secs(delay));
        let message = format!("{error}；{delay} 秒后重试");
        self.set_status("error", None, &message, Some(attempts));
    }

    fn bump_restart_attempts(&self) -> u32 {
        let mut state = self.lock_state();
        state.restart_attempts = state.restart_attempts.saturating_add(1);
        state.restart_attempts
    }

    fn exit_message(&self, exit_code: Option<i32>) -> String {
        match exit_code {
            Some(code) => format!("{}意外退出（代码 {code}）", self.plan.name),
            None => format!("{}意外退出", self.plan.name),
        }
    }

    fn set_status(
        &self,
        phase: &str,
        pid: Option<u32>,
        message: &str,
        restart_attempts: Option<u32>,
    ) {
        let mut state = self.lock_state();
        state.phase = phase.to_string();
        state.pid = pid;
        state.message = message.to_string();
        if let Some(attempts) = restart_attempts {
            state.restart_attempts = attempts;
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SupervisorState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    #[cfg(test)]
    fn force_restart_deadline(&self) {
        self.lock_state().next_restart = Some(Instant::now());
    }
}

pub fn spawn_process(plan: &ProcessPlan) -> Result<Box<dyn SupervisedProcess>, String> {
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&plan.log_file)
        .map_err(|error| format!("无法打开日志文件 {}：{error}", plan.log_file.display()))?;
    let mut header = log
        .try_clone()
        .map_err(|error| format!("无法复制日志文件句柄：{error}"))?;
    let _ = writeln!(header, "\n{}", plan.log_header);

    let mut command = match &plan.command {
        CommandPlan::Exe(path) => {
            let mut command = Command::new(path);
            command.current_dir(&plan.working_dir);
            command
        }
        CommandPlan::Interpreter { program, entry } => {
            let mut command = Command::new(program);
            command.arg(entry).current_dir(&plan.working_dir);
            command
        }
    };
    for (key, value) in &plan.env {
        command.env(key, value);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|error| {
            format!("无法连接子进程标准输出到日志：{error}")
        })?))
        .stderr(Stdio::from(log));
    hide_console(&mut command);

    let child = command.spawn().map_err(|error| {
        format!(
            "无法启动{}：{error}{}",
            plan.program_label, plan.spawn_error_hint
        )
    })?;
    Ok(Box::new(child))
}

fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn remove_pid_file(path: &Path) {
    let _ = fs::remove_file(path);
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    normalize_path(left) == normalize_path(right)
}

pub(crate) fn normalize_path(path: &Path) -> String {
    let path = fs::canonicalize(path).unwrap_or_else(|_| absolutize(path));
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// 判断 pid 是否仍存活，且可执行文件符合 matcher。
///
/// 除了 pid 文件之外，桌面壳在开发态还会用它识别“先手动启动”的 service /
/// 网关，避免再拉起第二份。
pub fn pid_matches(matcher: &ImageMatcher, pid: u32) -> bool {
    let Some(image) = process_image_path(pid) else {
        return false;
    };
    match matcher {
        ImageMatcher::Path(expected) => paths_equal(&image, expected),
        ImageMatcher::Interpreter(python) => program_image_matches(&image, python),
    }
}

fn program_image_matches(image: &Path, interpreter: &Path) -> bool {
    if paths_equal(image, interpreter) {
        return true;
    }
    let file_name = |path: &Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
            .trim_end_matches(".exe")
            .to_string()
    };
    let expected = file_name(interpreter);
    !expected.is_empty() && file_name(image) == expected
}

fn restart_delay(attempts: u32) -> u64 {
    match attempts {
        0 | 1 => 5,
        2 => 15,
        3 => 30,
        _ => 60,
    }
}

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    command
        .status()
        .is_ok_and(|status| status.code() == Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;

    struct FakeProcess {
        pid: u32,
        exited: bool,
        exit_code: Option<i32>,
        killed: bool,
    }

    impl SupervisedProcess for FakeProcess {
        fn pid(&self) -> u32 {
            self.pid
        }

        fn try_wait(&mut self) -> std::io::Result<Option<i32>> {
            if self.exited {
                Ok(Some(self.exit_code.unwrap_or(0)))
            } else {
                Ok(None)
            }
        }

        fn kill(&mut self) {
            self.killed = true;
        }
    }

    #[derive(Default)]
    struct LauncherScript {
        launches: Mutex<VecDeque<Box<dyn SupervisedProcess>>>,
        calls: Mutex<u32>,
    }

    fn fake_supervisor(script: Arc<LauncherScript>, pid_file: &Path) -> ProcessSupervisor {
        let launcher_script = script.clone();
        ProcessSupervisor::new(
            test_plan(pid_file),
            Box::new(move |_plan| {
                *launcher_script.calls.lock().unwrap() += 1;
                launcher_script
                    .launches
                    .lock()
                    .unwrap()
                    .pop_front()
                    .map(Ok::<_, String>)
                    .unwrap_or_else(|| Err("无法启动测试进程".to_string()))
            }),
            true,
            "stopped",
            "测试进程尚未启动".to_string(),
        )
    }

    fn test_plan(pid_file: &Path) -> ProcessPlan {
        let program = pid_file.with_file_name("test-process.exe");
        fs::write(&program, b"test").unwrap();
        ProcessPlan {
            name: "测试进程",
            program_label: "test-process.exe",
            log_header: "--- test header ---",
            log_file: pid_file.with_file_name("test.log"),
            pid_file: pid_file.to_path_buf(),
            working_dir: pid_file.parent().unwrap().to_path_buf(),
            command: CommandPlan::Exe(program),
            image_matcher: ImageMatcher::Path(PathBuf::from("test-process.exe")),
            env: Vec::new(),
            terminate_tree: false,
            spawn_error_hint: "",
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zhiwei-supervisor-test-{tag}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_child(pid: u32) -> Box<dyn SupervisedProcess> {
        Box::new(FakeProcess {
            pid,
            exited: false,
            exit_code: None,
            killed: false,
        })
    }

    #[test]
    fn start_runs_process_and_writes_pid_file() {
        let dir = temp_dir("start");
        let pid_file = dir.join("start.pid");
        let script = Arc::new(LauncherScript::default());
        script.launches.lock().unwrap().push_back(fake_child(4242));
        let supervisor = fake_supervisor(script, &pid_file);

        let snapshot = supervisor.start().unwrap();
        assert_eq!(snapshot.phase, "running");
        assert_eq!(snapshot.pid, Some(4242));
        assert_eq!(fs::read_to_string(&pid_file).unwrap(), "4242");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stop_kills_child_and_clears_pid_file() {
        let dir = temp_dir("stop");
        let pid_file = dir.join("stop.pid");
        let script = Arc::new(LauncherScript::default());
        script.launches.lock().unwrap().push_back(fake_child(4243));
        let supervisor = fake_supervisor(script, &pid_file);
        supervisor.start().unwrap();

        let snapshot = supervisor.stop().unwrap();
        assert_eq!(snapshot.phase, "stopped");
        assert!(snapshot.message.contains("已停止"));
        assert!(!pid_file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tick_schedules_backoff_restart_after_exit_and_relaunches() {
        let dir = temp_dir("tick");
        let pid_file = dir.join("tick.pid");
        let script = Arc::new(LauncherScript::default());
        script
            .launches
            .lock()
            .unwrap()
            .push_back(Box::new(FakeProcess {
                pid: 4244,
                exited: true,
                exit_code: Some(3),
                killed: false,
            }));
        script.launches.lock().unwrap().push_back(fake_child(4245));
        let supervisor = fake_supervisor(script.clone(), &pid_file);
        supervisor.start().unwrap();

        let snapshot = supervisor.tick();
        assert_eq!(snapshot.phase, "error");
        assert_eq!(snapshot.restart_attempts, 1);
        assert!(snapshot.message.contains("意外退出（代码 3）"));
        assert_eq!(*script.calls.lock().unwrap(), 1);

        supervisor.force_restart_deadline();
        let snapshot = supervisor.tick();
        assert_eq!(snapshot.phase, "running");
        assert_eq!(snapshot.pid, Some(4245));
        assert_eq!(*script.calls.lock().unwrap(), 2);
        assert_eq!(fs::read_to_string(&pid_file).unwrap(), "4245");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stop_disables_auto_restart_after_exit() {
        let dir = temp_dir("no-restart");
        let pid_file = dir.join("no-restart.pid");
        let script = Arc::new(LauncherScript::default());
        script
            .launches
            .lock()
            .unwrap()
            .push_back(Box::new(FakeProcess {
                pid: 4246,
                exited: false,
                exit_code: None,
                killed: false,
            }));
        let supervisor = fake_supervisor(script.clone(), &pid_file);
        supervisor.start().unwrap();
        supervisor.stop().unwrap();
        assert_eq!(*script.calls.lock().unwrap(), 1);

        let snapshot = supervisor.tick();
        assert_eq!(snapshot.phase, "stopped");
        assert!(!snapshot.auto_restart);
        assert_eq!(*script.calls.lock().unwrap(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refresh_external_adopts_current_process_by_pid_file() {
        let dir = temp_dir("external");
        let pid_file = dir.join("external.pid");
        let own_pid = std::process::id();
        fs::write(&pid_file, own_pid.to_string()).unwrap();
        let mut plan = test_plan(&pid_file);
        plan.image_matcher = ImageMatcher::Path(std::env::current_exe().unwrap());

        let supervisor = ProcessSupervisor::new(
            plan,
            Box::new(|_| Err("不应拉起进程".to_string())),
            true,
            "stopped",
            "测试进程尚未启动".to_string(),
        );
        assert!(supervisor.refresh_external());
        let snapshot = supervisor.snapshot();
        assert_eq!(snapshot.phase, "running");
        assert_eq!(snapshot.pid, Some(own_pid));
        assert!(snapshot.message.contains("已在运行"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_pid_file_is_removed_on_refresh() {
        let dir = temp_dir("stale");
        let pid_file = dir.join("stale.pid");
        fs::write(&pid_file, "499999999").unwrap();
        let script = Arc::new(LauncherScript::default());
        let supervisor = fake_supervisor(script, &pid_file);

        assert!(!supervisor.refresh_external());
        assert!(!pid_file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn restart_backoff_is_bounded() {
        assert_eq!(restart_delay(0), 5);
        assert_eq!(restart_delay(1), 5);
        assert_eq!(restart_delay(2), 15);
        assert_eq!(restart_delay(3), 30);
        assert_eq!(restart_delay(99), 60);
    }

    #[test]
    fn interpreter_image_matches_by_file_name() {
        assert!(program_image_matches(
            Path::new(r"C:\Python312\python.EXE"),
            Path::new("python")
        ));
        assert!(program_image_matches(
            Path::new(r"C:\venv\Scripts\python.exe"),
            Path::new(r"C:\venv\Scripts\python.exe")
        ));
        assert!(!program_image_matches(
            Path::new(r"C:\Windows\System32\cmd.exe"),
            Path::new("python")
        ));
    }

    #[test]
    fn read_pid_parses_and_rejects_garbage() {
        let dir = temp_dir("pid");
        let pid_file = dir.join("pid.txt");
        fs::write(&pid_file, " 123\n").unwrap();
        assert_eq!(read_pid(&pid_file), Some(123));
        fs::write(&pid_file, "not-a-pid").unwrap();
        assert_eq!(read_pid(&pid_file), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
