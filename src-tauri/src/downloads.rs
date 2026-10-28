use crate::database::Database;
use crate::models::{AppSettings, DownloadTask, Platform};
use crate::yt_dlp::{
    archive_path, base_command, download_output_dir, embed_args, final_output_path,
    map_download_error, parse_progress, split_format_spec, start_error, subtitle_args,
};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tauri::Emitter;
use uuid::Uuid;
#[derive(Default)]
struct ProcessState {
    running: HashMap<String, Arc<Mutex<Child>>>,
    reserved: HashSet<String>,
    native: HashMap<String, Arc<AtomicBool>>,
    cancelled: HashSet<String>,
}

impl ProcessState {
    fn active_count(&self) -> usize {
        self.running.len() + self.native.len() + self.reserved.len()
    }
}

#[derive(Clone, Default)]
pub struct ProcessRegistry(Arc<Mutex<ProcessState>>);

/// 入队/任务结束等"可能腾出槽位"的事件通知，唤醒调度线程
#[derive(Clone)]
pub struct Notify(pub std::sync::mpsc::Sender<()>);

pub struct DownloadRequest {
    pub source_url: String,
    pub title: String,
    pub format_id: String,
    pub platform: Platform,
    pub batch_id: Option<String>,
    /// 批次展示名，同时作为已消毒的输出子文件夹名
    pub batch_name: Option<String>,
}

/// 启动调度线程：任何事件（或 1 秒超时兜底）都会尝试把排队任务拉起到并发上限
pub fn start_dispatcher(
    app: tauri::AppHandle,
    database: Database,
    registry: ProcessRegistry,
) -> Notify {
    let (tx, rx) = std::sync::mpsc::channel();
    let notify = Notify(tx);
    let dispatcher_notify = notify.clone();
    std::thread::spawn(move || loop {
        let _ = rx.recv_timeout(std::time::Duration::from_secs(1));
        dispatch_once(&app, &database, &registry, &dispatcher_notify);
    });
    notify
}

pub fn dispatch_once(
    app: &tauri::AppHandle,
    database: &Database,
    registry: &ProcessRegistry,
    notify: &Notify,
) {
    let settings = match crate::settings::load(app) {
        Ok(settings) => settings,
        Err(_) => return,
    };
    let capacity = usize::from(settings.concurrent_downloads).max(1);
    loop {
        let queued = match database.list_queued(32) {
            Ok(value) => value,
            Err(_) => return,
        };
        let next = {
            let Ok(mut state) = registry.0.lock() else {
                return;
            };
            if state.active_count() >= capacity {
                return;
            }
            let Some(candidate) = queued
                .iter()
                .find(|(task, _)| !state.reserved.contains(&task.id))
                .cloned()
            else {
                return;
            };
            state.reserved.insert(candidate.0.id.clone());
            candidate
        };
        let (task, format_id) = next;
        let snapshot = task.clone();
        if let Err(error) = launch(app, database, registry, notify, task, format_id, &settings) {
            // launch 失败说明任务自身有问题（Cookie 缺失、链接失效等），
            // 释放槽位并把原因落库，继续尝试队列中的下一个
            if let Ok(mut state) = registry.0.lock() {
                state.reserved.remove(&snapshot.id);
            }
            let mut failed = snapshot;
            failed.status = "failed".into();
            failed.error = Some(error);
            let _ = database.update_progress(
                &failed.id,
                "failed",
                0.0,
                None,
                None,
                failed.error.as_deref(),
            );
            let _ = app.emit("task-progress", &failed);
        }
    }
}

/// 任务入队：只写库和事件，不直接启动；由调度器按并发上限拉起
pub fn enqueue(
    app: &tauri::AppHandle,
    database: &Database,
    notify: &Notify,
    request: DownloadRequest,
) -> Result<DownloadTask, String> {
    let media_details = crate::hongguo::task_details(&request.source_url, &request.format_id);
    let task = DownloadTask {
        id: Uuid::new_v4().to_string(),
        url: request.source_url,
        title: request.title,
        platform: request.platform,
        status: "queued".into(),
        progress: 0.0,
        speed: None,
        eta: None,
        output_path: None,
        error: None,
        batch_id: request.batch_id,
        batch_name: request.batch_name,
        media_details,
    };
    database.insert(&task, &request.format_id)?;
    let _ = app.emit("task-progress", &task);
    let _ = notify.0.send(());
    Ok(task)
}

