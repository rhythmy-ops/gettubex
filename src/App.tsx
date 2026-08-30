import { useEffect, useMemo, useState } from "react";
import {
  CheckCircle2,
  Clock3,
  Download,
  FolderClock,
  Link2,
  ListVideo,
  Settings,
  ShieldCheck,
  SlidersHorizontal,
  Wifi,
  RefreshCw,
  ExternalLink,
  ListChecks,
} from "lucide-react";
import { appVersion, cancelBatch, cancelDownload, checkUpdates, checkYtdlp, clearArchive, clearManagedCookie, clearTasks, createDownload, createDownloads, defaultSettings, listTasks, loadSettings, onTaskProgress, openReleaseUrl, probeList, probeMedia, retryDownload, saveSettings } from "./backend";
import { detectPlatform, extractFirstUrl, looksLikeChannel, looksLikeList, platformLabel } from "./platforms";
import type { AppSettings, CookieDrafts, CookiePlatform, DownloadTask, EngineStatus, ListInfo, MediaInfo, UpdateResult } from "./types";

type Page = "download" | "tasks" | "history" | "settings";

const nav = [
  { id: "download" as const, label: "新建下载", icon: Download },
  { id: "tasks" as const, label: "任务", icon: ListVideo },
  { id: "history" as const, label: "历史", icon: FolderClock },
  { id: "settings" as const, label: "设置", icon: Settings },
];

const ACTIVE_STATUSES = ["queued", "downloading", "processing"];
const RENDER_CAP = 200;
const CHANNEL_LIMITS = [30, 50, 100];

const LIST_FORMATS = [
  { id: "bestvideo*+bestaudio/best", label: "最佳画质（合并音视频）" },
  { id: "audio-mp3", label: "仅音频（MP3）" },
  { id: "audio-m4a", label: "仅音频（M4A）" },
  { id: "audio-opus", label: "仅音频（OPUS）" },
];

