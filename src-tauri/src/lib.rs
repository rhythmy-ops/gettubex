mod cookies;
mod database;
mod downloads;
mod engine;
mod hongguo;
mod models;
mod network;
mod platform;
mod resolver;
mod settings;
mod updater;
mod yt_dlp;

use database::Database;
use models::{AppSettings, DownloadTask, ListInfo, MediaInfo, Platform};
use tauri::Manager;

/// 批量入队条目（前端列表预览勾选项）
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchItem {
    pub url: String,
    pub title: String,
}

#[tauri::command]
async fn probe_media(app: tauri::AppHandle, url: String) -> Result<MediaInfo, String> {
    let platform = platform::detect(&url)?;
    tauri::async_runtime::spawn_blocking(move || {
        let settings = settings::load(&app)?;
        let download_url = resolver::execution_url(&app, &url, platform, &settings)?;
        if matches!(platform, Platform::Hongguo) {
            return Err("请通过分集列表选择红果剧集".into());
        }
        yt_dlp::probe(&app, &url, &download_url, platform, &settings)
    })
    .await
    .map_err(|error| format!("解析线程异常退出：{error}"))?
}

#[tauri::command]
async fn probe_list(
    app: tauri::AppHandle,
    url: String,
    limit: Option<u32>,
) -> Result<ListInfo, String> {
    let platform = platform::detect(&url)?;
    if !matches!(
        platform,
        Platform::Youtube | Platform::Bilibili | Platform::Hongguo
    ) {
        return Err("播放列表与 UP 主功能当前仅支持 YouTube、哔哩哔哩和红果".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let settings = settings::load(&app)?;
        if matches!(platform, Platform::Hongguo) {
            return hongguo::probe(&url, &settings);
        }
        let limit = limit
            .map(|value| value.max(1) as usize)
            .unwrap_or(usize::MAX);
        yt_dlp::probe_list(&app, &url, platform, &settings, limit)
    })
    .await
    .map_err(|error| format!("列表解析线程异常退出：{error}"))?
}

#[tauri::command]
fn create_download(
    app: tauri::AppHandle,
    database: tauri::State<Database>,
    notify: tauri::State<downloads::Notify>,
    url: String,
    format_id: String,
    title: String,
) -> Result<DownloadTask, String> {
    let platform = platform::detect(&url)?;
    if matches!(platform, Platform::Hongguo) {
        hongguo::validate_episode(&url, &format_id)?;
    }
    downloads::enqueue(
        &app,
        database.inner(),
        notify.inner(),
        downloads::DownloadRequest {
            source_url: url,
            title,
            format_id,
            platform,
            batch_id: None,
            batch_name: None,
        },
    )
}

#[tauri::command]
fn create_downloads(
    app: tauri::AppHandle,
    database: tauri::State<Database>,
    notify: tauri::State<downloads::Notify>,
    items: Vec<BatchItem>,
    format_id: String,
    folder: String,
) -> Result<Vec<DownloadTask>, String> {
    if items.is_empty() {
        return Err("没有选择任何条目".into());
    }
    if format_id.trim().is_empty() {
        return Err("缺少下载格式".into());
    }
    let batch_id = uuid::Uuid::new_v4().to_string();
    let batch_name = yt_dlp::sanitize_folder(&folder);
    let mut requests = Vec::with_capacity(items.len());
    for item in items {
        let platform = platform::detect(&item.url)?;
        if !matches!(
            platform,
            Platform::Youtube | Platform::Bilibili | Platform::Hongguo
        ) {
            return Err("批量下载当前仅支持 YouTube、哔哩哔哩和红果".into());
        }
        if matches!(platform, Platform::Hongguo) {
            hongguo::validate_episode(&item.url, &format_id)?;
        }
        let title = if item.title.trim().is_empty() {
            "未命名条目".into()
        } else {
            item.title
        };
        requests.push(downloads::DownloadRequest {
            source_url: item.url,
            title,
            format_id: format_id.clone(),
            platform,
            batch_id: Some(batch_id.clone()),
            batch_name: Some(batch_name.clone()),
        });
    }
    downloads::enqueue_many(&app, database.inner(), notify.inner(), requests)
}

#[tauri::command]
fn list_tasks(database: tauri::State<Database>) -> Result<Vec<DownloadTask>, String> {
    database.list()
}

/// 清除任务记录：history=已完成（历史页）；ended=完成/失败/取消（任务页清理，进行中不受影响）
#[tauri::command]
fn clear_tasks(database: tauri::State<Database>, scope: String) -> Result<u32, String> {
    let statuses: &[&str] = match scope.as_str() {
        "history" => &["completed"],
        "ended" => &["completed", "failed", "cancelled"],
        _ => return Err("未知的清除范围".into()),
    };
    let removed = database.clear_tasks(statuses)?;
    Ok(removed as u32)
}

#[tauri::command]
fn cancel_download(
    app: tauri::AppHandle,
    database: tauri::State<Database>,
    processes: tauri::State<downloads::ProcessRegistry>,
    id: String,
) -> Result<(), String> {
    downloads::cancel(&app, database.inner(), processes.inner(), &id)
}

