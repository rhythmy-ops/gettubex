use crate::models::{AppSettings, CookieSource, Platform};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tauri::Manager;

#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CookieDrafts {
    #[serde(default)]
    pub youtube: String,
    #[serde(default)]
    pub bilibili: String,
    #[serde(default)]
    pub douyin: String,
    #[serde(default)]
    pub twitter: String,
}

impl CookieDrafts {
    fn get(&self, platform: Platform) -> &str {
        match platform {
            Platform::Youtube => &self.youtube,
            Platform::Bilibili => &self.bilibili,
            Platform::Douyin => &self.douyin,
            Platform::Twitter => &self.twitter,
        }
    }
}

pub fn apply_drafts(
    app: &tauri::AppHandle,
    settings: &mut AppSettings,
    drafts: &CookieDrafts,
) -> Result<(), String> {
    for platform in all_platforms() {
        let draft = drafts.get(platform);
        if draft.trim().is_empty() {
            normalize_source(app, settings, platform)?;
            continue;
        }
        validate_netscape(draft)?;
        write_managed(app, platform, draft)?;
        let entry = settings.cookies.get_mut(platform);
        entry.source = CookieSource::Managed;
        entry.imported_at = Some(unix_now());
    }
    Ok(())
}

pub fn clear_managed(
    app: &tauri::AppHandle,
    settings: &mut AppSettings,
    platform: Platform,
) -> Result<(), String> {
    let managed = managed_path(app, platform)?;
    if managed.exists() {
        fs::remove_file(&managed).map_err(|error| format!("无法清除托管 Cookie：{error}"))?;
    }
    let entry = settings.cookies.get_mut(platform);
    entry.imported_at = None;
    entry.source =
        if !entry.file_path.trim().is_empty() && std::path::Path::new(&entry.file_path).is_file() {
            CookieSource::File
        } else {
            CookieSource::None
        };
    Ok(())
}

pub fn effective_path(
    app: &tauri::AppHandle,
    settings: &AppSettings,
    platform: Platform,
) -> Result<Option<PathBuf>, String> {
    let entry = settings.cookies.get(platform);
    if entry.source == CookieSource::Managed {
        let managed = managed_path(app, platform)?;
        if managed.is_file() {
            return Ok(Some(managed));
        }
    }
    if !entry.file_path.trim().is_empty() {
        let path = PathBuf::from(&entry.file_path);
        if path.is_file() {
            return Ok(Some(path));
        }
        return Err(format!(
            "{} Cookie 文件不存在或不可读取",
            platform_name(platform)
        ));
    }
    if entry.source == CookieSource::Managed {
        return Err(format!(
            "{} 托管 Cookie 已丢失，请重新导入",
            platform_name(platform)
        ));
    }
    Ok(None)
}

enum ParsedLine {
    Ignored,
    Valid,
    Malformed(&'static str),
}

fn parse_cookie_line(line: &str) -> ParsedLine {
    let line = line.trim_end_matches('\r');
    if line.trim().is_empty() || (line.starts_with('#') && !line.starts_with("#HttpOnly_")) {
        return ParsedLine::Ignored;
    }
    let normalized = line.strip_prefix("#HttpOnly_").unwrap_or(line);
    let fields: Vec<_> = normalized.split('\t').collect();
    if fields.len() != 7 {
        return ParsedLine::Malformed("不是 7 个 TAB 分隔字段");
    }
    if !matches!(fields[1], "TRUE" | "FALSE") || !matches!(fields[3], "TRUE" | "FALSE") {
        return ParsedLine::Malformed("的布尔字段必须为 TRUE/FALSE");
    }
    if fields[4].parse::<i64>().is_err() {
        return ParsedLine::Malformed("的过期时间必须为整数");
    }
    if fields[5].is_empty() {
        // 部分导出工具会产生空名称的退化行，携带不了任何数据，忽略
        return ParsedLine::Ignored;
    }
    ParsedLine::Valid
}

pub fn validate_netscape(content: &str) -> Result<(), String> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Err("检测到 JSON；请粘贴 Netscape cookies.txt 内容".into());
    }
    if !trimmed.contains('\t') && trimmed.contains('=') && trimmed.contains(';') {
        return Err("检测到 Cookie Header；请导出 Netscape cookies.txt 格式".into());
    }
    let mut valid = 0usize;
    for (index, line) in content.lines().enumerate() {
        match parse_cookie_line(line) {
            ParsedLine::Ignored => continue,
            ParsedLine::Malformed(reason) => {
                return Err(format!("Cookie 第 {} 行{}", index + 1, reason));
            }
            ParsedLine::Valid => valid += 1,
        }
    }
    if valid == 0 {
        return Err("Cookie 内容中没有有效记录".into());
    }
    Ok(())
}

