// SPDX-License-Identifier: GPL-3.0-only
// Line-delimited JSON over stdio; no listening socket or account storage.
const readline = require('node:readline');
const core = require('./core.cjs');
console.log = () => {};
console.warn = () => {};
const jobs = new Map();
const send = data => process.stdout.write(JSON.stringify(data) + '\n');
const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on('line', line => {
  if (line.length > 65536) return;
  let request;
  try { request = JSON.parse(line); } catch { return; }
  const { id, method, params = {} } = request;
  if (typeof id !== 'string' || id.length > 100) return;
  if (method === 'cancel') { jobs.get(id)?.abort(); return; }
  if (jobs.has(id)) return;
  if (!['probe', 'download', 'status'].includes(method)) return send({ id, type: 'error', error: '未知引擎操作' });
  const controller = new AbortController();
  jobs.set(id, controller);
  const timer = setTimeout(() => controller.abort(), method === 'download' ? 1800000 : 60000);
  core.withContext(params, controller.signal, () => method === 'status' ? core.status(controller.signal) : method === 'probe' ? core.probe(params)
    : core.download(params, controller.signal, event => send({ id, ...event })))
    .then(result => send({ id, type: 'result', result }))
    .catch(error => send({ id, type: 'error', error: core.safeError(error, controller.signal) }))
    .finally(() => { clearTimeout(timer); jobs.delete(id); });
});
input.on('close', () => { for (const controller of jobs.values()) controller.abort(); });
process.on('SIGTERM', () => { for (const controller of jobs.values()) controller.abort(); input.close(); });
