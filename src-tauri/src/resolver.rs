use crate::models::{AppSettings, Platform};

/// 抖音不再前置解析播放地址：服务端渲染已下线（2026-08 实测 www 与 iesdouyin
/// share 页均不含 play_addr），改由 yt-dlp 携 Cookie 处理，无水印靠格式过滤
/// （见 yt_dlp::format_choices）。此处只做链接归一化：yt-dlp 的抖音提取器只认
/// www.douyin.com/video/{id}，分享短链（v.douyin.com）必须先解析重定向。
pub fn execution_url(
    _app: &tauri::AppHandle,
    source_url: &str,
    platform: Platform,
    settings: &AppSettings,
) -> Result<String, String> {
    match platform {
        Platform::Douyin => normalize_douyin(settings, source_url),
        Platform::Twitter if is_tco(source_url) => {
            let client = crate::network::client(settings, true)?;
            crate::network::resolve_redirects(
                &client,
                source_url,
                &["t.co", "x.com", "twitter.com"],
                None,
            )
            .and_then(|url| {
                let parsed = url::Url::parse(&url).map_err(|_| "X 重定向结果无效")?;
                let host = parsed.host_str().ok_or("X 重定向结果缺少域名")?;
                if host == "x.com"
                    || host.ends_with(".x.com")
                    || host == "twitter.com"
                    || host.ends_with(".twitter.com")
                {
                    Ok(url)
                } else {
                    Err("t.co 最终目标不是 X".into())
                }
            })
        }
        _ => Ok(source_url.to_string()),
    }
}

fn normalize_douyin(settings: &AppSettings, source: &str) -> Result<String, String> {
    if let Some(id) = douyin_video_id(source) {
        return Ok(format!("https://www.douyin.com/video/{id}"));
    }
    let client = crate::network::client(settings, !settings.proxy.youtube_only)?;
    let final_url =
        crate::network::resolve_redirects(&client, source, &["douyin.com", "iesdouyin.com"], None)?;
    if let Some(id) = douyin_video_id(&final_url) {
        return Ok(format!("https://www.douyin.com/video/{id}"));
    }
    if url::Url::parse(&final_url)
        .map(|parsed| parsed.path().contains("/notes/"))
        .unwrap_or(false)
    {
        return Err("抖音图集暂不支持，仅支持视频".into());
    }
    Err("无法从抖音链接中提取视频 ID".into())
}

fn douyin_video_id(value: &str) -> Option<String> {
    let pattern = regex::Regex::new(r"(?:/share)?/video/(\d+)|[?&](?:modal_id|mode_id)=(\d+)")
        .expect("valid douyin id regex");
    pattern
        .captures(value)
        .and_then(|caps| caps.get(1).or_else(|| caps.get(2)))
        .map(|m| m.as_str().to_string())
}

fn is_tco(value: &str) -> bool {
    url::Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.eq_ignore_ascii_case("t.co")))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ids_from_all_douyin_url_shapes() {
        assert_eq!(
            douyin_video_id("https://www.douyin.com/video/7518178314357673225?previous_page=app")
                .as_deref(),
            Some("7518178314357673225")
        );
        assert_eq!(
            douyin_video_id("https://www.iesdouyin.com/share/video/7367177915424918821/?mid=1")
                .as_deref(),
            Some("7367177915424918821")
        );
        assert_eq!(
            douyin_video_id("https://www.douyin.com/?modal_id=7123456789012345678").as_deref(),
            Some("7123456789012345678")
        );
        assert_eq!(douyin_video_id("https://v.douyin.com/iAbCdEf/"), None);
    }
}