function formatDuration(seconds?: number): string {
  if (!seconds || !Number.isFinite(seconds) || seconds <= 0) return "--:--";
  const total = Math.round(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const two = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`;
}

function downloadKeyOf(url: string): string | null {
  try {
    const parsed = new URL(url);
    const v = parsed.searchParams.get("v");
    if (v) return v;
    const bv = parsed.pathname.match(/BV[a-z0-9]+/i);
    if (bv) return bv[0].toUpperCase();
    if (parsed.hostname.includes("youtu.be")) return parsed.pathname.slice(1) || null;
  } catch {
    return null;
  }
  return null;
}

/** D8 去重提示：同源视频已有完成任务时标记"已下载"（仅提示，不阻断） */
function isDownloaded(url: string, tasks: DownloadTask[]): boolean {
  const key = downloadKeyOf(url);
  if (!key) return false;
  return tasks.some((task) => task.status === "completed" && downloadKeyOf(task.url) === key);
}

/** 解析 "3-7,10" 形式的区间表达式为条目下标（0 基） */
function parseRange(text: string, max: number): number[] {
  const indices: number[] = [];
  for (const token of text.split(/[,，]/)) {
    const trimmed = token.trim();
    if (!trimmed) continue;
    const match = trimmed.match(/^(\d+)(?:\s*[-~]\s*(\d+))?$/);
    if (!match) continue;
    const start = Number(match[1]);
    const end = match[2] ? Number(match[2]) : start;
    for (let n = Math.min(start, end); n <= Math.max(start, end); n += 1) {
      if (n >= 1 && n <= max) indices.push(n - 1);
    }
  }
  return indices;
}

/** 入队事件与命令返回可能都带回同一任务，前插时按 id 去重防双份 */
function prependTasks(current: DownloadTask[], incoming: DownloadTask[]): DownloadTask[] {
  const seen = new Set(current.map((task) => task.id));
  return [...incoming.filter((task) => !seen.has(task.id)), ...current];
}

export default function App() {
  const [page, setPage] = useState<Page>("download");
  const [input, setInput] = useState("");
  const [media, setMedia] = useState<MediaInfo | null>(null);
  const [listInfo, setListInfo] = useState<ListInfo | null>(null);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [listFormat, setListFormat] = useState(LIST_FORMATS[0].id);
  const [channelLimit, setChannelLimit] = useState(30);
  const [rangeText, setRangeText] = useState("");
  const [format, setFormat] = useState("best");
  const [tasks, setTasks] = useState<DownloadTask[]>([]);
  const [settings, setSettings] = useState<AppSettings>(defaultSettings);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [update, setUpdate] = useState<UpdateResult | null>(null);
  const [version, setVersion] = useState("读取中…");
  const [engine, setEngine] = useState<EngineStatus | null>(null);

  useEffect(() => {
    loadSettings().then(setSettings).catch(() => setSettings(defaultSettings));
    listTasks().then(setTasks).catch(() => undefined);
    appVersion().then(setVersion).catch(() => setVersion("未知"));
    checkUpdates(false).then((result) => { if (result.status === "available") setUpdate(result); }).catch(() => undefined);
    checkYtdlp(false).then(setEngine).catch(() => undefined);
    let cleanup: () => void = () => {};
    onTaskProgress((updated) => setTasks((current) => {
      const exists = current.some((task) => task.id === updated.id);
      return exists ? current.map((task) => task.id === updated.id ? updated : task) : [updated, ...current];
    })).then((unlisten) => { cleanup = unlisten; });
    return () => cleanup();
  }, []);

  const platform = useMemo(() => detectPlatform(input), [input]);
  const activeTasks = tasks.filter((task) => ACTIVE_STATUSES.includes(task.status));

  function resetListState() {
    setListInfo(null);
    setMedia(null);
    setRangeText("");
  }

  async function loadList(url: string, limit: number) {
    const info = await probeList(url, limit);
    if (info.entries.length === 0) {
      throw new Error("列表为空或无法读取条目");
    }
    setListInfo(info);
    setListFormat(LIST_FORMATS[0].id);
    // D1：小列表默认全选，大列表默认不选
    setSelected(new Set(info.entries.length <= 20 ? info.entries.map((_, index) => index) : []));
    setRangeText("");
  }

  async function inspect() {
    const url = extractFirstUrl(input);
    if (!url || platform === "unknown") {
      setMessage("请输入受支持的 YouTube、哔哩哔哩、抖音或 X 链接");
      return;
    }
    setBusy(true);
    setMessage("");
    const isChannel = looksLikeChannel(url);
    const isList = isChannel || looksLikeList(url);
    try {
      if (isList) {
        // 频道只取所选的近期条目；播放列表保留全部条目，但界面只渲染前 200 条。
        if (isChannel) await loadList(url, channelLimit);
        else {
          const info = await probeList(url);
          if (info.entries.length === 0) throw new Error("列表为空或无法读取条目");
          setListInfo(info);
          setListFormat(LIST_FORMATS[0].id);
          setSelected(new Set(info.entries.length <= 20 ? info.entries.map((_, index) => index) : []));
          setRangeText("");
        }
      } else {
        const result = await probeMedia(url);
        setListInfo(null);
        setMedia(result);
        setFormat(result.formats[0]?.id ?? "best");
      }
    } catch (error) {
      if (isList) {
        // 列表探测失败时回落单视频解析（如带 list 参数的单视频）
        try {
          const result = await probeMedia(url);
          setListInfo(null);
          setMedia(result);
          setFormat(result.formats[0]?.id ?? "best");
          return;
        } catch {
          // 继续抛出原始列表错误
        }
      }
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  async function reloadChannelList(limit: number) {
    const url = extractFirstUrl(input);
    if (!url) return;
    setChannelLimit(limit);
    setBusy(true);
    setMessage("");
    try {
      await loadList(url, limit);
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  async function download() {
    if (!media) return;
    setBusy(true);
    try {
      const task = await createDownload(media.url, format, media.title);
      setTasks((current) => prependTasks(current, [task]));
      setPage("tasks");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  async function downloadList() {
    if (!listInfo) return;
    const items = [...selected].sort((a, b) => a - b)
      .map((index) => listInfo.entries[index])
      .filter(Boolean);
    if (items.length === 0) {
      setMessage("请先勾选要下载的条目");
      return;
    }
    setBusy(true);
    try {
      const folder = listInfo.title || listInfo.uploader || "playlist";
      const created = await createDownloads(
        items.map((entry) => ({ url: entry.url, title: entry.title || entry.id })),
        listFormat,
        folder,
      );
      setTasks((current) => prependTasks(current, created));
      setPage("tasks");
    } catch (error) {
      setMessage(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  }

  function toggleIndex(index: number) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(index)) next.delete(index);
      else next.add(index);
      return next;
    });
  }

  const displayEntries = listInfo ? listInfo.entries.slice(0, RENDER_CAP) : [];

  function selectDisplayed() {
    setSelected((current) => {
      const next = new Set(current);
      displayEntries.forEach((_, index) => next.add(index));
      return next;
    });
  }

  function invertDisplayed() {
    setSelected((current) => {
      const next = new Set(current);
      displayEntries.forEach((_, index) => {
        if (next.has(index)) next.delete(index);
        else next.add(index);
      });
      return next;
    });
  }

  function clearSelection() {
    setSelected(new Set());
  }

  function applyRange() {
    if (!listInfo) return;
    const indices = parseRange(rangeText, listInfo.entries.length);
    if (indices.length === 0) {
      setMessage("区间格式示例：3-7,10");
      return;
    }
    setSelected((current) => {
      const next = new Set(current);
      indices.forEach((index) => next.add(index));
      return next;
    });
  }

  const isChannelUrl = useMemo(() => looksLikeChannel(extractFirstUrl(input) ?? input), [input]);

  /** 清除历史，返回错误或 null；确认交互用页面内联条完成（WKWebView 不支持 window.confirm） */
  async function clearHistory(): Promise<string | null> {
    try {
      await clearTasks("history");
      setTasks((current) => current.filter((task) => task.status !== "completed"));
      return null;
    } catch (error) {
      return error instanceof Error ? error.message : String(error);
    }
  }

  async function clearEndedTasks(): Promise<string | null> {
    try {
      await clearTasks("ended");
      setTasks((current) => current.filter((task) => ACTIVE_STATUSES.includes(task.status)));
      return null;
    } catch (error) {
      return error instanceof Error ? error.message : String(error);
    }
  }

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand"><span className="brand-mark"><Download size={19} /></span><span>GetTube</span></div>
        <nav>{nav.map(({ id, label, icon: Icon }) => (
          <button key={id} className={page === id ? "nav-active" : ""} onClick={() => setPage(id)}>
            <Icon size={18} />{label}{id === "tasks" && activeTasks.length > 0 && <em>{activeTasks.length}</em>}
          </button>
        ))}</nav>
        <div className="sidebar-foot"><ShieldCheck size={16} /><span>文件与凭证仅保存在本机</span></div>
      </aside>

      <main>
        {update?.status === "available" && <div className="update-banner"><div><strong>GetTube {update.latestVersion} 可用</strong><span>{update.summary ?? "查看本次更新内容"}</span></div>{update.releaseUrl && <button className="ghost" onClick={() => openReleaseUrl(update.releaseUrl!)}><ExternalLink size={15} />查看发布页</button>}</div>}
        {page === "download" && (
          <section className="page hero-page">
            <div className="eyebrow">LOCAL MEDIA DOWNLOADER</div>
            <h1>把喜欢的内容，<br /><span>留在自己的设备里。</span></h1>
            <p className="lead">支持 YouTube、哔哩哔哩、抖音和 X，播放列表与 UP 主作品可勾选批量下载。</p>
            <div className="url-card">
              <div className="url-row">
                <Link2 size={21} />
                <textarea aria-label="媒体链接" value={input} onChange={(e) => { setInput(e.target.value); resetListState(); }}
                  placeholder="粘贴视频、播放列表或 UP 主主页链接…" rows={2} />
                <button className="primary" disabled={busy} onClick={inspect}>{busy ? "解析中…" : "解析链接"}</button>
              </div>
              <div className="url-meta">
                <span className={`platform ${platform}`}>{platformLabel[platform]}</span>
                <span>支持分享短链、播放列表与 UP 主主页</span>
                {settings.proxy.mode !== "none" && <span className="proxy-state"><Wifi size={13} />代理策略已启用</span>}
              </div>
            </div>
            {message && <div className="notice">{message}</div>}
            {media && <div className="media-card">
              {media.thumbnail ? <img src={media.thumbnail} alt="" /> : <div className="thumb-placeholder"><ListVideo /></div>}
              <div className="media-info"><small>{platformLabel[media.platform]}</small><h2>{media.title}</h2><p>{media.author ?? "未知作者"}</p>
                <select value={format} onChange={(e) => setFormat(e.target.value)}>
                  {media.formats.map((item) => <option key={item.id} value={item.id}>{item.label}</option>)}
                </select>
              </div>
              <button className="primary" onClick={download} disabled={busy}><Download size={17} />开始下载</button>
            </div>}
            {listInfo && <div className="list-card">
              <div className="list-head">
                <div>
                  <div className="eyebrow">{isChannelUrl ? "CHANNEL" : "PLAYLIST"}</div>
                  <h2>{listInfo.title || listInfo.uploader || "列表"}</h2>
                  <span>共 {listInfo.total} 条{listInfo.entries.length < listInfo.total && ` · 已加载 ${listInfo.entries.length} 条`}</span>
                </div>
                {isChannelUrl && (
                  <label className="limit-select">条数
                    <select value={channelLimit} onChange={(e) => reloadChannelList(Number(e.target.value))}>
                      {CHANNEL_LIMITS.map((n) => <option key={n} value={n}>最近 {n}</option>)}
                    </select>
                  </label>
                )}
              </div>
              <div className="list-toolbar">
                <button className="ghost" onClick={selectDisplayed}>全选</button>
                <button className="ghost" onClick={invertDisplayed}>反选</button>
                <button className="ghost" onClick={clearSelection}>清空</button>
                <input className="range-input" value={rangeText} placeholder="区间，如 3-7,10" onChange={(e) => setRangeText(e.target.value)} />
                <button className="ghost" onClick={applyRange}>按区间勾选</button>
              </div>
              <div className="list-rows">
                {displayEntries.map((entry, index) => (
                  <label key={`${entry.id}-${index}`} className={`list-row${selected.has(index) ? " picked" : ""}${isDownloaded(entry.url, tasks) ? " done" : ""}`}>
                    <input type="checkbox" checked={selected.has(index)} onChange={() => toggleIndex(index)} />
                    <span className="idx">{index + 1}</span>
                    <span className="row-title">{entry.title || entry.id}</span>
                    <span className="row-duration">{formatDuration(entry.duration)}</span>
                    {entry.viewCount !== undefined && <span className="row-views">{entry.viewCount.toLocaleString()} 次播放</span>}
                    {entry.source && <span className="row-source">{entry.source}</span>}
                    {isDownloaded(entry.url, tasks) && <span className="badge-done">已下载</span>}
                  </label>
                ))}
              </div>
              <div className="list-foot">
                <div className="format-pick">
                  <SlidersHorizontal size={15} />
                  <select value={listFormat} onChange={(e) => setListFormat(e.target.value)}>
                    {LIST_FORMATS.map((item) => <option key={item.id} value={item.id}>{item.label}</option>)}
                  </select>
                </div>
                <button className="primary" onClick={downloadList} disabled={busy || selected.size === 0}>
                  <ListChecks size={17} />下载选中 {selected.size > 0 ? `（${selected.size}）` : ""}
                </button>
              </div>
            </div>}
            <div className="feature-grid">
              <article><SlidersHorizontal /><h3>自由选择格式</h3><p>按内容选择画质与音频格式。</p></article>
              <article><Wifi /><h3>按平台使用代理</h3><p>YouTube和X可走代理，国内平台保持直连。</p></article>
              <article><ListChecks /><h3>列表与 UP 主批量</h3><p>勾选条目排队下载，自动整理子文件夹。</p></article>
            </div>
          </section>
        )}
        {page === "tasks" && <TaskPage tasks={tasks} onCancel={(id) => cancelDownload(id)} onRetry={async (id) => { const task = await retryDownload(id); setTasks((current) => prependTasks(current, [task])); }} onCancelBatch={(batchId) => cancelBatch(batchId)} onClearEnded={clearEndedTasks} />}
        {page === "history" && <HistoryPage tasks={tasks.filter((task) => task.status === "completed")} onClear={clearHistory} />}
        {page === "settings" && <SettingsPage value={settings} onChange={setSettings} version={version} engine={engine} onRefreshEngine={() => { checkYtdlp(true).then(setEngine).catch(() => undefined); }} />}
      </main>
    </div>
  );
}

function TaskRow({ task, onCancel, onRetry }: { task: DownloadTask; onCancel: (id: string) => void; onRetry: (id: string) => void }) {
  return <article>
    <div className="task-icon"><Download /></div><div className="task-main"><strong>{task.title}</strong><span>{platformLabel[task.platform]} · {displayStatus(task)}{task.speed ? ` · ${task.speed}` : ""}{task.eta ? ` · 剩余 ${task.eta}` : ""}</span>{task.error && <span className="task-error">{task.error}</span>}<div className="progress"><i style={{ width: `${task.progress}%` }} /></div></div><b>{Math.round(task.progress)}%</b>
    {ACTIVE_STATUSES.includes(task.status) && <button className="ghost" onClick={() => onCancel(task.id)}>取消</button>}
    {["failed", "cancelled"].includes(task.status) && <button className="ghost" onClick={() => onRetry(task.id)}>重试</button>}
  </article>;
}

function statusText(status: string): string {
  const map: Record<string, string> = {
    queued: "排队中",
    downloading: "下载中",
    processing: "处理中",
    completed: "已完成",
    failed: "失败",
    cancelled: "已取消",
    probing: "解析中",
    auth_required: "需要登录",
  };
  return map[status] ?? status;
}

/** 完成但无输出文件 = 被下载归档跳过的条目（需求 v4 F2），显式标注避免困惑 */
function displayStatus(task: DownloadTask): string {
  if (task.status === "completed" && !task.outputPath) return "已跳过（归档）";
  return statusText(task.status);
}

function TaskPage({ tasks, onCancel, onRetry, onCancelBatch, onClearEnded }: { tasks: DownloadTask[]; onCancel: (id: string) => void; onRetry: (id: string) => void; onCancelBatch: (batchId: string) => void; onClearEnded: () => Promise<string | null> }) {
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState("");
  const groups = new Map<string, DownloadTask[]>();
  const singles: DownloadTask[] = [];
  for (const task of tasks) {
    if (task.batchId) {
      const group = groups.get(task.batchId) ?? [];
      group.push(task);
      groups.set(task.batchId, group);
    } else {
      singles.push(task);
    }
  }
  const endedCount = tasks.filter((task) => !ACTIVE_STATUSES.includes(task.status)).length;
  async function runClear() {
    const err = await onClearEnded();
    if (err) setError(err);
    else {
      setError("");
      setConfirming(false);
    }
  }
  return <section className="page"><div className="page-title"><div><div className="eyebrow">TASK QUEUE</div><h1>下载任务</h1></div><div className="title-actions">
    <span>{tasks.length} 个任务</span>
    {endedCount > 0 && (confirming
      ? <span className="confirm-strip">{error ? <em className="strip-error">清除失败：{error}</em> : `清除 ${endedCount} 条已结束任务？`}
          <button className="danger-ghost" onClick={runClear}>确认</button>
          <button className="ghost" onClick={() => { setConfirming(false); setError(""); }}>取消</button>
        </span>
      : <button className="ghost" onClick={() => setConfirming(true)}>清除已结束</button>)}
  </div></div>
    {tasks.length === 0 ? <EmptyPage icon={<Clock3 />} title="队列是空的" text="粘贴一个链接开始第一次下载。" /> : <div className="task-list">
      {[...groups.entries()].map(([batchId, group]) => {
        const active = group.filter((task) => ACTIVE_STATUSES.includes(task.status)).length;
        const name = group[0]?.batchName || "批量任务";
        return <div className="batch" key={batchId}>
          <div className="batch-header">
            <strong>{name}</strong>
            <span>{active > 0 ? `${active} 进行中 · ` : ""}{group.length} 项</span>
            {active > 0 && <button className="ghost" onClick={() => onCancelBatch(batchId)}>整批取消</button>}
          </div>
          {group.map((task) => <TaskRow key={task.id} task={task} onCancel={onCancel} onRetry={onRetry} />)}
        </div>;
      })}
      {singles.map((task) => <TaskRow key={task.id} task={task} onCancel={onCancel} onRetry={onRetry} />)}
    </div>}
  </section>;
}

function HistoryPage({ tasks, onClear }: { tasks: DownloadTask[]; onClear: () => Promise<string | null> }) {
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState("");
  async function runClear() {
    const err = await onClear();
    if (err) setError(err);
    else {
      setError("");
      setConfirming(false);
    }
  }
  return <section className="page"><div className="page-title"><div><div className="eyebrow">LOCAL HISTORY</div><h1>下载历史</h1></div><div className="title-actions">
    <span>{tasks.length} 项</span>
    {tasks.length > 0 && (confirming
      ? <span className="confirm-strip">{error ? <em className="strip-error">清除失败：{error}</em> : `确认清除全部 ${tasks.length} 条历史？不可恢复`}
          <button className="danger-ghost" onClick={runClear}>确认</button>
          <button className="ghost" onClick={() => { setConfirming(false); setError(""); }}>取消</button>
        </span>
      : <button className="ghost" onClick={() => setConfirming(true)}>清除历史</button>)}
  </div></div>
    {tasks.length === 0 ? <EmptyPage icon={<FolderClock />} title="还没有下载历史" text="完成的任务会安全地记录在本机。" /> : <div className="task-list">{tasks.map((task) => <article key={task.id}><div className="task-icon"><CheckCircle2 /></div><div className="task-main"><strong>{task.title}</strong><span>{platformLabel[task.platform]} · {displayStatus(task)}{task.outputPath ? ` · ${task.outputPath}` : ""}{task.batchName ? ` · ${task.batchName}` : ""}</span></div></article>)}</div>}
  </section>;
}

function EmptyPage({ icon, title, text }: { icon: React.ReactNode; title: string; text: string }) {
  return <div className="empty"><div>{icon}</div><h2>{title}</h2><p>{text}</p></div>;
}

function SettingsPage({ value, onChange, version, engine, onRefreshEngine }: { value: AppSettings; onChange: (v: AppSettings) => void; version: string; engine: EngineStatus | null; onRefreshEngine: () => void }) {
  const [feedback, setFeedback] = useState<{ ok: boolean; text: string } | null>(null);
  const [confirmClearArchive, setConfirmClearArchive] = useState(false);
  const emptyDrafts = (): CookieDrafts => ({ youtube: "", bilibili: "", douyin: "", twitter: "" });
  const [drafts, setDrafts] = useState<CookieDrafts>(emptyDrafts);
  const [confirmClearCookie, setConfirmClearCookie] = useState<CookiePlatform | null>(null);
  const [updateResult, setUpdateResult] = useState<UpdateResult | null>(null);
  async function runClearArchive() {
    setConfirmClearArchive(false);
    try {
      const count = await clearArchive();
      setFeedback({ ok: true, text: count > 0 ? `已清空 ${count} 条归档记录` : "归档本就为空" });
    } catch (error) {
      setFeedback({ ok: false, text: error instanceof Error ? error.message : String(error) });
    }
  }
  const updateProxy = (patch: Partial<AppSettings["proxy"]>) => onChange({ ...value, proxy: { ...value.proxy, ...patch } });
  async function save() {
    setFeedback(null);
    try {
      const saved = await saveSettings(value, drafts);
      onChange(saved);
      setDrafts(emptyDrafts());
      setFeedback({ ok: true, text: "设置已保存" });
    } catch (error) {
      setFeedback({ ok: false, text: error instanceof Error ? error.message : String(error) });
    }
  }
  const [countText, setCountText] = useState(String(value.concurrentDownloads));
  useEffect(() => { setCountText(String(value.concurrentDownloads)); }, [value.concurrentDownloads]);
  // 编辑期间显示原始输入（允许暂时为空/非法），仅在解析合法时提交；失焦时归一化回填
  const updateCount = (raw: string) => {
    const digits = raw.replace(/[^\d]/g, "").slice(0, 2);
    setCountText(digits);
    const parsed = Number(digits);
    if (digits !== "" && parsed >= 1) {
      onChange({ ...value, concurrentDownloads: Math.min(5, parsed) });
    }
  };
  const commitCount = () => {
    const parsed = Number(countText);
    const effective = Number.isFinite(parsed) && parsed >= 1 ? Math.min(5, Math.floor(parsed)) : 1;
    if (effective !== value.concurrentDownloads) {
      onChange({ ...value, concurrentDownloads: effective });
    }
    setCountText(String(effective));
  };
  return <section className="page settings-page"><div className="page-title"><div><div className="eyebrow">PREFERENCES</div><h1>设置</h1></div><button className="primary" onClick={save}>保存设置</button></div>
    {feedback && <div className={feedback.ok ? "notice notice-ok" : "notice"}>{feedback.text}</div>}
    <div className="settings-card"><h2>下载</h2><label>保存目录<input value={value.downloadDir} placeholder="默认：系统下载目录/GetTube" onChange={(e) => onChange({ ...value, downloadDir: e.target.value })} /></label>
      <label>同时下载数量<input type="text" inputMode="numeric" value={countText} onChange={(e) => updateCount(e.target.value)} onBlur={commitCount} /></label>
      <label>文件名模板<input value={value.filenameTemplate} placeholder="%(title)s [%(id)s].%(ext)s" onChange={(e) => onChange({ ...value, filenameTemplate: e.target.value })} /></label>
      <label className="checkbox"><input type="checkbox" checked={value.subtitleDownload} onChange={(e) => onChange({ ...value, subtitleDownload: e.target.checked })} />下载字幕（YouTube / B站，中英文）</label>
      <label className="checkbox"><input type="checkbox" checked={value.danmakuDownload} onChange={(e) => onChange({ ...value, danmakuDownload: e.target.checked })} />下载弹幕（B站，保存为 danmaku.xml）</label>
      <label className="checkbox"><input type="checkbox" checked={value.embedMediaMeta} onChange={(e) => onChange({ ...value, embedMediaMeta: e.target.checked })} />嵌入元数据与封面到文件（M4A/MP4 完整封面支持需安装 AtomicParsley）</label>
      <label className="checkbox"><input type="checkbox" checked={value.downloadArchive} onChange={(e) => onChange({ ...value, downloadArchive: e.target.checked })} />下载归档：批量下载自动跳过已下过的视频</label></div>
    <div className="settings-card"><h2>网络与代理</h2><p>代理将同时用于链接解析、媒体、字幕和封面下载。“跟随系统代理”会读取系统网络设置（macOS）或环境变量。</p>
      <label>代理模式<select value={value.proxy.mode} onChange={(e) => updateProxy({ mode: e.target.value as AppSettings["proxy"]["mode"] })}><option value="none">不使用代理</option><option value="system">跟随系统代理</option><option value="manual">手动配置</option></select></label>
      {value.proxy.mode === "manual" && <label>代理地址<input value={value.proxy.url} placeholder="socks5://127.0.0.1:7890" onChange={(e) => updateProxy({ url: e.target.value })} /></label>}
      <label className="checkbox"><input type="checkbox" checked={value.proxy.youtubeOnly} onChange={(e) => updateProxy({ youtubeOnly: e.target.checked })} />仅对 YouTube、X 使用代理</label>
    </div>
    <div className="settings-card"><h2>平台 Cookie</h2><p>可使用本地 Netscape cookies.txt，或粘贴后由 GetTube 托管。空白粘贴框不会删除已有凭证。</p>
      {(["youtube", "bilibili", "douyin", "twitter"] as CookiePlatform[]).map((platform) => <div className="cookie-group" key={platform}>
        <div className="cookie-heading"><strong>{platformLabel[platform]}</strong><span className={`source-pill ${value.cookies[platform].source}`}>{value.cookies[platform].source === "managed" ? `已托管${value.cookies[platform].importedAt ? ` · ${new Date(value.cookies[platform].importedAt! * 1000).toLocaleDateString()}` : ""}` : value.cookies[platform].source === "file" ? "文件" : "未配置"}</span></div>
        <label>Cookie 文件<input value={value.cookies[platform].filePath} placeholder="/Users/you/cookies.txt" onChange={(e) => onChange({ ...value, cookies: { ...value.cookies, [platform]: { ...value.cookies[platform], filePath: e.target.value, source: value.cookies[platform].source === "managed" ? "managed" : e.target.value ? "file" : "none" } } })} /></label>
        <label>粘贴并导入<textarea value={drafts[platform]} placeholder="粘贴 Netscape Cookie 内容；保存后不会回显" rows={3} onChange={(e) => setDrafts({ ...drafts, [platform]: e.target.value })} /></label>
        {value.cookies[platform].source === "managed" && (confirmClearCookie === platform
          ? <span className="confirm-strip">确认清除{platformLabel[platform]}托管 Cookie？
              <button className="danger-ghost" onClick={async () => { setConfirmClearCookie(null); try { onChange(await clearManagedCookie(platform)); setFeedback({ ok: true, text: "托管 Cookie 已清除" }); } catch (error) { setFeedback({ ok: false, text: String(error) }); } }}>确认</button>
              <button className="ghost" onClick={() => setConfirmClearCookie(null)}>取消</button>
            </span>
          : <button className="danger-ghost" onClick={() => setConfirmClearCookie(platform)}>清除已托管 Cookie</button>)}
      </div>)}
    </div>
    <div className="settings-card"><h2>下载引擎</h2><p>GetTube 依赖系统 yt-dlp（建议保持较新版本，各平台提取器随版本更新修复）。</p>
      <div className="about-row"><span>yt-dlp 版本</span><strong>{engine?.currentVersion ?? "检测中…"}</strong><button className="ghost" onClick={onRefreshEngine}><RefreshCw size={15} />检查引擎更新</button></div>
      {engine?.latestVersion && <div className="about-row"><span>官方最新版本</span><strong>{engine.latestVersion}</strong></div>}
      {engine?.enginePath && <p className="engine-path">引擎位置：{engine.enginePath}</p>}
      {engine?.status === "outdated" && engine.latestVersion && (
        <div className="notice notice-engine">yt-dlp 有新版本 {engine.latestVersion}（当前 {engine.currentVersion}）。请在终端执行：{engine.upgradeCommand ?? "yt-dlp -U"}</div>
      )}
      {engine?.status === "missing" && <div className="notice">未找到 yt-dlp，请先安装（如 brew install yt-dlp）</div>}
      {engine?.status === "channel_unavailable" && <p className="engine-path">无法获取最新版本信息（网络或代理原因），本地引擎不受影响。</p>}
      <div className="about-row" style={{ marginTop: 10 }}><span>下载归档</span>
        {confirmClearArchive
          ? <span className="confirm-strip">确认清空归档？之后批量下载将重新下载全部条目
              <button className="danger-ghost" onClick={runClearArchive}>确认</button>
              <button className="ghost" onClick={() => setConfirmClearArchive(false)}>取消</button>
            </span>
          : <button className="ghost" onClick={() => setConfirmClearArchive(true)}>清空归档</button>}
      </div>
    </div>
    <div className="settings-card"><h2>关于与更新</h2><p>开发阶段仅检查 GitHub Release 和打开说明页，不下载或安装应用包。</p>
      <div className="about-row"><span>当前版本</span><strong>{updateResult?.currentVersion ?? version}</strong><button className="ghost" onClick={async () => { setFeedback(null); try { const result = await checkUpdates(true); setUpdateResult(result); setFeedback({ ok: ["available", "latest"].includes(result.status), text: updateText(result) }); } catch (error) { setFeedback({ ok: false, text: error instanceof Error ? error.message : String(error) }); } }}><RefreshCw size={15} />检查更新</button></div>
      {updateResult?.status === "available" && updateResult.releaseUrl && <button className="ghost" onClick={() => openReleaseUrl(updateResult.releaseUrl!)}><ExternalLink size={15} />查看 {updateResult.latestVersion} 发布说明</button>}
    </div>
  </section>;
}

function updateText(result: UpdateResult): string {
  switch (result.status) {
    case "available": return `发现新版本 ${result.latestVersion}`;
    case "latest": return "已是最新版本";
    case "channel_unconfigured": return "发布渠道尚未配置";
    case "channel_unavailable": return "发布渠道尚未建立";
    default: return "24 小时内已检查过更新";
  }
}