fn normalize_source(
    app: &tauri::AppHandle,
    settings: &mut AppSettings,
    platform: Platform,
) -> Result<(), String> {
    let managed_exists = managed_path(app, platform)?.is_file();
    let entry = settings.cookies.get_mut(platform);
    let file_valid =
        !entry.file_path.trim().is_empty() && std::path::Path::new(&entry.file_path).is_file();
    entry.source = normalized_source(entry.source, managed_exists, file_valid);
    Ok(())
}

fn normalized_source(
    current: CookieSource,
    managed_exists: bool,
    file_valid: bool,
) -> CookieSource {
    if current == CookieSource::Managed && managed_exists {
        CookieSource::Managed
    } else if file_valid {
        CookieSource::File
    } else {
        CookieSource::None
    }
}

fn write_managed(app: &tauri::AppHandle, platform: Platform, content: &str) -> Result<(), String> {
    let target = managed_path(app, platform)?;
    let parent = target.parent().ok_or("Cookie 目录无效")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建 Cookie 目录：{error}"))?;
    let temporary = target.with_extension("txt.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("无法写入 Cookie：{error}"))?;
    file.write_all(content.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("无法写入 Cookie：{error}"))?;
    fs::rename(&temporary, &target).map_err(|error| format!("无法保存 Cookie：{error}"))
}

fn managed_path(app: &tauri::AppHandle, platform: Platform) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| {
            dir.join("cookies")
                .join(format!("{}.txt", platform_key(platform)))
        })
        .map_err(|error| error.to_string())
}

fn all_platforms() -> [Platform; 4] {
    [
        Platform::Youtube,
        Platform::Bilibili,
        Platform::Douyin,
        Platform::Twitter,
    ]
}
fn platform_key(platform: Platform) -> &'static str {
    match platform {
        Platform::Youtube => "youtube",
        Platform::Bilibili => "bilibili",
        Platform::Douyin => "douyin",
        Platform::Twitter => "twitter",
    }
}
fn platform_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Youtube => "YouTube",
        Platform::Bilibili => "哔哩哔哩",
        Platform::Douyin => "抖音",
        Platform::Twitter => "X",
    }
}
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_http_only_netscape_cookie() {
        assert!(validate_netscape("# Netscape HTTP Cookie File\n#HttpOnly_.x.com\tTRUE\t/\tTRUE\t1999999999\tauth_token\tsecret\n").is_ok());
    }
    #[test]
    fn rejects_cookie_header_and_json() {
        assert!(validate_netscape("a=b; c=d")
            .unwrap_err()
            .contains("Header"));
        assert!(validate_netscape("{\"a\":\"b\"}")
            .unwrap_err()
            .contains("JSON"));
    }

    #[test]
    fn degenerate_empty_name_line_is_ignored() {
        // 导出工具常产生空名称退化行，不应导致整个文件校验失败
        let content = ".x.com\tTRUE\t/\tTRUE\t1999999999\tauth_token\tsecret\n.douyin.com\tTRUE\t/\tFALSE\t0\t\t\n";
        assert!(validate_netscape(content).is_ok());
    }

    #[test]
    fn malformed_line_still_fails_import_validation() {
        assert!(validate_netscape("broken line\n").is_err());
    }

    #[test]
    fn blank_draft_preserves_existing_managed_source() {
        assert_eq!(
            normalized_source(CookieSource::Managed, true, true),
            CookieSource::Managed
        );
        assert_eq!(
            normalized_source(CookieSource::Managed, false, true),
            CookieSource::File
        );
        assert_eq!(
            normalized_source(CookieSource::Managed, false, false),
            CookieSource::None
        );
    }
}