/// 批量任务已经由调用方完整预校验；这里用一个事务入库，避免半批成功。
pub fn enqueue_many(
    app: &tauri::AppHandle,
    database: &Database,
    notify: &Notify,
    requests: Vec<DownloadRequest>,
) -> Result<Vec<DownloadTask>, String> {
    let tasks_with_formats: Vec<(DownloadTask, String)> = requests
        .into_iter()
        .map(|request| {
            let media_details =
                crate::hongguo::task_details(&request.source_url, &request.format_id);
            (
                DownloadTask {
                    id: Uuid::new_v4().to_string(),
                    url: request.source_url,
                    title: request.title,
                    platform: request.platform,
                    status: "queued".into(),
                    progress: 0.0,
                    speed: None,
                    eta: None,
                    output_path: None,
                    error: None,
                    batch_id: request.batch_id,
                    batch_name: request.batch_name,
                    media_details,
                },
                request.format_id,
            )
        })
        .collect();
    database.insert_many(&tasks_with_formats)?;
    let tasks: Vec<DownloadTask> = tasks_with_formats
        .into_iter()
        .map(|(task, _)| task)
        .collect();
    for task in &tasks {
        let _ = app.emit("task-progress", task);
    }
    let _ = notify.0.send(());
    Ok(tasks)
}

