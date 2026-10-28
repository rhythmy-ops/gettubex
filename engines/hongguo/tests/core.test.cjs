const { test, before, after, beforeEach } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const fsp = fs.promises;
const path = require('node:path');
const os = require('node:os');
const { Readable } = require('node:stream');
const { execFileSync } = require('node:child_process');
const axios = require('axios');
const core = require('../core.cjs');
const native = require('../vendor/hongguo');
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gettube-hongguo-test-'));
let bytes, serial = 100, seriesId, requests, adapter;
before(() => {
  const fixture = path.join(root, 'synthetic.mp4');
  execFileSync('ffmpeg', ['-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=64x96:rate=5',
    '-f', 'lavfi', '-i', 'sine=frequency=440', '-t', '16', '-c:v', 'libx264', '-c:a', 'aac', '-shortest', fixture]);
  bytes = fs.readFileSync(fixture);
});
beforeEach(() => {
  seriesId = String(++serial); requests = [];
  native.resolveSeriesId = async url => new URL(url).pathname.split('/')[2];
  native.fetchEpisodeList = async sid => ({ series_id: sid, series_title: 'Synthetic', episodes: [1, 2].map(i =>
    ({ vid: String(i), vid_index: i, series_id: sid, web_available: i === 1 })) });
  native.fetchPlayUrlSingle = async vid => ({ url: 'https://media.example/' + vid, source: 'app', vid });
  adapter = async config => ({ data: Readable.from([bytes]), headers: { 'content-length': String(bytes.length) }, status: 200, config });
  axios.defaults.adapter = async config => { requests.push(config); return adapter(config); };
});
after(() => fs.rmSync(root, { recursive: true, force: true }));
function params(id = '1', directory = path.join(root, seriesId)) {
  return { url: `https://hongguoduanju.com/player/${seriesId}/${id}`, quality: 'auto', directory, proxy: null };
}
function run(p, signal = AbortSignal.timeout(15000), emit = () => {}) {
  return core.withContext(p, signal, () => core.download(p, signal, emit));
}

test('official inputs, exact episode IDs, and list metadata', async () => {
  for (const url of ['https://hongguoduanju.com.evil.test/player/1/2', 'file:///tmp/a', 'https://user:pass@hongguoduanju.com/player/1/2']) {
    assert.throws(() => core.checkInput(url));
  }
  assert.equal(core.episodeIdentity('https://hongguoduanju.com/player/1/9007199254740993').episodeId, '9007199254740993');
  const list = await core.withContext({}, AbortSignal.timeout(1000), () => core.probe(params()));
  assert.equal(list.platform, 'hongguo'); assert.equal(list.entries.length, 2);
  assert.match(list.entries[1].title, /第 2 集/);
});
test('direct mode ignores inherited environment proxy; configured proxy is explicit', async () => {
  const previous = process.env.ALL_PROXY;
  process.env.ALL_PROXY = 'http://127.0.0.1:1';
  try {
    await core.withContext({}, AbortSignal.timeout(1000), () => axios.get('https://media.example/test'));
    assert.equal(requests.at(-1).proxy, false);
    for (const proxy of ['http://127.0.0.1:7890', 'https://127.0.0.1:7890', 'socks5://127.0.0.1:7890', 'socks5h://127.0.0.1:7890']) {
      await core.withContext({ proxy }, AbortSignal.timeout(1000), () => axios.get('https://media.example/test'));
      assert.equal(requests.at(-1).proxy, false); assert.ok(requests.at(-1).httpsAgent);
    }
  } finally { if (previous === undefined) delete process.env.ALL_PROXY; else process.env.ALL_PROXY = previous; }
});
test('valid download is fully decoded, stored with a receipt, and safely reused', async () => {
  const p = params(), events = [];
  const result = await run(p, undefined, e => events.push(e));
  assert.equal(result.actualQuality, '64p'); assert.ok(result.duration >= 16);
  assert.ok(events.some(e => e.type === 'processing'));
  assert.equal(result.episodeId, '1'); assert.equal(result.seriesId, seriesId);
  assert.equal((await run(p)).reused, true); assert.equal(requests.length, 1);
  assert.equal((await fsp.readdir(p.directory)).filter(n => n.startsWith('.gettube-hongguo-')).length, 0);
});
test('existing foreign file is never overwritten', async () => {
  const p = params(); await fsp.mkdir(p.directory);
  const destination = path.join(p.directory, 'Synthetic 001 [1].mp4');
  await fsp.writeFile(destination, 'user content');
  await assert.rejects(run(p), /同名文件/);
  assert.equal(await fsp.readFile(destination, 'utf8'), 'user content');
  assert.equal(requests.length, 0);
});
test('corrupt media and insufficient quality never publish completed files', async () => {
  const p = params(); p.quality = '1080p';
  await assert.rejects(run(p), /低于/);
  assert.deepEqual(await fsp.readdir(p.directory), []);
  adapter = async config => ({ data: Readable.from(['garbage']), headers: {}, status: 200, config });
  p.quality = 'auto';
  await assert.rejects(run(p), /媒体检查/);
  assert.deepEqual(await fsp.readdir(p.directory), []);
});
test('cancel during transfer removes owned temporary files', async () => {
  const controller = new AbortController();
  adapter = async config => {
    const stream = new Readable({ read() {} });
    stream.push(bytes.subarray(0, 1000));
    setTimeout(() => controller.abort(), 25);
    return { data: stream, headers: {}, status: 200, config };
  };
  const p = params(); await assert.rejects(run(p, controller.signal));
  assert.deepEqual(await fsp.readdir(p.directory), []);
});
test('cancel at processing stage never publishes output', async () => {
  const controller = new AbortController(), p = params();
  await assert.rejects(run(p, controller.signal, e => { if (e.type === 'processing') controller.abort(); }));
  assert.deepEqual(await fsp.readdir(p.directory), []);
});
test('parallel episodes with identical bytes cannot both publish', async () => {
  const a = params('1'), b = params('2');
  const results = await Promise.allSettled([run(a), run(b)]);
  assert.equal(results.filter(r => r.status === 'fulfilled').length, 1);
  assert.match(results.find(r => r.status === 'rejected').reason.message, /重复/);
});
test('403 tries a same-quality backup; 429 stops without retry storm', async () => {
  native.fetchPlayUrlSingle = async vid => ({ url: 'https://media.example/primary', source: 'app', vid,
    variants: [{ url: 'https://media.example/primary' }, { url: 'https://media.example/backup' }] });
  const standard = adapter;
  adapter = async config => {
    if (config.url.endsWith('/primary')) throw Object.assign(new Error('403'), { response: { status: 403 } });
    return standard(config);
  };
  await run(params()); assert.equal(requests.length, 2);
  requests = [];
  adapter = async () => { throw Object.assign(new Error('429'), { response: { status: 429 } }); };
  await assert.rejects(run(params('2')), /429/);
  assert.equal(requests.length, 1);
});
