use crate::database::Database;
use crate::models::{
    AppSettings, DownloadTask, ListEntry, ListInfo, MediaFormat, MediaInfo, Platform, ProxyMode,
};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::Emitter;
use tauri::Manager;
use uuid::Uuid;

#[derive(Default)]
struct ProcessState {
    running: HashMap<String, Arc<Mutex<Child>>>,
    reserved: HashSet<String>,
    cancelled: HashSet<String>,
}

#[derive(Clone, Default)]
pub struct ProcessRegistry(Arc<Mutex<ProcessState>>);

/// 入队/任务结束等"可能腾出槽位"的事件通知，唤醒调度线程
#[derive(Clone)]
pub struct Notify(pub std::sync::mpsc::Sender<()>);

/// 引擎路径解析结果缓存（仅缓存命中；未命中时每次重扫，装好后无需重启）
static ENGINE_PATH: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// yt-dlp 定位：PATH → /opt/homebrew/bin → /usr/local/bin → ~/.local/bin。
/// GUI 直启时进程 PATH 不含 homebrew，必须显式补查（需求 v4 F4）。
pub fn engine_path() -> Result<std::path::PathBuf, String> {
    if let Some(cached) = ENGINE_PATH.lock().ok().and_then(|guard| guard.clone()) {
        return Ok(cached);
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if !dir.is_empty() {
                candidates.push(std::path::PathBuf::from(dir).join("yt-dlp"));
            }
        }
    }
    candidates.push(std::path::PathBuf::from("/opt/homebrew/bin/yt-dlp"));
    candidates.push(std::path::PathBuf::from("/usr/local/bin/yt-dlp"));
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            std::path::PathBuf::from(home)
                .join(".local")
                .join("bin")
                .join("yt-dlp"),
        );
    }
    for candidate in &candidates {
        if candidate.is_file() {
            if let Ok(mut guard) = ENGINE_PATH.lock() {
                *guard = Some(candidate.clone());
            }
            return Ok(candidate.clone());
        }
    }
    Err("未找到 yt-dlp，请先安装（如 brew install yt-dlp）后重试".into())
}

/// 下载归档文件路径（单文件；yt-dlp 归档行自带 提取器.视频ID 前缀，跨平台不冲突）
pub fn archive_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("download_archive.txt"))
        .map_err(|error| format!("无法确定配置目录：{error}"))
}

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
            if state.running.len() + state.reserved.len() >= capacity {
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

fn base_command(
    app: &tauri::AppHandle,
    platform: Platform,
    settings: &AppSettings,
) -> Result<Command, String> {
    let mut command = Command::new(engine_path()?);
    if let Some(proxy) = proxy_for(platform, settings)? {
        command.args(["--proxy", &proxy]);
    }
    if let Some(cookies) = crate::cookies::effective_path(app, settings, platform)? {
        command.arg("--cookies").arg(cookies);
    }
    Ok(command)
}

fn proxy_for(platform: Platform, settings: &AppSettings) -> Result<Option<String>, String> {
    if settings.proxy.youtube_only && !matches!(platform, Platform::Youtube | Platform::Twitter) {
        return Ok(None);
    }
    match settings.proxy.mode {
        ProxyMode::None => Ok(None),
        ProxyMode::System => system_proxy(),
        ProxyMode::Manual => {
            let parsed =
                url::Url::parse(&settings.proxy.url).map_err(|_| "代理地址格式无效".to_string())?;
            if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
                return Err("不支持的代理协议".into());
            }
            Ok(Some(settings.proxy.url.clone()))
        }
    }
}

