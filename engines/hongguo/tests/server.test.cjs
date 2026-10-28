const { test } = require('node:test');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const path = require('node:path');
const readline = require('node:readline');

test('stdio protocol correlates concurrent failures and exits on input closure', { timeout: 5000 }, async () => {
  const child = spawn(process.execPath, [path.join(__dirname, '../server.cjs')]);
  try {
    const events = [], reader = readline.createInterface({ input: child.stdout });
    const done = new Promise(resolve => reader.on('line', line => { events.push(JSON.parse(line)); if (events.length === 2) resolve(); }));
    for (const id of ['one', 'two']) child.stdin.write(JSON.stringify({ id, method: 'probe', params: { url: 'https://localhost/private' } }) + '\n');
    await done;
    assert.deepEqual(events.map(e => e.id).sort(), ['one', 'two']);
    assert.ok(events.every(e => e.type === 'error'));
    child.stdin.end();
    await new Promise(resolve => child.on('close', resolve));
    assert.equal(child.exitCode, 0);
  } finally { child.kill(); }
});
