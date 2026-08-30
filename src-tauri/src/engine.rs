use crate::models::AppSettings;
use serde::Serialize;

/// yt-dlp 引擎状态（需求 v4 F3）：本地信息始终返回，网络检查受 24h 节流
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    /// ok | outdated | missing | skipped | channel_unavailable
    pub status: String,
    pub current_version: Option<String>,
    pub engine_path: Option<String>,
    pub latest_version: Option<String>,
    pub upgrade_command: Option<String>,
}

pub fn check(app: &tauri::AppHandle, manual: bool) -> Result<EngineStatus, String> {
    let settings = crate::settings::load(app)?;
    let path = crate::yt_dlp::engine_path().ok();
    let path_display = path
        .as_ref()
        .map(|value| value.to_string_lossy().to_string());
    let Some(path) = path else {
        return Ok(EngineStatus {
            status: "missing".into(),
            current_version: None,
            engine_path: None,
            latest_version: None,
            upgrade_command: None,
        });
    };
    let current = local_version(&path)?;
    let base = EngineStatus {
        status: "ok".into(),
        current_version: Some(current.clone()),
        engine_path: path_display,
        latest_version: settings.last_ytdlp_latest.clone(),
        upgrade_command: Some(upgrade_command(&path)),
    };
    let now = unix_now();
    if !manual
        && settings
            .last_ytdlp_check
            .is_some_and(|checked| now.saturating_sub(checked) < 86_400)
    {
        let status = if base
            .latest_version
            .as_deref()
            .is_some_and(|latest| is_newer(latest, &current))
        {
            "outdated"
        } else {
            "skipped"
        };
        return Ok(EngineStatus {
            status: status.into(),
            ..base
        });
    }
    let latest = match latest_version(&settings) {
        Ok(value) => value,
        Err(error) => {
            // 网络失败不阻塞本地信息展示，也不写节流时间
            let _ = error;
            let status = if base
                .latest_version
                .as_deref()
                .is_some_and(|latest| is_newer(latest, &current))
            {
                "outdated"
            } else {
                "channel_unavailable"
            };
            return Ok(EngineStatus {
                status: status.into(),
                ..base
            });
        }
    };
    crate::settings::set_ytdlp_check(app, now, latest.clone())?;
    let status = if is_newer(&latest, &current) {
        "outdated"
    } else {
        "ok"
    };
    Ok(EngineStatus {
        status: status.into(),
        latest_version: Some(latest),
        ..base
    })
}

fn upgrade_command(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if text.starts_with("/opt/homebrew/") || text.starts_with("/usr/local/") {
        "brew upgrade yt-dlp".into()
    } else {
        "yt-dlp -U".into()
    }
}

fn local_version(path: &std::path::Path) -> Result<String, String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .map_err(|error| format!("无法执行 yt-dlp：{error}"))?;
    if !output.status.success() {
        return Err("yt-dlp --version 执行失败".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn latest_version(settings: &AppSettings) -> Result<String, String> {
    let response = crate::network::client(settings, true)?
        .get("https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest")
        .header("User-Agent", "GetTube")
        .send()
        .map_err(|error| format!("检查 yt-dlp 版本失败：{error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("发布渠道不可用".into());
    }
    let value: serde_json::Value = response
        .error_for_status()
        .map_err(|error| format!("检查 yt-dlp 版本失败：{error}"))?
        .json()
        .map_err(|error| format!("版本响应格式无效：{error}"))?;
    value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "版本响应缺少 tag_name".to_string())
}

/// calver（YYYY.MM.DD）逐段数值比较：latest 是否严格新于 current
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |text: &str| -> Vec<u32> {
        text.split('.')
            .map(|part| part.trim().parse::<u32>().unwrap_or(0))
            .collect()
    };
    let (left, right) = (parse(latest), parse(current));
    for index in 0..left.len().max(right.len()) {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    false
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 清空下载归档，返回清除的记录条数（需求 v4 F2）
pub fn clear_archive(app: &tauri::AppHandle) -> Result<u64, String> {
    let path = crate::yt_dlp::archive_path(app)?;
    if !path.exists() {
        return Ok(0);
    }
    let content =
        std::fs::read_to_string(&path).map_err(|error| format!("无法读取归档：{error}"))?;
    let count = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count() as u64;
    std::fs::remove_file(&path).map_err(|error| format!("无法清空归档：{error}"))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_calver_versions() {
        assert!(is_newer("2026.08.04", "2026.07.04"));
        assert!(is_newer("2027.01.01", "2026.12.31"));
        assert!(!is_newer("2026.07.04", "2026.07.04"));
        assert!(!is_newer("2026.07.04", "2026.08.04"));
        assert!(is_newer("2026.07.04.232006", "2026.07.04"));
    }

    #[test]
    fn chooses_upgrade_command_from_engine_path() {
        assert_eq!(
            upgrade_command(std::path::Path::new("/opt/homebrew/bin/yt-dlp")),
            "brew upgrade yt-dlp"
        );
        assert_eq!(
            upgrade_command(std::path::Path::new("/usr/local/bin/yt-dlp")),
            "brew upgrade yt-dlp"
        );
        assert_eq!(
            upgrade_command(std::path::Path::new("/Users/test/.local/bin/yt-dlp")),
            "yt-dlp -U"
        );
    }
}