pub(crate) fn system_proxy() -> Result<Option<String>, String> {
    if !cfg!(target_os = "macos") {
        return Ok(env_proxy());
    }
    let output = Command::new("scutil")
        .args(["--proxy"])
        .output()
        .map_err(|error| format!("无法读取系统代理设置：{error}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(parse_scutil_proxy(&String::from_utf8_lossy(&output.stdout)))
}

fn env_proxy() -> Option<String> {
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

fn parse_scutil_proxy(text: &str) -> Option<String> {
    let field = |key: &str| -> Option<String> {
        text.lines()
            .find(|line| line.trim_start().starts_with(key))
            .and_then(|line| line.split_once(':'))
            .map(|(_, value)| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let enabled = |key: &str| field(key).as_deref() == Some("1");
    if enabled("HTTPSEnable") {
        if let (Some(host), Some(port)) = (field("HTTPSProxy"), field("HTTPSPort")) {
            return Some(format!("http://{host}:{port}"));
        }
    }
    if enabled("HTTPEnable") {
        if let (Some(host), Some(port)) = (field("HTTPProxy"), field("HTTPPort")) {
            return Some(format!("http://{host}:{port}"));
        }
    }
    if enabled("SOCKSEnable") {
        if let (Some(host), Some(port)) = (field("SOCKSProxy"), field("SOCKSPort")) {
            return Some(format!("socks5://{host}:{port}"));
        }
    }
    None
}

pub fn probe(
    app: &tauri::AppHandle,
    source_url: &str,
    download_url: &str,
    platform: Platform,
    settings: &AppSettings,
) -> Result<MediaInfo, String> {
    let mut command = base_command(app, platform, settings)?;
    command.args([
        "--dump-single-json",
        "--no-playlist",
        "--no-warnings",
        "--",
        download_url,
    ]);
    let output = command.output().map_err(start_error)?;
    if !output.status.success() {
        return Err(map_error(
            &String::from_utf8_lossy(&output.stderr),
            platform,
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("无法读取媒体信息：{error}"))?;
    let formats = format_choices(&value, platform);
    Ok(MediaInfo {
        id: string_value(&value, "id").unwrap_or_else(|| Uuid::new_v4().to_string()),
        url: source_url.to_string(),
        platform,
        title: string_value(&value, "title").unwrap_or_else(|| "未命名媒体".into()),
        author: string_value(&value, "uploader").or_else(|| string_value(&value, "channel")),
        thumbnail: string_value(&value, "thumbnail"),
        duration: value.get("duration").and_then(Value::as_f64),
        formats,
    })
}

/// 播放列表 / 频道 / UP 主空间枚举（flat 模式：一次请求拿条目，不逐条解析）
pub fn probe_list(
    app: &tauri::AppHandle,
    url: &str,
    platform: Platform,
    settings: &AppSettings,
    limit: usize,
) -> Result<ListInfo, String> {
    let mut command = base_command(app, platform, settings)?;
    command.args([
        "--flat-playlist",
        "--dump-single-json",
        "--no-warnings",
        "--",
        url,
    ]);
    let output = command.output().map_err(start_error)?;
    if !output.status.success() {
        return Err(map_error(
            &String::from_utf8_lossy(&output.stderr),
            platform,
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("无法读取列表信息：{error}"))?;
    parse_list_json(&value, limit)
}

pub fn parse_list_json(value: &Value, limit: usize) -> Result<ListInfo, String> {
    let kind = value
        .get("_type")
        .and_then(Value::as_str)
        .unwrap_or("video");
    if matches!(kind, "video" | "url" | "url_transparent") {
        return Err("这不是播放列表或频道链接".into());
    }
    let raw = value
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = raw.len();
    let mut entries = Vec::new();
    for item in raw.into_iter().take(limit.max(1)) {
        let Some(url) = string_value(&item, "url") else {
            continue;
        };
        entries.push(ListEntry {
            id: string_value(&item, "id").unwrap_or_else(|| url.clone()),
            title: string_value(&item, "title"),
            duration: item.get("duration").and_then(|duration| {
                duration
                    .as_f64()
                    .or_else(|| duration.as_str().and_then(|text| text.parse().ok()))
            }),
            view_count: item.get("view_count").and_then(Value::as_u64),
            source: string_value(&item, "extractor_key").or_else(|| string_value(&item, "ie_key")),
            url,
        });
    }
    Ok(ListInfo {
        title: string_value(value, "title"),
        uploader: string_value(value, "uploader").or_else(|| string_value(value, "channel")),
        entries,
        total,
    })
}

/// `audio-mp3/m4a/opus` 拆解为格式表达式 + 音频提取参数；其余原样作为 --format 值
fn split_format_spec(spec: &str) -> (String, Vec<String>) {
    if let Some(audio_format) = spec.strip_prefix("audio-") {
        (
            "bestaudio/best".into(),
            vec![
                "--extract-audio".into(),
                "--audio-format".into(),
                audio_format.into(),
            ],
        )
    } else {
        (spec.to_string(), Vec::new())
    }
}

/// 字幕与弹幕参数（D5/D10）：B 站弹幕默认开、字幕默认关，仅 YouTube/B站生效
fn subtitle_args(platform: Platform, settings: &AppSettings) -> Vec<String> {
    let mut langs: Vec<&str> = Vec::new();
    if matches!(platform, Platform::Bilibili) && settings.danmaku_download {
        langs.push("danmaku");
    }
    if settings.subtitle_download && matches!(platform, Platform::Youtube | Platform::Bilibili) {
        langs.extend(["zh-Hans", "zh", "en"]);
    }
    if langs.is_empty() {
        return Vec::new();
    }
    vec![
        "--write-subs".into(),
        "--write-auto-subs".into(),
        "--sub-langs".into(),
        langs.join(","),
    ]
}

/// 元数据与封面嵌入（需求 v4 F1）：MP3 补封面格式转换；M4A/MP4 缺 AtomicParsley
/// 时 yt-dlp 自动降级为警告，任务不失败（D5）
fn embed_args(format_id: &str, settings: &AppSettings) -> Vec<String> {
    if !settings.embed_media_meta {
        return Vec::new();
    }
    let mut args = vec!["--embed-metadata".to_string()];
    if format_id == "audio-mp3" {
        args.push("--convert-thumbnails".into());
        args.push("jpg".into());
    }
    args.push("--embed-thumbnail".into());
    args
}

fn download_output_dir(
    app: &tauri::AppHandle,
    settings: &AppSettings,
    subfolder: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let base = if settings.download_dir.trim().is_empty() {
        app.path()
            .download_dir()
            .map_err(|error| format!("无法确定下载目录：{error}"))?
            .join("GetTube")
    } else {
        std::path::PathBuf::from(&settings.download_dir)
    };
    Ok(match subfolder {
        Some(name) => base.join(sanitize_folder(name)),
        None => base,
    })
}

/// 批量文件夹名消毒：去路径分隔符与非法字符、限长、兜底名称
pub fn sanitize_folder(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            _ => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    let limited: String = trimmed.chars().take(80).collect();
    let limited = limited.trim();
    if limited.is_empty() {
        "playlist".into()
    } else {
        limited.to_string()
    }
}

fn format_choices(value: &Value, platform: Platform) -> Vec<MediaFormat> {
    if matches!(platform, Platform::Douyin) {
        // 抖音的 download_addr 系列带 watermark=1（App 端动态水印源），播放格式
        // （h264_* 等）为无水印源。不设 /best 兜底：仅剩带水印源时显式失败（需求 D4）。
        return vec![MediaFormat {
            id: "best[format_id!^=download_addr]".into(),
            label: "无水印（最佳）".into(),
            extension: Some("mp4".into()),
            estimated_size: None,
        }];
    }
    let mut choices = vec![MediaFormat {
        id: "bestvideo*+bestaudio/best".into(),
        label: "最佳视频与音频".into(),
        extension: Some("mp4".into()),
        estimated_size: None,
    }];
    match platform {
        // D6：音频格式选择仅 YouTube/B站生效
        Platform::Youtube | Platform::Bilibili => {
            for (format, label) in [
                ("mp3", "仅音频（MP3）"),
                ("m4a", "仅音频（M4A）"),
                ("opus", "仅音频（OPUS）"),
            ] {
                choices.push(MediaFormat {
                    id: format!("audio-{format}"),
                    label: label.into(),
                    extension: Some(format.into()),
                    estimated_size: None,
                });
            }
        }
        _ => {
            choices.push(MediaFormat {
                id: "bestaudio/best".into(),
                label: "仅音频（最佳质量）".into(),
                extension: None,
                estimated_size: None,
            });
        }
    }
    if let Some(formats) = value.get("formats").and_then(Value::as_array) {
        for height in [2160, 1440, 1080, 720, 480] {
            if formats
                .iter()
                .any(|item| item.get("height").and_then(Value::as_u64) == Some(height))
            {
                choices.push(MediaFormat {
                    id: format!("bestvideo[height<={height}]+bestaudio/best[height<={height}]"),
                    label: format!("最高 {height}p"),
                    extension: Some("mp4".into()),
                    estimated_size: None,
                });
            }
        }
    }
    choices
}

fn parse_progress(pattern: &Regex, line: &str) -> Option<(f64, String, String)> {
    let values = pattern.captures(line)?;
    Some((
        values[1].parse().ok()?,
        values[2].to_string(),
        values[3].to_string(),
    ))
}

fn final_output_path(line: &str) -> Option<String> {
    let path = line.trim();
    let windows_drive = path.as_bytes().get(1) == Some(&b':')
        && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    if path.starts_with('/') || path.starts_with("\\\\") || windows_drive {
        Some(path.to_string())
    } else {
        None
    }
}

fn string_value(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn start_error(error: std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        "未找到 yt-dlp，请先安装或配置下载引擎".into()
    } else {
        format!("无法启动下载引擎：{error}")
    }
}

fn map_error(stderr: &str, platform: Platform) -> String {
    let lower = stderr.to_lowercase();
    if lower.contains("proxy") || lower.contains("connection refused") {
        "代理连接失败，请检查代理程序、协议和端口".into()
    } else if matches!(platform, Platform::Twitter) {
        "X 内容通常需要登录 Cookie，请导入有效的 X cookies.txt".into()
    } else if lower.contains("sign in") || lower.contains("cookies") {
        "此内容需要登录，请导入有效 Cookie".into()
    } else if lower.contains("unsupported url") {
        "下载引擎暂不支持此链接".into()
    } else if lower.contains("requested format") {
        "没有可用的无水印格式，该内容可能仅提供带水印源".into()
    } else {
        stderr.lines().last().unwrap_or("媒体解析失败").to_string()
    }
}

fn map_download_error(stderr: &str, platform: Platform) -> String {
    let lower = stderr.to_lowercase();
    if lower.contains("requested format") {
        return "没有可用的无水印格式，该内容可能仅提供带水印源".into();
    }
    if matches!(platform, Platform::Twitter) {
        return "X 下载失败；X 通常需要有效登录 Cookie，并请检查代理设置".into();
    }
    if stderr.trim().is_empty() {
        "下载进程异常退出，请检查网络、代理或登录状态".into()
    } else {
        map_error(stderr, platform)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_download_progress() {
        let pattern =
            Regex::new(r"\[download\]\s+([0-9.]+)%.*?at\s+([^\s]+).*?ETA\s+([^\s]+)").unwrap();
        let parsed =
            parse_progress(&pattern, "[download]  42.3% of 10MiB at 2.4MiB/s ETA 00:04").unwrap();
        assert_eq!(parsed, (42.3, "2.4MiB/s".into(), "00:04".into()));
    }

    #[test]
    fn preserves_absolute_output_paths() {
        assert_eq!(
            final_output_path("/Users/test/video.mp4\n").as_deref(),
            Some("/Users/test/video.mp4")
        );
        assert_eq!(
            final_output_path(r"C:\Users\test\video.mp4").as_deref(),
            Some(r"C:\Users\test\video.mp4")
        );
        assert_eq!(final_output_path("[download] 100%"), None);
    }

    #[test]
    fn reports_missing_unwatermarked_format_during_download() {
        assert_eq!(
            map_download_error("ERROR: Requested format is not available", Platform::Douyin),
            "没有可用的无水印格式，该内容可能仅提供带水印源"
        );
    }

    #[test]
    fn parses_scutil_proxy() {
        let sample = "<dictionary> {\n  HTTPEnable : 0\n  HTTPProxy : 127.0.0.1\n  HTTPPort : 7890\n  HTTPSEnable : 1\n  HTTPSProxy : 127.0.0.1\n  HTTPSPort : 7890\n  SOCKSEnable : 0\n  SOCKSProxy : 127.0.0.1\n  SOCKSPort : 7890\n}\n";
        assert_eq!(
            parse_scutil_proxy(sample).as_deref(),
            Some("http://127.0.0.1:7890")
        );
        let socks_only = "SOCKSEnable : 1\nSOCKSProxy : 127.0.0.1\nSOCKSPort : 1080\n";
        assert_eq!(
            parse_scutil_proxy(socks_only).as_deref(),
            Some("socks5://127.0.0.1:1080")
        );
        let disabled = "HTTPEnable : 0\nHTTPSEnable : 0\nSOCKSEnable : 0\n";
        assert_eq!(parse_scutil_proxy(disabled), None);
    }

    #[test]
    fn builds_embed_args_per_format_and_setting() {
        let mut settings = crate::models::AppSettings::default();
        assert!(settings.embed_media_meta);
        let args = embed_args("audio-mp3", &settings);
        assert_eq!(
            args,
            vec![
                "--embed-metadata".to_string(),
                "--convert-thumbnails".to_string(),
                "jpg".to_string(),
                "--embed-thumbnail".to_string()
            ]
        );
        let plain = embed_args("bestvideo*+bestaudio/best", &settings);
        assert_eq!(plain, vec!["--embed-metadata", "--embed-thumbnail"]);
        settings.embed_media_meta = false;
        assert!(embed_args("audio-mp3", &settings).is_empty());
    }

    #[test]
    fn splits_audio_format_specs() {
        let (expr, args) = split_format_spec("audio-mp3");
        assert_eq!(expr, "bestaudio/best");
        assert_eq!(
            args,
            vec![
                "--extract-audio".to_string(),
                "--audio-format".to_string(),
                "mp3".to_string()
            ]
        );
        let (expr, args) = split_format_spec("bestvideo*+bestaudio/best");
        assert_eq!(expr, "bestvideo*+bestaudio/best");
        assert!(args.is_empty());
    }

    #[test]
    fn sanitizes_batch_folder_names() {
        assert_eq!(sanitize_folder("我的/收藏:合集*?"), "我的 收藏 合集");
        assert_eq!(sanitize_folder("   ..  "), "playlist");
        let long = "x".repeat(300);
        assert_eq!(sanitize_folder(&long).chars().count(), 80);
    }

    #[test]
    fn parses_flat_playlist_json() {
        let value = serde_json::json!({
            "_type": "playlist",
            "title": "收藏夹",
            "channel": "某UP",
            "entries": [
                {"id": "a", "title": "第一集", "duration": 61, "view_count": 1234, "ie_key": "Youtube", "url": "https://www.youtube.com/watch?v=a"},
                {"id": "b", "title": null, "duration": "77", "url": "https://www.bilibili.com/video/BVb"},
                {"id": "c"},
                {"id": "d", "url": "https://www.youtube.com/watch?v=d"}
            ]
        });
        let info = parse_list_json(&value, 10).unwrap();
        assert_eq!(info.total, 4);
        assert_eq!(info.entries.len(), 3); // 缺 url 的条目跳过
        assert_eq!(info.entries[0].view_count, Some(1234));
        assert_eq!(info.entries[0].source.as_deref(), Some("Youtube"));
        assert_eq!(info.entries[1].duration, Some(77.0));
        assert_eq!(info.uploader.as_deref(), Some("某UP"));
        let limited = parse_list_json(&value, 1).unwrap();
        assert_eq!(limited.entries.len(), 1);
        assert_eq!(limited.total, 4);
        let single = serde_json::json!({"_type": "video", "id": "x"});
        assert!(parse_list_json(&single, 10).is_err());
    }
}
