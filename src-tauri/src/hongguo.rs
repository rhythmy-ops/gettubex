use crate::models::{AppSettings, ListInfo};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};
include!(concat!(env!("OUT_DIR"), "/hongguo_assets.rs"));

struct Bridge {
    input: Mutex<ChildStdin>,
    child: Mutex<Child>,
    pending: Arc<Mutex<HashMap<String, mpsc::Sender<Value>>>>,
    alive: Arc<AtomicBool>,
    root: std::path::PathBuf,
}
static BRIDGE: Mutex<Option<Arc<Bridge>>> = Mutex::new(None);

impl Bridge {
    fn stop(&self) {
        if let Ok(mut child) = self.child.lock() {
            if child.try_wait().ok().flatten().is_none() {
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM);
                }
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.alive.store(false, Ordering::SeqCst);
    }
    fn send(&self, message: Value) -> Result<(), String> {
        let mut input = self.input.lock().map_err(|_| "红果引擎写入锁异常")?;
        writeln!(input, "{message}")
            .and_then(|_| input.flush())
            .map_err(|_| "红果引擎连接中断".into())
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
pub fn shutdown() {
    if let Ok(mut state) = BRIDGE.lock() {
        if let Some(engine) = state.take() {
            engine.stop();
        }
    }
}
fn bridge() -> Result<Arc<Bridge>, String> {
    let mut state = BRIDGE.lock().map_err(|_| "红果引擎锁异常")?;
    if let Some(existing) = state.as_ref() {
        if existing.alive.load(Ordering::SeqCst) {
            return Ok(existing.clone());
        }
    }
    // Ship code as application resources, not runtime reads from the user's Documents.
    let root = std::env::temp_dir().join(format!("gettube-engine-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).map_err(|e| format!("无法准备红果引擎：{e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    for (name, bytes) in ENGINE_ASSETS {
        let file = root.join(name);
        std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(file, bytes).map_err(|e| e.to_string())?;
    }
    let node = find_node().ok_or("红果引擎需要 Node.js 22.12 或更高版本")?;
    let mut command = Command::new(node);
    command
        .arg(root.join("server.cjs"))
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("无法启动红果引擎：{e}"))?;
    let input = child.stdin.take().ok_or("无法读取引擎输入")?;
    let output = child.stdout.take().ok_or("无法读取引擎输出")?;
    let pending: Arc<Mutex<HashMap<String, mpsc::Sender<Value>>>> = Arc::default();
    let alive = Arc::new(AtomicBool::new(true));
    let readers = pending.clone();
    let running = alive.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(id) = event["id"].as_str() else {
                continue;
            };
            if let Ok(mut map) = readers.lock() {
                if let Some(sender) = map.get(id) {
                    let _ = sender.send(event.clone());
                }
                if matches!(event["type"].as_str(), Some("result" | "error")) {
                    map.remove(id);
                }
            }
        }
        running.store(false, Ordering::SeqCst);
        if let Ok(mut map) = readers.lock() {
            for (_, sender) in map.drain() {
                let _ = sender.send(json!({"type":"error", "error":"红果引擎异常退出，请重试"}));
            }
        }
    });
    let result = Arc::new(Bridge {
        input: Mutex::new(input),
        child: Mutex::new(child),
        pending,
        alive,
        root,
    });
    *state = Some(result.clone());
    Ok(result)
}

/// Node 定位：PATH → /opt/homebrew/bin → /usr/local/bin → nvm 各版本（取最高）。
/// GUI 直启时进程 PATH 不含 nvm，必须显式补查（与 yt_dlp::engine_path 同类策略）。
fn find_node() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    let mut candidates: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|p| p.join(name))
        .collect();
    candidates.push(std::path::PathBuf::from("/opt/homebrew/bin").join(name));
    candidates.push(std::path::PathBuf::from("/usr/local/bin").join(name));
    if let Some(candidate) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(candidate);
    }
    let home = std::env::var_os("HOME")?;
    find_node_in_nvm(std::path::PathBuf::from(home))
}

/// 在 HOME 下的 .nvm/versions/node 里找最高版本的 node（数值比较，避免 v9 > v24 的字典序坑）
fn find_node_in_nvm(home: std::path::PathBuf) -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    let versions = home.join(".nvm").join("versions").join("node");
    let mut found: Vec<(Vec<u64>, std::path::PathBuf)> = std::fs::read_dir(versions)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let version = dir_name.strip_prefix('v')?;
            let numbers: Vec<u64> = version
                .split('.')
                .map(|part| part.parse::<u64>().ok())
                .collect::<Option<_>>()?;
            Some((numbers, entry.path().join("bin").join(name)))
        })
        .filter(|(_, path)| path.is_file())
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().next().map(|(_, path)| path)
}

