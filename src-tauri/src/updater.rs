use semver::Version;
use serde::{Deserialize, Serialize};
use tauri_plugin_opener::OpenerExt;

const UPDATE_REPO: &str = match option_env!("GETTUBE_UPDATE_REPO") {
    Some(value) => value,
    None => "",
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateResult {
    pub status: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub summary: Option<String>,
    pub release_url: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    body: Option<String>,
    html_url: String,
}

pub fn check(app: &tauri::AppHandle, manual: bool) -> Result<UpdateResult, String> {
    let current_raw = raw_version(app);
    let current = Version::parse(&current_raw).map_err(|_| "当前应用版本格式无效")?;
    if UPDATE_REPO.is_empty() {
        return Ok(result(
            "channel_unconfigured",
            &current_raw,
            None,
            None,
            None,
        ));
    }
    let settings = crate::settings::load(app)?;
    if !should_check(settings.last_version_check, unix_now(), manual) {
        return Ok(result("skipped", &current_raw, None, None, None));
    }
    let endpoint = format!("https://api.github.com/repos/{UPDATE_REPO}/releases/latest");
    let response = crate::network::client(&settings, true)?
        .get(endpoint)
        .header("User-Agent", format!("GetTube/{current_raw}"))
        .send()
        .map_err(|error| format!("检查更新失败：{error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(result(
            "channel_unavailable",
            &current_raw,
            None,
            None,
            None,
        ));
    }
    let release: Release = response
        .error_for_status()
        .map_err(|error| format!("检查更新失败：{error}"))?
        .json()
        .map_err(|error| format!("更新响应格式无效：{error}"))?;
    let latest_raw = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let latest = Version::parse(latest_raw).map_err(|_| "发布版本格式无效")?;
    validate_release_url(&release.html_url)?;
    crate::settings::set_last_version_check(app, unix_now())?;
    let status = if latest > current {
        "available"
    } else {
        "latest"
    };
    Ok(result(
        status,
        &current_raw,
        Some(release.tag_name),
        release.body.map(|body| summarize(&body)),
        Some(release.html_url),
    ))
}

pub fn current_version(app: &tauri::AppHandle) -> String {
    let current = raw_version(app);
    if cfg!(debug_assertions) {
        format!("{current}-dev")
    } else {
        current
    }
}

fn raw_version(app: &tauri::AppHandle) -> String {
    app.config()
        .version
        .clone()
        .unwrap_or_else(|| "0.0.0".into())
}

pub fn open_release(app: &tauri::AppHandle, url: &str) -> Result<(), String> {
    validate_release_url(url)?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|error| format!("无法打开发布页：{error}"))
}

fn validate_release_url(value: &str) -> Result<(), String> {
    if UPDATE_REPO.is_empty() {
        return Err("发布渠道尚未配置".into());
    }
    let expected = format!("https://github.com/{UPDATE_REPO}/releases/");
    if value.starts_with(&expected) {
        Ok(())
    } else {
        Err("发布页地址未通过安全校验".into())
    }
}

fn result(
    status: &str,
    current: &str,
    latest: Option<String>,
    summary: Option<String>,
    url: Option<String>,
) -> UpdateResult {
    UpdateResult {
        status: status.into(),
        current_version: if cfg!(debug_assertions) {
            format!("{current}-dev")
        } else {
            current.into()
        },
        latest_version: latest,
        summary,
        release_url: url,
    }
}

fn summarize(value: &str) -> String {
    let compact = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    compact.chars().take(240).collect()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn should_check(last_success: Option<i64>, now: i64, manual: bool) -> bool {
    manual || last_success.is_none_or(|time| now.saturating_sub(time) >= 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trims_release_summary() {
        assert_eq!(summarize(" hello\n\n world "), "hello world");
    }
    #[test]
    fn only_successful_recent_check_is_throttled() {
        assert!(!should_check(Some(100), 200, false));
        assert!(should_check(None, 200, false));
        assert!(should_check(Some(100), 200, true));
        assert!(should_check(Some(100), 100 + 86_400, false));
    }
}