fn launch(
    app: &tauri::AppHandle,
    database: &Database,
    registry: &ProcessRegistry,
    notify: &Notify,
    task: DownloadTask,
    format_id: String,
    settings: &AppSettings,
) -> Result<(), String> {
    if matches!(task.platform, Platform::Hongguo) {
        return launch_hongguo(app, database, registry, notify, task, format_id, settings);
    }
    let mut task = task;
    // 入队后到拉起前可能已被取消
    if let Ok((current, _)) = database.task_full(&task.id) {
        if current.status != "queued" {
            if let Ok(mut state) = registry.0.lock() {
                state.reserved.remove(&task.id);
            }
            return Ok(());
        }
    }
    let platform = crate::platform::detect(&task.url)?;
    // 链接解析（抖音短链归一化、t.co 复验）放在拉起时执行，重试天然重新解析
    let download_url = crate::resolver::execution_url(app, &task.url, platform, settings)?;
    let (format_expr, extra_args) = split_format_spec(&format_id);
    let mut command = base_command(app, platform, settings)?;
    let output_dir = download_output_dir(app, settings, task.batch_name.as_deref())?;
    std::fs::create_dir_all(&output_dir).map_err(|error| format!("无法创建下载目录：{error}"))?;
    let output_template = output_dir
        .join(&settings.filename_template)
        .to_string_lossy()
        .to_string();
    command.args([
        "--newline",
        "--progress",
        "--no-playlist",
        "--print",
        "after_move:filepath",
        "--format",
        &format_expr,
        "--output",
        &output_template,
    ]);
    if extra_args.is_empty() {
        command.args(["--merge-output-format", "mp4"]);
    } else {
        command.args(&extra_args);
    }
    command.args(subtitle_args(platform, settings));
    command.args(embed_args(&format_id, settings));
    // 下载归档仅作用于批量场景：单视频手动下载是明确意图，永不跳过（需求 v4 F2/D2）
    if settings.download_archive && task.batch_id.is_some() {
        if let Ok(archive) = archive_path(app) {
            command.args(["--download-archive", &archive.to_string_lossy()]);
        }
    }
    command.args(["--", &download_url]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 独立进程组，取消时可连同 ffmpeg 等子进程一并终止
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(start_error)?;
    // spawn 后再核对一次取消状态，关闭"取消→仍被拉起"的窗口
    if let Ok((current, _)) = database.task_full(&task.id) {
        if current.status != "queued" {
            let _ = child.kill();
            if let Ok(mut state) = registry.0.lock() {
                state.reserved.remove(&task.id);
            }
            return Ok(());
        }
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取下载进度".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取下载错误信息".to_string())?;
    task.status = "downloading".into();
    task.progress = 0.0;
    task.speed = None;
    task.eta = None;
    let _ = database.update_progress(&task.id, "downloading", 0.0, None, None, None);
    let _ = app.emit("task-progress", &task);
    let child = Arc::new(Mutex::new(child));
    let registered = registry.0.lock().map(|mut state| {
        state.reserved.remove(&task.id);
        state.running.insert(task.id.clone(), child.clone());
        true
    });
    if registered.is_err() {
        let _ = child.lock().map(|mut process| process.kill());
        return Err("任务注册表不可用".into());
    }
    monitor(
        app.clone(),
        database.clone(),
        registry.clone(),
        notify.clone(),
        task,
        child,
        stdout,
        stderr,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn monitor(
    app: tauri::AppHandle,
    database: Database,
    registry: ProcessRegistry,
    notify: Notify,
    mut task: DownloadTask,
    child: Arc<Mutex<Child>>,
    stdout: impl std::io::Read + Send + 'static,
    stderr: impl std::io::Read + Send + 'static,
) {
    std::thread::spawn(move || {
        let progress = Regex::new(r"\[download\]\s+([0-9.]+)%.*?at\s+([^\s]+).*?ETA\s+([^\s]+)")
            .expect("valid progress regex");
        let (sender, receiver) = std::sync::mpsc::channel();
        let stdout_sender = sender.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = stdout_sender.send((false, line));
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = sender.send((true, line));
            }
        });
        let mut stderr_lines = Vec::new();
        for (is_stderr, line) in receiver {
            if is_stderr {
                if stderr_lines.len() == 100 {
                    stderr_lines.remove(0);
                }
                stderr_lines.push(line.clone());
            }
            if let Some((value, speed, eta)) = parse_progress(&progress, &line) {
                task.progress = value;
                task.speed = Some(speed);
                task.eta = Some(eta);
                let _ = database.update_progress(
                    &task.id,
                    "downloading",
                    task.progress,
                    task.speed.as_deref(),
                    task.eta.as_deref(),
                    None,
                );
                let _ = app.emit("task-progress", &task);
            } else if line.contains("[Merger]") || line.contains("[ExtractAudio]") {
                task.status = "processing".into();
                let _ = database.update_progress(
                    &task.id,
                    "processing",
                    task.progress,
                    task.speed.as_deref(),
                    task.eta.as_deref(),
                    None,
                );
                let _ = app.emit("task-progress", &task);
            } else if !is_stderr {
                // --print after_move:filepath 输出的最终文件路径
                if let Some(path) = final_output_path(&line) {
                    let _ = database.set_output_path(&task.id, &path);
                    task.output_path = Some(path);
                }
            }
        }
        let exit = child
            .lock()
            .ok()
            .and_then(|mut process| process.wait().ok());
        let was_cancelled = registry.0.lock().ok().is_some_and(|mut state| {
            state.running.remove(&task.id);
            state.cancelled.remove(&task.id)
        });
        if was_cancelled {
            task.status = "cancelled".into();
            task.speed = None;
            task.eta = None;
            task.error = None;
            let _ =
                database.update_progress(&task.id, "cancelled", task.progress, None, None, None);
            let _ = app.emit("task-progress", &task);
        } else if exit.is_some_and(|status| status.success()) {
            task.status = "completed".into();
            task.progress = 100.0;
            task.speed = None;
            task.eta = None;
            let _ = database.update_progress(&task.id, "completed", 100.0, None, None, None);
            let _ = app.emit("task-progress", &task);
        } else {
            task.status = "failed".into();
            task.error = Some(map_download_error(&stderr_lines.join("\n"), task.platform));
            let _ = database.update_progress(
                &task.id,
                "failed",
                task.progress,
                None,
                None,
                task.error.as_deref(),
            );
            let _ = app.emit("task-progress", &task);
        }
        // 终态落定，唤醒调度器填补空出的槽位
        let _ = notify.0.send(());
    });
}

