use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Youtube,
    Bilibili,
    Douyin,
    Twitter,
    Hongguo,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaFormat {
    pub id: String,
    pub label: String,
    pub extension: Option<String>,
    pub estimated_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub id: String,
    pub url: String,
    pub platform: Platform,
    pub title: String,
    pub author: Option<String>,
    pub thumbnail: Option<String>,
    pub duration: Option<f64>,
    pub formats: Vec<MediaFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadTask {
    pub id: String,
    pub url: String,
    pub title: String,
    pub platform: Platform,
    pub status: String,
    pub progress: f64,
    pub speed: Option<String>,
    pub eta: Option<String>,
    pub output_path: Option<String>,
    pub error: Option<String>,
    pub batch_id: Option<String>,
    pub batch_name: Option<String>,
    #[serde(default)]
    pub media_details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListEntry {
    pub id: String,
    pub title: Option<String>,
    pub duration: Option<f64>,
    pub view_count: Option<u64>,
    pub source: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListInfo {
    #[serde(default)]
    pub platform: Option<Platform>,
    pub title: Option<String>,
    pub uploader: Option<String>,
    pub entries: Vec<ListEntry>,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    None,
    System,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxySettings {
    pub mode: ProxyMode,
    pub url: String,
    pub youtube_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub download_dir: String,
    pub concurrent_downloads: u8,
    pub default_quality: String,
    pub filename_template: String,
    pub proxy: ProxySettings,
    #[serde(default)]
    pub cookies: CookieSettings,
    #[serde(default)]
    pub last_version_check: Option<i64>,
    #[serde(default)]
    pub subtitle_download: bool,
    #[serde(default = "default_true")]
    pub danmaku_download: bool,
    #[serde(default = "default_true")]
    pub embed_media_meta: bool,
    #[serde(default = "default_true")]
    pub download_archive: bool,
    #[serde(default)]
    pub last_ytdlp_check: Option<i64>,
    #[serde(default)]
    pub last_ytdlp_latest: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CookieSettings {
    #[serde(default)]
    pub youtube: PlatformCookieSettings,
    #[serde(default)]
    pub bilibili: PlatformCookieSettings,
    #[serde(default)]
    pub douyin: PlatformCookieSettings,
    #[serde(default)]
    pub twitter: PlatformCookieSettings,
    #[serde(skip)]
    pub unsupported: PlatformCookieSettings,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCookieSettings {
    pub file_path: String,
    pub source: CookieSource,
    pub imported_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CookieSource {
    #[default]
    None,
    File,
    Managed,
}

impl<'de> Deserialize<'de> for PlatformCookieSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(path) = value.as_str() {
            return Ok(Self {
                file_path: path.to_string(),
                source: if path.is_empty() {
                    CookieSource::None
                } else {
                    CookieSource::File
                },
                imported_at: None,
            });
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Current {
            #[serde(default)]
            file_path: String,
            #[serde(default)]
            source: CookieSource,
            #[serde(default)]
            imported_at: Option<i64>,
        }
        let current: Current = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            file_path: current.file_path,
            source: current.source,
            imported_at: current.imported_at,
        })
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            download_dir: String::new(),
            concurrent_downloads: 2,
            default_quality: "best".into(),
            filename_template: "%(title)s [%(id)s].%(ext)s".into(),
            proxy: ProxySettings {
                mode: ProxyMode::System,
                url: String::new(),
                youtube_only: true,
            },
            cookies: CookieSettings::default(),
            last_version_check: None,
            subtitle_download: false,
            danmaku_download: true,
            embed_media_meta: true,
            download_archive: true,
            last_ytdlp_check: None,
            last_ytdlp_latest: None,
        }
    }
}

impl CookieSettings {
    pub fn get(&self, platform: Platform) -> &PlatformCookieSettings {
        match platform {
            Platform::Youtube => &self.youtube,
            Platform::Bilibili => &self.bilibili,
            Platform::Douyin => &self.douyin,
            Platform::Twitter => &self.twitter,
            Platform::Hongguo | Platform::Unknown => &self.unsupported,
        }
    }

    pub fn get_mut(&mut self, platform: Platform) -> &mut PlatformCookieSettings {
        match platform {
            Platform::Youtube => &mut self.youtube,
            Platform::Bilibili => &mut self.bilibili,
            Platform::Douyin => &mut self.douyin,
            Platform::Twitter => &mut self.twitter,
            Platform::Hongguo | Platform::Unknown => &mut self.unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_cookie_path_strings() {
        let value = serde_json::json!({
            "youtube": "/tmp/youtube.txt",
            "bilibili": "",
            "douyin": ""
        });
        let cookies: CookieSettings = serde_json::from_value(value).unwrap();
        assert_eq!(cookies.youtube.file_path, "/tmp/youtube.txt");
        assert_eq!(cookies.youtube.source, CookieSource::File);
        assert_eq!(cookies.bilibili.source, CookieSource::None);
        assert_eq!(cookies.twitter.source, CookieSource::None);
    }
}
