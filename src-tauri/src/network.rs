use crate::models::{AppSettings, ProxyMode};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::redirect::Policy;

pub fn client(settings: &AppSettings, use_proxy: bool) -> Result<Client, String> {
    let mut builder = ClientBuilder::new()
        .redirect(Policy::none())
        .timeout(std::time::Duration::from_secs(20));
    if use_proxy {
        if let Some(proxy) = proxy_url(settings)? {
            builder = builder.proxy(reqwest::Proxy::all(&proxy).map_err(|_| "代理地址格式无效")?);
        }
    }
    builder
        .build()
        .map_err(|error| format!("无法初始化网络客户端：{error}"))
}

pub fn proxy_url(settings: &AppSettings) -> Result<Option<String>, String> {
    match settings.proxy.mode {
        ProxyMode::None => Ok(None),
        ProxyMode::System => crate::yt_dlp::system_proxy(),
        ProxyMode::Manual => {
            let parsed = url::Url::parse(&settings.proxy.url).map_err(|_| "代理地址格式无效")?;
            if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
                return Err("不支持的代理协议".into());
            }
            Ok(Some(settings.proxy.url.clone()))
        }
    }
}

pub fn resolve_redirects(
    client: &Client,
    start: &str,
    allowed: &[&str],
    cookie_header: Option<&str>,
) -> Result<String, String> {
    let mut current = url::Url::parse(start).map_err(|_| "短链接格式无效")?;
    for _ in 0..=5 {
        validate_target(&current, allowed)?;
        let mut request = client.get(current.clone()).header("User-Agent", "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148");
        if let Some(cookie) = cookie_header {
            if !cookie.is_empty() {
                request = request.header("Cookie", cookie);
            }
        }
        let response = request
            .send()
            .map_err(|error| format!("短链接请求失败：{error}"))?;
        if !response.status().is_redirection() {
            return Ok(current.to_string());
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .ok_or("重定向缺少 Location")?
            .to_str()
            .map_err(|_| "重定向地址编码无效")?;
        current = current.join(location).map_err(|_| "重定向地址无效")?;
    }
    Err("短链接重定向超过 5 次".into())
}

fn validate_target(url: &url::Url, allowed: &[&str]) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err("短链接只允许 HTTP(S) 跳转".into());
    }
    let host = url.host_str().ok_or("重定向缺少域名")?;
    if host.parse::<std::net::IpAddr>().is_ok() || host.eq_ignore_ascii_case("localhost") {
        return Err("短链接禁止跳转到本机或 IP 地址".into());
    }
    if !allowed
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    {
        return Err("短链接跳转到了未授权域名".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_untrusted_redirect_targets() {
        assert!(
            validate_target(&url::Url::parse("http://127.0.0.1/a").unwrap(), &["x.com"]).is_err()
        );
        assert!(validate_target(
            &url::Url::parse("https://x.com.evil.test/a").unwrap(),
            &["x.com"]
        )
        .is_err());
        assert!(validate_target(
            &url::Url::parse("https://mobile.x.com/a").unwrap(),
            &["x.com"]
        )
        .is_ok());
    }
}
