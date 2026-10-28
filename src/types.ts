export type Platform = "youtube" | "bilibili" | "douyin" | "twitter" | "hongguo" | "unknown";
export type TaskStatus =
  | "probing"
  | "queued"
  | "downloading"
  | "processing"
  | "completed"
  | "failed"
  | "cancelled"
  | "auth_required";

export interface MediaInfo {
  id: string;
  url: string;
  platform: Platform;
  title: string;
  author?: string;
  thumbnail?: string;
  duration?: number;
  formats: MediaFormat[];
}

export interface MediaFormat {
  id: string;
  label: string;
  extension?: string;
  estimatedSize?: number;
}

export interface DownloadTask {
  id: string;
  url: string;
  title: string;
  platform: Platform;
  status: TaskStatus;
  progress: number;
  speed?: string;
  eta?: string;
  outputPath?: string;
  error?: string;
  batchId?: string;
  batchName?: string;
  mediaDetails?: { engine?: string; seriesId?: string; episodeId?: string; episodeIndex?: number; requestedQuality?: string; actualQuality?: string; duration?: number; sha256?: string; reused?: boolean };
}

export interface ListEntry {
  id: string;
  title?: string;
  duration?: number;
  viewCount?: number;
  source?: string;
  url: string;
}

export interface ListInfo {
  platform?: Platform;
  title?: string;
  uploader?: string;
  entries: ListEntry[];
  total: number;
}

export interface BatchItem {
  url: string;
  title: string;
}

export type ProxyMode = "none" | "system" | "manual";

export interface ProxySettings {
  mode: ProxyMode;
  url: string;
  youtubeOnly: boolean;
}

export interface AppSettings {
  downloadDir: string;
  concurrentDownloads: number;
  defaultQuality: string;
  filenameTemplate: string;
  proxy: ProxySettings;
  cookies: Record<CookiePlatform, PlatformCookieSettings>;
  lastVersionCheck?: number;
  subtitleDownload: boolean;
  danmakuDownload: boolean;
  embedMediaMeta: boolean;
  downloadArchive: boolean;
  lastYtdlpCheck?: number;
  lastYtdlpLatest?: string;
}

export type CookiePlatform = "youtube" | "bilibili" | "douyin" | "twitter";
export type CookieSource = "none" | "file" | "managed";
export interface PlatformCookieSettings { filePath: string; source: CookieSource; importedAt?: number; }
export type CookieDrafts = Record<CookiePlatform, string>;

export interface UpdateResult {
  status: "available" | "latest" | "skipped" | "channel_unconfigured" | "channel_unavailable";
  currentVersion: string;
  latestVersion?: string;
  summary?: string;
  releaseUrl?: string;
}

export interface EngineStatus {
  status: "ok" | "outdated" | "missing" | "skipped" | "channel_unavailable";
  currentVersion?: string;
  enginePath?: string;
  latestVersion?: string;
  upgradeCommand?: string;
}
