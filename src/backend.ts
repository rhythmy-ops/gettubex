import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppSettings, BatchItem, CookieDrafts, CookiePlatform, DownloadTask, EngineStatus, ListInfo, MediaInfo, UpdateResult } from "./types";

function inTauri(): boolean {
  return "__TAURI_INTERNALS__" in window;
}

export async function probeMedia(url: string): Promise<MediaInfo> {
  if (!inTauri()) {
    throw new Error("媒体解析需要在 GetTube 桌面应用中运行");
  }
  return invoke<MediaInfo>("probe_media", { url });
}

export async function probeList(url: string, limit?: number): Promise<ListInfo> {
  if (!inTauri()) {
    throw new Error("列表解析需要在 GetTube 桌面应用中运行");
  }
  return invoke<ListInfo>("probe_list", { url, ...(limit === undefined ? {} : { limit }) });
}

export async function createDownload(
  url: string,
  formatId: string,
  title: string,
): Promise<DownloadTask> {
  if (!inTauri()) {
    throw new Error("下载功能需要在 GetTube 桌面应用中运行");
  }
  return invoke<DownloadTask>("create_download", { url, formatId, title });
}

export async function createDownloads(
  items: BatchItem[],
  formatId: string,
  folder: string,
): Promise<DownloadTask[]> {
  return invoke<DownloadTask[]>("create_downloads", { items, formatId, folder });
}

export async function listTasks(): Promise<DownloadTask[]> {
  if (!inTauri()) return [];
  return invoke<DownloadTask[]>("list_tasks");
}

/** history=清除已完成（历史页）；ended=清除完成/失败/取消（任务页） */
export async function clearTasks(scope: "history" | "ended"): Promise<number> {
  return invoke<number>("clear_tasks", { scope });
}

export async function cancelDownload(id: string): Promise<void> {
  await invoke("cancel_download", { id });
}

export async function cancelBatch(batchId: string): Promise<void> {
  await invoke("cancel_batch", { batchId });
}

export async function retryDownload(id: string): Promise<DownloadTask> {
  return invoke<DownloadTask>("retry_download", { id });
}

export async function onTaskProgress(handler: (task: DownloadTask) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => undefined;
  return listen<DownloadTask>("task-progress", (event) => handler(event.payload));
}

export async function loadSettings(): Promise<AppSettings> {
  if (!inTauri()) return defaultSettings;
  return invoke<AppSettings>("load_settings");
}

export async function saveSettings(settings: AppSettings, cookieDrafts: CookieDrafts): Promise<AppSettings> {
  if (!inTauri()) {
    localStorage.setItem("gettube-settings", JSON.stringify(settings));
    return settings;
  }
  return invoke<AppSettings>("save_settings", { settings, cookieDrafts });
}

export async function clearManagedCookie(platform: CookiePlatform): Promise<AppSettings> {
  return invoke<AppSettings>("clear_managed_cookie", { platform });
}

export async function checkUpdates(manual: boolean): Promise<UpdateResult> {
  if (!inTauri()) return { status: "channel_unconfigured", currentVersion: "dev" };
  return invoke<UpdateResult>("check_updates", { manual });
}

export async function checkYtdlp(manual: boolean): Promise<EngineStatus> {
  if (!inTauri()) return { status: "skipped" };
  return invoke<EngineStatus>("check_ytdlp", { manual });
}

export async function clearArchive(): Promise<number> {
  return invoke<number>("clear_archive");
}

export async function openReleaseUrl(url: string): Promise<void> {
  await invoke("open_release_url", { url });
}

export async function appVersion(): Promise<string> {
  if (!inTauri()) return "dev";
  return invoke<string>("app_version");
}

export const defaultSettings: AppSettings = {
  downloadDir: "",
  concurrentDownloads: 2,
  defaultQuality: "best",
  filenameTemplate: "%(title)s [%(id)s].%(ext)s",
  proxy: { mode: "system", url: "", youtubeOnly: true },
  cookies: {
    youtube: { filePath: "", source: "none" },
    bilibili: { filePath: "", source: "none" },
    douyin: { filePath: "", source: "none" },
    twitter: { filePath: "", source: "none" },
  },
  subtitleDownload: false,
  danmakuDownload: true,
  embedMediaMeta: true,
  downloadArchive: true,
};
