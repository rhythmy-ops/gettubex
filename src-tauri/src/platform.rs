use crate::models::Platform;
use url::Url;

pub fn detect(input: &str) -> Result<Platform, String> {
    let parsed = Url::parse(input).map_err(|_| "链接格式无效".to_string())?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("只支持 HTTP 或 HTTPS 链接".into());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "链接缺少域名".to_string())?;
    if domain_matches(host, &["youtube.com", "youtu.be"]) {
        Ok(Platform::Youtube)
    } else if domain_matches(host, &["bilibili.com", "b23.tv"]) {
        Ok(Platform::Bilibili)
    } else if domain_matches(host, &["douyin.com", "iesdouyin.com"]) {
        Ok(Platform::Douyin)
    } else if domain_matches(host, &["x.com", "twitter.com", "t.co"]) {
        Ok(Platform::Twitter)
    } else {
        Err("当前仅支持 YouTube、哔哩哔哩、抖音和 X".into())
    }
}

fn domain_matches(host: &str, allowed: &[&str]) -> bool {
    allowed
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_lookalike_domain() {
        assert!(detect("https://youtube.com.example.test/watch").is_err());
    }
}