#[tauri::command]
fn cancel_batch(
    app: tauri::AppHandle,
    database: tauri::State<Database>,
    processes: tauri::State<downloads::ProcessRegistry>,
    batch_id: String,
) -> Result<(), String> {
    downloads::cancel_batch(&app, database.inner(), processes.inner(), &batch_id)
}

#[tauri::command]
fn retry_download(
    app: tauri::AppHandle,
    database: tauri::State<Database>,
    notify: tauri::State<downloads::Notify>,
    id: String,
) -> Result<DownloadTask, String> {
    let data = database.retry_data(&id)?;
    let platform = platform::detect(&data.url)?;
    if matches!(platform, Platform::Hongguo) {
        hongguo::validate_episode(&data.url, &data.format_id)?;
    }
    downloads::enqueue(
        &app,
        database.inner(),
        notify.inner(),
        downloads::DownloadRequest {
            source_url: data.url,
            title: data.title,
            format_id: data.format_id,
            platform,
            batch_id: data.batch_id,
            batch_name: data.batch_name,
        },
    )
}

#[tauri::command]
fn load_settings(app: tauri::AppHandle) -> Result<AppSettings, String> {
    settings::load(&app)
}

#[tauri::command]
fn save_settings(
    app: tauri::AppHandle,
    mut settings: AppSettings,
    cookie_drafts: cookies::CookieDrafts,
) -> Result<AppSettings, String> {
    cookies::apply_drafts(&app, &mut settings, &cookie_drafts)?;
    settings::save_user(&app, settings)
}

#[tauri::command]
fn clear_managed_cookie(app: tauri::AppHandle, platform: String) -> Result<AppSettings, String> {
    let platform = parse_platform_key(&platform)?;
    let mut settings = settings::load(&app)?;
    cookies::clear_managed(&app, &mut settings, platform)?;
    settings::save_user(&app, settings)
}

#[tauri::command]
async fn check_updates(
    app: tauri::AppHandle,
    manual: bool,
) -> Result<updater::UpdateResult, String> {
    tauri::async_runtime::spawn_blocking(move || updater::check(&app, manual))
        .await
        .map_err(|error| format!("更新检查线程异常退出：{error}"))?
}

/// yt-dlp 引擎状态（版本/路径/最新版，24h 节流仅计成功）
#[tauri::command]
async fn check_ytdlp(app: tauri::AppHandle, manual: bool) -> Result<engine::EngineStatus, String> {
    tauri::async_runtime::spawn_blocking(move || engine::check(&app, manual))
        .await
        .map_err(|error| format!("引擎检查线程异常退出：{error}"))?
}

#[tauri::command]
async fn check_hongguo() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| {
        hongguo::request(
            &uuid::Uuid::new_v4().to_string(),
            "status",
            serde_json::json!({}),
            &std::sync::atomic::AtomicBool::new(false),
            |_| {},
        )
    })
    .await
    .map_err(|_| "红果引擎检测线程异常")?
}

/// 清空下载归档，返回清除条数
#[tauri::command]
fn clear_archive(app: tauri::AppHandle) -> Result<u64, String> {
    engine::clear_archive(&app)
}

#[tauri::command]
fn open_release_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    updater::open_release(&app, &url)
}

#[tauri::command]
fn app_version(app: tauri::AppHandle) -> String {
    updater::current_version(&app)
}

fn parse_platform_key(value: &str) -> Result<Platform, String> {
    match value {
        "youtube" => Ok(Platform::Youtube),
        "bilibili" => Ok(Platform::Bilibili),
        "douyin" => Ok(Platform::Douyin),
        "twitter" => Ok(Platform::Twitter),
        _ => Err("未知 Cookie 平台".into()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(data_dir)?;
            let database = Database::new(app.handle()).map_err(std::io::Error::other)?;
            let registry = downloads::ProcessRegistry::default();
            let notify = downloads::start_dispatcher(
                app.handle().clone(),
                database.clone(),
                registry.clone(),
            );
            app.manage(database);
            app.manage(registry);
            app.manage(notify);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            probe_media,
            probe_list,
            create_download,
            create_downloads,
            list_tasks,
            clear_tasks,
            cancel_download,
            cancel_batch,
            retry_download,
            load_settings,
            save_settings,
            clear_managed_cookie,
            check_updates,
            check_ytdlp,
            check_hongguo,
            clear_archive,
            open_release_url,
            app_version
        ])
        .build(tauri::generate_context!())
        .expect("error while building GetTube")
        .run(|_app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                hongguo::shutdown();
            }
        });
}

#[cfg(test)]
mod version_tests {
    #[test]
    fn project_versions_are_consistent() {
        let tauri: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let package: serde_json::Value =
            serde_json::from_str(include_str!("../../package.json")).unwrap();
        assert_eq!(tauri["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
        assert_eq!(package["version"].as_str(), Some(env!("CARGO_PKG_VERSION")));
    }
}
