use crate::models::AppSettings;
use std::fs;
use tauri::Manager;

static SETTINGS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join("settings.json"))
        .map_err(|error| format!("无法确定配置目录：{error}"))
}

pub fn load(app: &tauri::AppHandle) -> Result<AppSettings, String> {
    let _guard = SETTINGS_LOCK.lock().map_err(|_| "设置锁不可用")?;
    load_unlocked(app)
}

fn load_unlocked(app: &tauri::AppHandle) -> Result<AppSettings, String> {
    let path = path(app)?;
    if !path.exists() {
        return Ok(AppSettings::default());
    }
    let content = fs::read_to_string(path).map_err(|error| format!("无法读取设置：{error}"))?;
    serde_json::from_str(&content).map_err(|error| format!("设置文件无效：{error}"))
}

fn save_unlocked(app: &tauri::AppHandle, settings: &AppSettings) -> Result<(), String> {
    if settings.concurrent_downloads == 0 || settings.concurrent_downloads > 5 {
        return Err("同时下载数量必须在 1 到 5 之间".into());
    }
    let template = std::path::Path::new(&settings.filename_template);
    if template.is_absolute()
        || template
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("文件名模板不能是绝对路径或包含上级目录".into());
    }
    validate_proxy(settings)?;
    let path = path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建配置目录：{error}"))?;
    }
    let content = serde_json::to_string_pretty(settings).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, content).map_err(|error| format!("无法保存设置：{error}"))?;
    fs::rename(temporary, path).map_err(|error| format!("无法保存设置：{error}"))
}

/// 保存用户可编辑设置时保留后端维护的检查缓存，避免前端旧快照覆盖节流状态。
pub fn save_user(app: &tauri::AppHandle, mut settings: AppSettings) -> Result<AppSettings, String> {
    let _guard = SETTINGS_LOCK.lock().map_err(|_| "设置锁不可用")?;
    let persisted = load_unlocked(app)?;
    settings.last_version_check = persisted.last_version_check;
    settings.last_ytdlp_check = persisted.last_ytdlp_check;
    settings.last_ytdlp_latest = persisted.last_ytdlp_latest;
    save_unlocked(app, &settings)?;
    Ok(settings)
}

pub fn set_last_version_check(app: &tauri::AppHandle, checked: i64) -> Result<(), String> {
    let _guard = SETTINGS_LOCK.lock().map_err(|_| "设置锁不可用")?;
    let mut settings = load_unlocked(app)?;
    settings.last_version_check = Some(checked);
    save_unlocked(app, &settings)
}

pub fn set_ytdlp_check(app: &tauri::AppHandle, checked: i64, latest: String) -> Result<(), String> {
    let _guard = SETTINGS_LOCK.lock().map_err(|_| "设置锁不可用")?;
    let mut settings = load_unlocked(app)?;
    settings.last_ytdlp_check = Some(checked);
    settings.last_ytdlp_latest = Some(latest);
    save_unlocked(app, &settings)
}

fn validate_proxy(settings: &AppSettings) -> Result<(), String> {
    use crate::models::ProxyMode;
    if matches!(settings.proxy.mode, ProxyMode::Manual) {
        let parsed =
            url::Url::parse(&settings.proxy.url).map_err(|_| "代理地址格式无效".to_string())?;
        if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
            return Err("代理协议必须是 HTTP、HTTPS、SOCKS5 或 SOCKS5H".into());
        }
    }
    Ok(())
}