pub fn cancel(
    app: &tauri::AppHandle,
    database: &Database,
    registry: &ProcessRegistry,
    id: &str,
) -> Result<(), String> {
    let running = {
        let mut state = registry
            .0
            .lock()
            .map_err(|_| "任务注册表不可用".to_string())?;
        if let Some(cancelled) = state.native.get(id) {
            cancelled.store(true, Ordering::SeqCst);
            return Ok(());
        }
        if let Some(process) = state.running.get(id).cloned() {
            state.cancelled.insert(id.to_string());
            Some(process)
        } else {
            None
        }
    };
    if let Some(process) = running {
        let mut child = process.lock().map_err(|_| "无法访问下载进程".to_string())?;
        #[cfg(unix)]
        unsafe {
            // 先向整个进程组发 SIGKILL，连带终止 yt-dlp 派生的 ffmpeg 等子进程
            libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
        child
            .kill()
            .map_err(|error| format!("无法取消任务：{error}"))?;
        // 最终状态由 monitor 线程在进程退出后统一写入
        return Ok(());
    }
    // 不在运行：排队中的任务直接取消（launch 的双重核对保证不会被再拉起）
    if let Ok((mut task, _)) = database.task_full(id) {
        if task.status == "queued" {
            database.mark_cancelled(id)?;
            task.status = "cancelled".into();
            task.speed = None;
            task.eta = None;
            let _ = app.emit("task-progress", &task);
            return Ok(());
        }
    }
    Err("任务没有在运行".into())
}

pub fn cancel_batch(
    app: &tauri::AppHandle,
    database: &Database,
    registry: &ProcessRegistry,
    batch_id: &str,
) -> Result<(), String> {
    let tasks = database.list_by_batch(batch_id)?;
    for task in tasks.iter().filter(|task| {
        matches!(
            task.status.as_str(),
            "queued" | "downloading" | "processing"
        )
    }) {
        let _ = cancel(app, database, registry, &task.id);
    }
    Ok(())
}

fn launch_hongguo(
    app: &tauri::AppHandle,
    database: &Database,
    registry: &ProcessRegistry,
    notify: &Notify,
    mut task: DownloadTask,
    quality: String,
    settings: &AppSettings,
) -> Result<(), String> {
    crate::hongguo::validate_episode(&task.url, &quality)?;
    let directory = download_output_dir(app, settings, task.batch_name.as_deref())?;
    let proxy = crate::hongguo::proxy(settings)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut state = registry.0.lock().map_err(|_| "任务注册表不可用")?;
        // The same lock guards cancellation before and after native registration.
        if database.task_full(&task.id)?.0.status != "queued" {
            state.reserved.remove(&task.id);
            return Ok(());
        }
        state.reserved.remove(&task.id);
        state.native.insert(task.id.clone(), cancelled.clone());
    }
    let app = app.clone();
    let db = database.clone();
    let registry = registry.clone();
    let notify = notify.clone();
    std::thread::spawn(move || {
        task.status = "downloading".into();
        let _ = db.update_progress(&task.id, &task.status, 0.0, None, None, None);
        let _ = app.emit("task-progress", &task);
        let id = task.id.clone();
        let result = crate::hongguo::request(
            &id,
            "download",
            serde_json::json!({
                "url":task.url, "quality":quality, "directory":directory, "proxy":proxy
            }),
            &cancelled,
            |event| {
                task.status = if event["type"] == "processing" {
                    "processing"
                } else {
                    "downloading"
                }
                .into();
                task.progress = event["progress"]
                    .as_f64()
                    .unwrap_or(task.progress)
                    .clamp(0.0, 99.0);
                let _ = db.update_progress(&id, &task.status, task.progress, None, None, None);
                let _ = app.emit("task-progress", &task);
            },
        );
        // A published, verified result wins a late cancellation; never orphan a successful output.
        match result {
            Ok(details) => {
                task.output_path = details["path"].as_str().map(str::to_string);
                task.media_details = Some(details.clone());
                let saved = task
                    .output_path
                    .as_deref()
                    .ok_or("红果引擎没有返回文件路径".to_string())
                    .and_then(|path| db.set_output_path(&id, path))
                    .and_then(|_| db.set_media_details(&id, &details));
                match saved {
                    Ok(()) => {
                        task.status = "completed".into();
                        task.progress = 100.0;
                    }
                    Err(error) => {
                        task.status = "failed".into();
                        task.error = Some(error);
                    }
                }
            }
            Err(error) if cancelled.load(Ordering::SeqCst) => {
                task.status = "cancelled".into();
                let _ = error;
            }
            Err(error) => {
                task.status = "failed".into();
                task.error = Some(error);
            }
        }
        let _ = db.update_progress(
            &id,
            &task.status,
            task.progress,
            None,
            None,
            task.error.as_deref(),
        );
        let _ = app.emit("task-progress", &task);
        if let Ok(mut state) = registry.0.lock() {
            state.native.remove(&id);
        }
        let _ = notify.0.send(());
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_tasks_and_reserved_tasks_share_capacity() {
        let mut state = ProcessState::default();
        state.reserved.insert("yt-starting".into());
        state
            .native
            .insert("hongguo-running".into(), Arc::new(AtomicBool::new(false)));
        assert_eq!(state.active_count(), 2);
        state.native["hongguo-running"].store(true, Ordering::SeqCst);
        assert_eq!(
            state.active_count(),
            2,
            "cancellation must retain slot until cleanup finishes"
        );
        state.native.remove("hongguo-running");
        assert_eq!(state.active_count(), 1);
    }
}
