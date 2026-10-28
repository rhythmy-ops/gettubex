import type { Platform } from "./types";

export const HONGGUO_FORMATS = [
  { id: "auto", label: "自动画质（验证实际分辨率）" },
  { id: "1080p", label: "至少 1080p（不足则失败）" },
  { id: "720p", label: "至少 720p（不足则失败）" },
];

const PLATFORM_HOSTS: Array<[Platform, RegExp]> = [
  ["hongguo", /(^|\.)((hongguoduanju\.com)|(novelquickapp\.com)|(changdunovel\.com))$/i],
  ["youtube", /(^|\.)((youtube\.com)|(youtu\.be))$/i],
  ["bilibili", /(^|\.)((bilibili\.com)|(b23\.tv))$/i],
  ["douyin", /(^|\.)((douyin\.com)|(iesdouyin\.com))$/i],
  ["twitter", /(^|\.)((x\.com)|(twitter\.com)|(t\.co))$/i],
];

export function extractFirstUrl(input: string): string | null {
  const match = input.match(/https?:\/\/[^\s，。；、）)\]}]+/i);
  return match?.[0] ?? null;
}

export function detectPlatform(input: string): Platform {
  const value = extractFirstUrl(input) ?? input.trim();
  try {
    const host = new URL(value).hostname;
    return PLATFORM_HOSTS.find(([, pattern]) => pattern.test(host))?.[0] ?? "unknown";
  } catch {
    return "unknown";
  }
}

function parsed(input: string): URL | null {
  const value = extractFirstUrl(input) ?? input.trim();
  try {
    return new URL(value);
  } catch {
    return null;
  }
}

/** UP 主 / 频道页：进入"最近作品"模式（条数可调，默认 30） */
export function looksLikeChannel(input: string): boolean {
  const url = parsed(input);
  if (!url) return false;
  const path = url.pathname.replace(/\/+$/, "");
  if (/(^|\.)youtube\.com$/i.test(url.hostname)) {
    return /^\/@[^/]+\/videos$/.test(path) || (/^\/channel\/[^/]+/.test(path) && path.endsWith("/videos"));
  }
  if (url.hostname.toLowerCase() === "space.bilibili.com") {
    return /^\/\d+\/video$/.test(path);
  }
  return false;
}

/** 播放列表 / 合集 / 多 P / 收藏夹：进入"列表预览"模式 */
export function looksLikeList(input: string): boolean {
  if (detectPlatform(input) === "hongguo") return true;
  const url = parsed(input);
  if (!url) return false;
  const path = url.pathname.replace(/\/+$/, "");
  if (/(^|\.)youtube\.com$/i.test(url.hostname)) {
    if (path.startsWith("/playlist")) return true;
    // 带 list 参数但无 v 参数是纯列表；带 v 参数仍按单视频处理
    if (url.searchParams.has("list") && !url.searchParams.has("v")) return true;
    return false;
  }
  if (/(^|\.)bilibili\.com$/i.test(url.hostname)) {
    if (url.hostname.toLowerCase() === "space.bilibili.com") {
      return /\/(favlist|lists)(\/|$)/i.test(path) || url.searchParams.has("fid");
    }
    if (path.includes("/favlist") || url.searchParams.has("fid")) return true;
    if (/\/(medialist|list|festival)(\/|$)/i.test(path)) return true;
    // 无 p 参数的 BV 链接视为多 P 列表（单 P 视频会得到 1 条列表，行为一致）
    if (/^\/video\/BV/i.test(path) && !url.searchParams.has("p")) return true;
    return false;
  }
  return false;
}

export const platformLabel: Record<Platform, string> = {
  youtube: "YouTube",
  bilibili: "哔哩哔哩",
  douyin: "抖音",
  twitter: "X",
  hongguo: "红果短剧",
  unknown: "未识别",
};