pub fn request(
    id: &str,
    method: &str,
    params: Value,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(&Value),
) -> Result<Value, String> {
    if cancelled.load(Ordering::SeqCst) {
        return Err("任务已取消".into());
    }
    let engine = bridge()?;
    let (sender, receiver) = mpsc::channel();
    engine
        .pending
        .lock()
        .map_err(|_| "红果引擎锁异常")?
        .insert(id.into(), sender);
    let outcome = (|| {
        engine.send(json!({ "id": id, "method": method, "params": params }))?;
        let started = Instant::now();
        let limit = Duration::from_secs(if method == "download" { 1830 } else { 70 });
        let mut cancel_sent = false;
        loop {
            if cancelled.load(Ordering::SeqCst) && !cancel_sent {
                engine.send(json!({"id":id,"method":"cancel"}))?;
                cancel_sent = true;
            }
            match receiver.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => match event["type"].as_str() {
                    Some("result") => return Ok(event["result"].clone()),
                    Some("error") => {
                        return Err(event["error"].as_str().unwrap_or("红果任务失败").into())
                    }
                    _ => progress(&event),
                },
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("红果引擎连接中断".into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if started.elapsed() > limit {
                let _ = engine.send(json!({"id":id,"method":"cancel"}));
                return Err("红果任务超时，请重试".into());
            }
        }
    })();
    if let Ok(mut map) = engine.pending.lock() {
        map.remove(id);
    }
    outcome
}
pub fn proxy(settings: &AppSettings) -> Result<Option<String>, String> {
    if settings.proxy.youtube_only {
        Ok(None)
    } else {
        crate::network::proxy_url(settings)
    }
}
pub fn probe(url: &str, settings: &AppSettings) -> Result<ListInfo, String> {
    let result = request(
        &uuid::Uuid::new_v4().to_string(),
        "probe",
        json!({"url":url,"proxy":proxy(settings)?}),
        &AtomicBool::new(false),
        |_| {},
    )?;
    serde_json::from_value(result).map_err(|_| "红果分集数据格式无效".into())
}
pub fn validate_episode(url: &str, quality: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| "分集地址无效")?;
    let pattern = regex::Regex::new(r"^/player/[1-9]\d{0,29}/[1-9]\d{0,29}/?$").unwrap();
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("hongguoduanju.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !pattern.is_match(parsed.path())
    {
        return Err("请先解析红果链接并选择分集".into());
    }
    if !matches!(quality, "auto" | "1080p" | "720p") {
        return Err("请选择红果视频画质".into());
    }
    Ok(())
}

pub fn task_details(url: &str, quality: &str) -> Option<Value> {
    validate_episode(url, quality).ok()?;
    let parsed = url::Url::parse(url).ok()?;
    let parts: Vec<_> = parsed.path().split('/').collect();
    Some(
        json!({"engine":"hongguo", "seriesId":parts[2], "episodeId":parts[3], "requestedQuality":quality}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nvm_versions_sort_numerically_not_lexically() {
        let mut found: Vec<(Vec<u64>, std::path::PathBuf)> = vec![
            (vec![9], "v9".into()),
            (vec![24, 14, 0], "v24.14.0".into()),
            (vec![22, 12], "v22.12.0".into()),
        ];
        found.sort_by(|a, b| b.0.cmp(&a.0));
        assert_eq!(found[0].1, std::path::PathBuf::from("v24.14.0"));
        assert_eq!(found[2].1, std::path::PathBuf::from("v9"));
    }

    #[test]
    fn finds_node_in_nvm_layout() {
        // 本机 node 仅装在 nvm；模拟 GUI 直启（无 PATH）场景直接扫 nvm 目录
        let home = std::env::var_os("HOME").expect("HOME");
        let node =
            find_node_in_nvm(std::path::PathBuf::from(home)).expect("本机应能在 nvm 中找到 node");
        assert!(node.to_string_lossy().contains(".nvm"));
    }

    #[test]
    fn validates_episode_and_quality_before_enqueue() {
        assert!(validate_episode(
            "https://hongguoduanju.com/player/123/9007199254740993",
            "720p"
        )
        .is_ok());
        for value in [
            "https://hongguoduanju.com.evil.test/player/123/456",
            "https://hongguoduanju.com/player/123",
            "https://user@hongguoduanju.com/player/1/2",
        ] {
            assert!(validate_episode(value, "auto").is_err());
        }
        assert!(validate_episode("https://hongguoduanju.com/player/1/2", "audio-mp3").is_err());
    }

    #[test]
    fn stdio_bridge_returns_status_and_isolates_failed_requests() {
        let cancelled = AtomicBool::new(false);
        let status = request("test-status", "status", json!({}), &cancelled, |_| {}).unwrap();
        assert!(status["nodeVersion"].as_str().is_some());
        let error = request(
            "test-invalid",
            "probe",
            json!({"url":"https://localhost/private"}),
            &cancelled,
            |_| {},
        )
        .unwrap_err();
        assert!(error.contains("分享链接"));
        let again = request("test-status-again", "status", json!({}), &cancelled, |_| {}).unwrap();
        assert_eq!(status, again);
    }
}
