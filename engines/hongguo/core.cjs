// SPDX-License-Identifier: GPL-3.0-only
// GetTube adapter, 2026-10-28. Upstream attribution: NOTICE.
const fs = require('node:fs');
const fsp = fs.promises;
const path = require('node:path');
const { AsyncLocalStorage } = require('node:async_hooks');
const { pipeline } = require('node:stream/promises');
const { Transform } = require('node:stream');
const { createHash } = require('node:crypto');
const { execFile } = require('node:child_process');
const axios = require('axios');
const { HttpsProxyAgent } = require('https-proxy-agent');
const { SocksProxyAgent } = require('socks-proxy-agent');
const native = require('./vendor/hongguo');
const { decryptInWorker } = require('./vendor/decrypt-worker.cjs');
const { webMediaRequestOptions } = require('./vendor/web-media-guard');
const context = new AsyncLocalStorage();
const destinations = new Set();
const publications = new Map();
const seriesCache = new Map();
const HOSTS = ['hongguoduanju.com', 'novelquickapp.com', 'changdunovel.com'];
const idPattern = /^[1-9]\d{0,29}$/;
const QUALITIES = ['auto', '1080p', '720p'];

function checkInput(input) {
  const url = new URL(input);
  if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password ||
      !HOSTS.some(host => url.hostname === host || url.hostname.endsWith('.' + host))) {
    throw new Error('请输入红果官方分享链接');
  }
  return url.href;
}
function episodeIdentity(input) {
  const url = new URL(checkInput(input));
  const match = url.pathname.match(/^\/player\/([1-9]\d{0,29})\/([1-9]\d{0,29})\/?$/);
  if (url.hostname !== 'hongguoduanju.com' || !match) throw new Error('请先解析红果链接并选择分集');
  return { seriesId: match[1], episodeId: match[2] };
}
function proxyAgent(value) {
  if (!value) return null;
  const url = new URL(value);
  if (['http:', 'https:'].includes(url.protocol)) return new HttpsProxyAgent(url);
  if (['socks5:', 'socks5h:'].includes(url.protocol)) return new SocksProxyAgent(url);
  throw new Error('代理仅支持 HTTP、HTTPS、SOCKS5、SOCKS5h');
}
// No environment proxy inheritance. Every request carries its task's routing policy.
axios.interceptors.request.use(config => {
  const ctx = context.getStore();
  if (!ctx) throw new Error('缺少任务网络配置');
  config.proxy = false;
  // Registration is shared for this helper session and already has an 8-second
  // upstream deadline. Cancelling one episode must not poison all other jobs.
  if (!String(config.url).includes('/device_register/')) {
    config.signal = config.signal ? AbortSignal.any([config.signal, ctx.signal]) : ctx.signal;
  }
  config.timeout = Math.min(config.timeout || 30000, 30000);
  if (ctx.agent) { config.httpAgent = ctx.agent; config.httpsAgent = ctx.agent; }
  return config;
});
async function withContext(params, signal, run) {
  const agent = proxyAgent(params.proxy);
  try { return await context.run({ signal, agent, proxy: params.proxy || '' }, run); }
  finally { agent?.destroy(); }
}
function safeError(error, signal) {
  if (signal?.aborted || error?.code === 'ERR_CANCELED') return '任务已取消或超时';
  const message = String(error?.message || error);
  if (/ENOSPC/.test(message)) return '磁盘空间不足';
  if (/EACCES|EPERM/.test(message)) return '无法写入下载目录';
  if (/ECONNREFUSED|proxy|Proxy/.test(message)) return '网络或代理连接失败，请检查设置';
  if (/ENOTFOUND|EAI_AGAIN|ECONNRESET|timeout|ETIMEDOUT/.test(message)) return '网络连接失败或超时，请检查直连与代理设置';
  return message.replace(/https?:\/\/\S+/g, '[地址已隐藏]').slice(0, 400);
}
async function series(input) {
  const seriesId = await native.resolveSeriesId(checkInput(input));
  if (!idPattern.test(seriesId)) throw new Error('分享链接没有有效剧集 ID');
  const cacheKey = `${context.getStore()?.proxy || ''}:${seriesId}`;
  const cached = seriesCache.get(cacheKey);
  const data = cached?.expires > Date.now() ? cached.data :
    await native.fetchEpisodeList(seriesId, { deadline: Date.now() + 45000, force: true });
  if (String(data.series_id) !== seriesId || !data.episodes?.length) throw new Error('剧集列表为空或身份不匹配');
  const seen = new Set();
  for (const ep of data.episodes) {
    if (!idPattern.test(ep.vid) || seen.has(ep.vid) || String(ep.series_id) !== seriesId ||
        !Number.isSafeInteger(ep.vid_index) || ep.vid_index <= 0) throw new Error('分集信息不完整或重复');
    seen.add(ep.vid);
  }
  if (seriesCache.size > 50) seriesCache.clear();
  seriesCache.set(cacheKey, { data, expires: Date.now() + 60000 });
  return data;
}
async function probe(params) {
  const data = await series(params.url);
  return {
    title: data.series_title, total: data.episodes.length, platform: 'hongguo',
    entries: data.episodes.map(ep => ({ id: ep.vid, title: `第 ${ep.vid_index} 集${ep.title ? ' · ' + ep.title : ''}`,
      source: ep.web_available === true ? '网页可播' : '下载时验证片源',
      url: `https://hongguoduanju.com/player/${data.series_id}/${ep.vid}` })),
  };
}
function tool(name) {
  for (const dir of [...(process.env.PATH || '').split(path.delimiter), '/opt/homebrew/bin', '/usr/local/bin']) {
    const candidate = path.join(dir, process.platform === 'win32' ? name + '.exe' : name);
    if (fs.existsSync(candidate)) return candidate;
  }
  throw new Error(`缺少 ${name}，请安装 FFmpeg 后重试`);
}
function execute(file, args, signal, timeout = 600000) {
  return new Promise((resolve, reject) => {
    execFile(file, args, { signal, timeout, maxBuffer: 2 * 1024 * 1024, windowsHide: true }, (err, stdout) => {
      if (err) reject(new Error(signal.aborted ? '任务已取消' : '媒体检查或处理失败（文件可能不完整，或本机 FFmpeg 不支持该格式）'));
      else resolve(stdout);
    });
  });
}
async function validateMedia(file, quality, signal) {
  const info = JSON.parse(await execute(tool('ffprobe'), ['-v', 'error', '-protocol_whitelist', 'file,crypto,data',
    '-show_streams', '-show_format', '-of', 'json', file], signal, 30000));
  const video = info.streams?.find(s => s.codec_type === 'video');
  const duration = Number(info.format?.duration);
  if (!video?.width || !video?.height || !(duration > 0) || !info.streams.some(s => s.codec_type === 'audio')) {
    throw new Error('文件缺少有效视频、音频或时长');
  }
  if (duration < 15) throw new Error('视频不足 15 秒，可能是片头或预告，请人工核实；未标记成功');
  const pixels = Math.min(video.width, video.height);
  if (quality !== 'auto' && pixels < parseInt(quality)) throw new Error(`实际画质 ${pixels}p 低于所选 ${quality}，未标记成功`);
  // Full decode, not just a successful download or the first few seconds.
  await execute(tool('ffmpeg'), ['-v', 'error', '-xerror', '-nostdin', '-protocol_whitelist', 'file,crypto,data',
    '-i', file, '-map', '0:v:0', '-map', '0:a:0', '-f', 'null', '-'], signal);
  return { actualQuality: `${pixels}p`, duration };
}
async function hashFile(file) {
  const hash = createHash('sha256');
  for await (const chunk of fs.createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}
function cleanName(name) {
  return String(name).replace(/[\x00-\x1f/\\:*?"<>|]/g, ' ').trim().replace(/^\.+|\.+$/g, '').slice(0, 65) || '红果短剧';
}
async function publishSerial(directory, operation) {
  const previous = publications.get(directory) || Promise.resolve();
  let release;
  const current = new Promise(resolve => { release = resolve; });
  publications.set(directory, current);
  await previous;
  try { return await operation(); }
  finally { release(); if (publications.get(directory) === current) publications.delete(directory); }
}
async function transfer(info, file, signal, emit) {
  const response = await axios.get(info.url, { ...webMediaRequestOptions(info.url), signal,
    headers: { 'User-Agent': info.userAgent || native.UA, Referer: info.source === 'web' ? native.WEB_REFERER : native.VIDEO_REFERER }, responseType: 'stream', timeout: 30000 });
  const total = Number(response.headers['content-length']) || 0;
  let bytes = 0, last = 0;
  const meter = new Transform({ transform(chunk, _encoding, cb) {
    bytes += chunk.length;
    if (bytes > 2 * 1024 ** 3) return cb(new Error('分集超过 2 GB，已停止下载'));
    if (Date.now() - last > 200) { last = Date.now(); emit({ type: 'progress', progress: total ? Math.min(90, bytes / total * 90) : 0 }); }
    cb(null, chunk);
  } });
  await pipeline(response.data, meter, fs.createWriteStream(file, { flags: 'wx', mode: 0o600 }), { signal });
  if (!bytes || total && total !== bytes) throw new Error('媒体下载不完整');
}
async function download(params, signal, emit) {
  const identity = episodeIdentity(params.url);
  const quality = params.quality;
  if (!QUALITIES.includes(quality)) throw new Error('红果只支持自动、1080p 或 720p 视频');
  if (!path.isAbsolute(params.directory)) throw new Error('下载目录必须是绝对路径');
  const data = await series(params.url);
  const ep = data.episodes.find(e => e.vid === identity.episodeId);
  if (!ep) throw new Error('该分集不属于所选剧集');
  const filename = `${cleanName(data.series_title)} ${String(ep.vid_index).padStart(3, '0')} [${ep.vid}].mp4`;
  const destination = path.join(params.directory, filename);
  if (destinations.has(destination)) throw new Error('该分集已有任务正在下载');
  destinations.add(destination);
  let temporary;
  try {
    await fsp.mkdir(params.directory, { recursive: true });
    const receiptPath = destination + '.gettube.json';
    if (fs.existsSync(destination)) {
      let receipt;
      try { receipt = JSON.parse(await fsp.readFile(receiptPath, 'utf8')); } catch { /* preserve unowned files */ }
      if (receipt?.episodeId !== ep.vid || receipt?.seriesId !== data.series_id || receipt.sha256 !== await hashFile(destination)) {
        throw new Error('同名文件已存在，无法确认完整性，已保留原文件');
      }
      emit({ type: 'processing', progress: 90 });
      const verified = await validateMedia(destination, quality, signal);
      return { ...receipt, ...verified, path: destination, reused: true };
    }
    temporary = await fsp.mkdtemp(path.join(params.directory, '.gettube-hongguo-'));
    const encrypted = path.join(temporary, 'source.mp4');
    const decoded = path.join(temporary, 'decoded.mp4');
    let info;
    for (let attempt = 0; attempt < 2; attempt++) {
      signal.throwIfAborted();
      info = await native.fetchPlayUrlSingle(ep.vid, data.series_id, { quality, force: true, allowLowerQuality: false });
      if (!info.url) throw new Error(info.error || '该分集暂无可用片源');
      if (info.format === 'hls') throw new Error('该分集使用尚未验证的 HLS 片源');
      try {
        let last;
        const variants = info.variants?.length ? info.variants.slice(0, 4) : [info];
        for (const candidate of variants) {
          try { await transfer(candidate, encrypted, signal, emit); info = candidate; last = null; break; }
          catch (error) {
            last = error;
            await fsp.rm(encrypted, { force: true });
            if (signal.aborted || ![403, 404, 410, 502, 503, 504].includes(error.response?.status) &&
              !['ECONNRESET', 'ETIMEDOUT', 'ECONNABORTED'].includes(error.code)) throw error;
          }
        }
        if (last) throw last;
        break;
      }
      catch (err) {
        await fsp.rm(encrypted, { force: true });
        native.markWebMediaFailure(info);
        if (attempt || signal.aborted || ![403, 404, 410, 502, 503, 504].includes(err.response?.status) &&
          !['ECONNRESET', 'ETIMEDOUT', 'ECONNABORTED'].includes(err.code)) throw err;
      }
    }
    signal.throwIfAborted();
    emit({ type: 'processing', progress: 90 });
    let final = encrypted;
    if (info.contentKey) {
      if (!/^[a-f\d]{32}$/i.test(info.contentKey)) throw new Error('媒体处理信息无效');
      await execute(tool('ffmpeg'), ['-v', 'error', '-nostdin', '-n', '-protocol_whitelist', 'file,crypto,data',
        '-decryption_key', info.contentKey, '-i', encrypted, '-map', '0:v:0', '-map', '0:a:0', '-c', 'copy', decoded], signal);
      final = decoded;
    } else if (info.spadeA) {
      if ((await fsp.stat(encrypted)).size > 512 * 1024 ** 2) throw new Error('该媒体处理路径超过 512 MB 内存保护上限');
      const key = native.deriveKey(info.spadeA);
      if (!key) throw new Error('媒体处理信息无效');
      await decryptInWorker({ srcPath: encrypted, dstPath: decoded, key }, { signal });
      final = decoded;
    }
    const verified = await validateMedia(final, quality, signal);
    const sha256 = await hashFile(final);
    return await publishSerial(params.directory, async () => {
    // Serial publication closes the race between concurrent duplicate checks.
    for (const name of await fsp.readdir(params.directory)) {
      if (!name.endsWith('.mp4.gettube.json')) continue;
      let other;
      try { other = JSON.parse(await fsp.readFile(path.join(params.directory, name), 'utf8')); } catch { continue; }
      if (other.seriesId === data.series_id && other.episodeId !== ep.vid && other.sha256 === sha256) throw new Error('检测到与其他分集完全重复的内容，未标记成功');
    }
    const result = { engine: 'hongguo', ...identity, episodeIndex: ep.vid_index, requestedQuality: quality, ...verified, sha256, path: destination };
    signal.throwIfAborted();
    // Atomic no-overwrite publication; all decoding and validation precedes this step.
    try { await fsp.link(final, destination); }
    catch (err) {
      if (!['EXDEV', 'ENOTSUP', 'EPERM'].includes(err.code)) throw err;
      try { await fsp.copyFile(final, destination, fs.constants.COPYFILE_EXCL); }
      catch (copyError) { throw copyError; }
    }
    await fsp.writeFile(receiptPath, JSON.stringify(result), { mode: 0o600 });
    return result;
    });
  } finally {
    if (temporary) await fsp.rm(temporary, { recursive: true, force: true });
    destinations.delete(destination);
  }
}
async function status(signal) {
  const result = { nodeVersion: process.versions.node, ready: true };
  for (const name of ['ffmpeg', 'ffprobe']) {
    try { const file = tool(name); await execute(file, ['-version'], signal, 5000); result[name] = file; }
    catch { result[name] = null; result.ready = false; }
  }
  return result;
}
module.exports = { status, probe, download, withContext, safeError, checkInput, episodeIdentity, cleanName, validateMedia, hashFile, proxyAgent };
