#!/usr/bin/env node
// Real browser/stdio integration, independent of the desktop UI and personal
// browser state. No dependencies are installed and only localhost is visited.
import assert from 'node:assert/strict';
import test from 'node:test';
import fs from 'node:fs/promises';
import { existsSync } from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';
import { once } from 'node:events';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const moduleDir = process.env.OPENIDE_PLAYWRIGHT_MODULE_DIR
  || path.join(process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local/share'), 'openide/browser-runtime/deps');
const systemBrowser = existsSync('/etc/NIXOS')
  ? ['chromium', 'chromium-browser', 'google-chrome-stable', 'google-chrome']
    .flatMap(name => (process.env.PATH || '').split(path.delimiter).map(dir => path.join(dir, name)))
    .find(candidate => existsSync(candidate))
  : undefined;
const executable = process.env.OPENIDE_BROWSER_EXECUTABLE || systemBrowser;

function client(script, env, cwd) {
  const child = spawn(process.execPath, [path.join(root, script)], {
    cwd, env, stdio: ['pipe', 'pipe', 'pipe'],
  });
  let output = Buffer.alloc(0);
  let stderr = '';
  const pending = new Map();
  let sequence = 0;
  child.stderr.on('data', bytes => { stderr = (stderr + bytes.toString()).slice(-4096); });
  child.stdout.on('data', bytes => {
    output = Buffer.concat([output, bytes]);
    assert.ok(output.length <= 4 * 1024 * 1024, 'stdout is bounded');
    for (;;) {
      const newline = output.indexOf(10);
      if (newline < 0) break;
      const value = JSON.parse(output.subarray(0, newline).toString('utf8'));
      output = output.subarray(newline + 1);
      const waiter = pending.get(value.id);
      assert.ok(waiter, `Unexpected response ${value.id}`);
      pending.delete(value.id);
      clearTimeout(waiter.timer);
      waiter.resolve(value);
    }
  });
  const rejectPending = reason => {
    for (const waiter of pending.values()) { clearTimeout(waiter.timer); waiter.reject(reason); }
    pending.clear();
  };
  const exited = new Promise(resolve => child.once('exit', (code, signal) => {
    rejectPending(new Error(`Browser exited ${code}/${signal}: ${stderr}`));
    resolve({ code, signal });
  }));
  child.once('error', rejectPending);
  function request(payload) {
    const id = ++sequence;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error(`Request ${id} timed out: ${stderr}`));
      }, 35_000);
      pending.set(id, { resolve, reject, timer });
      child.stdin.write(JSON.stringify({ id, ...payload }) + '\n');
    });
  }
  return {
    request,
    command: command => request({ command }),
    async stop() {
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
      const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
      try { return await exited; } finally { clearTimeout(timer); }
    },
    exited,
  };
}

function environment(temporary) {
  const env = {};
  for (const name of ['PATH', 'HOME', 'USER', 'LOGNAME', 'LANG', 'LC_ALL', 'XDG_RUNTIME_DIR', 'SystemRoot', 'WINDIR', 'USERPROFILE', 'LOCALAPPDATA', 'APPDATA', 'PATHEXT']) {
    if (process.env[name] !== undefined) env[name] = process.env[name];
  }
  return { ...env, PLAYWRIGHT_BROWSERS_PATH: '0', TMPDIR: path.join(temporary, 'deleted-nix-shell'), TMP: temporary, TEMP: temporary };
}

function screenshot(snapshot) {
  assert.equal(snapshot.capture_error, undefined);
  const bytes = Buffer.from(snapshot.screenshot.base64, 'base64');
  assert.ok(bytes.length < 2 * 1024 * 1024);
  assert.deepEqual(bytes.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
  assert.equal(bytes.readUInt32BE(16), snapshot.viewport_width);
  assert.equal(bytes.readUInt32BE(20), snapshot.viewport_height);
  return bytes;
}

async function artifact(name, snapshot) {
  const bytes = screenshot(snapshot);
  if (process.env.OPENIDE_BROWSER_E2E_ARTIFACTS) {
    await fs.mkdir(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, { recursive: true });
    await fs.writeFile(path.join(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, name), bytes);
  }
}

test('real Chromium bridge survives deleted TMPDIR and handles navigation, input, scrolling, retry and cleanup', { timeout: 90_000 }, async t => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'openide-browser-e2e-'));
  const fixture = http.createServer((req, res) => {
    res.setHeader('Content-Type', 'text/html; charset=utf-8');
    if (req.url === '/second') {
      res.end('<!doctype html><title>Segunda página</title><h1>Segunda página</h1><a href="/">Inicio</a>');
      return;
    }
    res.end(`<!doctype html><html lang="es"><head><title>Fixture OpenIDE</title><style>
      body{font:18px sans-serif;margin:20px;background:linear-gradient(#fff,#bcd);min-height:2200px}
      #panel{height:130px}input,button,a{font:inherit;margin:4px}footer{position:absolute;top:1900px}
      </style></head><body><div id="panel"><label>Nombre <input id="name" aria-label="Nombre"></label>
      <button id="apply" onclick="document.querySelector('#value').textContent=document.querySelector('#name').value">Aplicar</button>
      <p id="value">Sin cambios</p><a id="next" href="/second">Segunda página</a></div>
      <textarea id="memo" aria-label="Notas">Texto de notas</textarea>
      <input id="readonly" aria-label="Sólo lectura" readonly value="Conservar lectura">
      <input id="password" type="password" value="no copiar">
      <div id="rich" contenteditable="true">Texto enriquecido</div>
      <p id="selection">Texto seleccionable 🧠</p>
      <button id="select" onclick="let r=document.createRange();r.selectNodeContents(document.querySelector('#selection'));getSelection().removeAllRanges();getSelection().addRange(r)">Seleccionar párrafo</button>
      <footer>Final de página</footer><script>addEventListener('scroll',()=>document.title='Scroll '+Math.round(scrollY))</script></body></html>`);
  });
  fixture.listen(0, '127.0.0.1');
  await once(fixture, 'listening');
  const origin = `http://127.0.0.1:${fixture.address().port}`;
  const bridge = client('apps/agents-window/src/browser_bridge.mjs', environment(temporary), temporary);
  t.after(async () => {
    await bridge.stop();
    fixture.closeAllConnections();
    await new Promise(resolve => fixture.close(resolve));
    await fs.rm(temporary, { recursive: true, force: true });
  });
  const ok = async command => {
    const response = await bridge.command(command);
    assert.equal(response.ok, true, response.error);
    return response.snapshot;
  };
  const start = { type: 'start', module_dir: moduleDir, executable_path: executable, width: 640, height: 480 };
  const initial = await ok(start);
  assert.equal(initial.url, 'about:blank');
  screenshot(initial);
  assert.equal(existsSync(path.join(temporary, 'deleted-nix-shell')), false, 'the deleted shell directory is not recreated');
  assert.ok((await fs.readdir(temporary)).some(name => name.startsWith('oid-')), 'private temporary root exists while browser is alive');
  let snapshot = await ok({ type: 'navigate', url: origin });
  assert.match(snapshot.title, /Fixture OpenIDE/);
  screenshot(snapshot);
  await ok({ type: 'click', x: 200, y: 35 });
  await ok({ type: 'type', text: 'Niño 🧠' });
  snapshot = await ok({ type: 'click_locator', selector: '#apply' });
  assert.match(snapshot.aria_snapshot, /Niño 🧠/);
  await ok({ type: 'fill', role: 'textbox', name: 'Nombre', text: 'Otro' });
  await ok({ type: 'press', key: 'Control+A' });
  await ok({ type: 'type', text: 'Reemplazo á' });
  snapshot = await ok({ type: 'click_locator', role: 'button', name: 'Aplicar' });
  assert.match(snapshot.aria_snapshot, /Reemplazo á/);
  await artifact('browser-input.png', snapshot);
  await ok({ type: 'click_locator', selector: '#name' });
  await ok({ type: 'press', key: 'Control+A' });
  const copied = await ok({ type: 'copy_selection' });
  assert.equal(copied.clipboard.text, 'Reemplazo á');
  assert.equal(copied.screenshot, undefined, 'clipboard read does not wait for a PNG');
  const prepared = await ok({ type: 'prepare_cut' });
  assert.equal(prepared.clipboard.text, 'Reemplazo á');
  assert.ok(prepared.clipboard.cut_token);
  assert.equal((await ok({ type: 'copy_selection' })).clipboard.text, 'Reemplazo á', 'preparing cut alone must not delete text');
  // Copy invalidates the old preparation. A new cut is explicitly confirmed
  // only after the host would have accepted the clipboard payload.
  const confirmed = await ok({ type: 'prepare_cut' });
  await ok({ type: 'confirm_cut', token: confirmed.clipboard.cut_token });
  assert.equal((await bridge.command({ type: 'copy_selection' })).ok, false, 'cut input is now empty');
  await ok({ type: 'fill', selector: '#memo', text: 'Notas á 🧠' });
  await ok({ type: 'press', key: 'Control+A' });
  const textarea = await ok({ type: 'prepare_cut' });
  assert.equal(textarea.clipboard.text, 'Notas á 🧠');
  await ok({ type: 'fill', selector: '#memo', text: 'Texto cambiado' });
  assert.equal((await bridge.command({ type: 'confirm_cut', token: textarea.clipboard.cut_token })).ok, false, 'a changed field is never cut');
  await ok({ type: 'press', key: 'Control+A' });
  assert.equal((await ok({ type: 'copy_selection' })).clipboard.text, 'Texto cambiado');
  await ok({ type: 'click_locator', selector: '#readonly' });
  await ok({ type: 'press', key: 'Control+A' });
  assert.equal((await ok({ type: 'copy_selection' })).clipboard.text, 'Conservar lectura');
  assert.equal((await bridge.command({ type: 'prepare_cut' })).ok, false);
  await ok({ type: 'click_locator', selector: '#password' });
  await ok({ type: 'press', key: 'Control+A' });
  assert.equal((await bridge.command({ type: 'copy_selection' })).ok, false, 'password clipboard is disabled');
  await ok({ type: 'click_locator', selector: '#rich' });
  await ok({ type: 'press', key: 'Control+A' });
  assert.equal((await bridge.command({ type: 'prepare_cut' })).ok, false, 'contenteditable cut is not emulated');
  await ok({ type: 'click_locator', selector: '#select' });
  assert.equal((await ok({ type: 'copy_selection' })).clipboard.text, 'Texto seleccionable 🧠');
  await ok({ type: 'fill', selector: '#name', text: 'x'.repeat(16 * 1024 + 1) });
  await ok({ type: 'press', key: 'Control+A' });
  const oversizedCut = await bridge.command({ type: 'prepare_cut' });
  assert.equal(oversizedCut.ok, false);
  assert.match(oversizedCut.error, /16 KiB/);
  await ok({ type: 'fill', selector: '#name', text: 'Reemplazo á' });
  const quiet = await ok({ type: 'type', text: '!', include_aria: false });
  assert.equal(quiet.aria_snapshot, undefined, 'UI typing omits expensive ARIA snapshots');
  await ok({ type: 'scroll', dx: 0, dy: 800 });
  const deadline = Date.now() + 3000;
  do { snapshot = await ok({ type: 'snapshot' }); }
  while (!/^Scroll [1-9]/.test(snapshot.title) && Date.now() < deadline);
  assert.match(snapshot.title, /^Scroll [1-9]/);
  await artifact('browser-scrolled.png', snapshot);
  snapshot = await ok({ type: 'click_locator', selector: '#next' });
  assert.equal(snapshot.url, origin + '/second');
  assert.equal(snapshot.can_go_back, true);
  snapshot = await ok({ type: 'back' });
  assert.equal(snapshot.url, origin + '/');
  assert.equal(snapshot.can_go_forward, true);
  snapshot = await ok({ type: 'forward' });
  assert.equal(snapshot.url, origin + '/second');
  snapshot = await ok({ type: 'reload' });
  assert.equal(snapshot.title, 'Segunda página');
  snapshot = await ok({ type: 'resize', width: 800, height: 600 });
  screenshot(snapshot);
  const invalid = await bridge.command({ type: 'navigate', url: 'file:///etc/passwd' });
  assert.equal(invalid.ok, false);
  assert.match(invalid.error, /HTTP/);
  assert.equal(invalid.snapshot.url, origin + '/second');
  const failed = await bridge.command({ type: 'navigate', url: 'http://127.0.0.1:1/' });
  assert.equal(failed.ok, false);
  snapshot = await ok({ type: 'navigate', url: origin });
  assert.equal(snapshot.title, 'Fixture OpenIDE');
  screenshot(snapshot);
  assert.equal((await bridge.command({ type: 'close' })).ok, true);
  assert.equal((await bridge.exited).code, 0);
  assert.deepEqual(await fs.readdir(temporary), [], 'browser artifacts and temporary profiles are removed');
  t.diagnostic(`Engine: ${executable || 'Playwright bundled Chromium'}; Playwright: ${moduleDir}`);
});

test('managed temporary directory and failed dependency retry use fresh browser resources', { timeout: 60_000 }, async t => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'obr-'));
  const managed = path.join(temporary, 'managed');
  await fs.mkdir(managed, { mode: 0o700 });
  const bridge = client('apps/agents-window/src/browser_bridge.mjs', {
    ...environment(temporary), OPENIDE_BROWSER_TEMP_DIR: managed,
  }, temporary);
  t.after(async () => { await bridge.stop(); await fs.rm(temporary, { recursive: true, force: true }); });
  const start = { type: 'start', module_dir: path.join(temporary, 'missing'), executable_path: executable, width: 320, height: 240 };
  assert.equal((await bridge.command(start)).code, 'playwright_missing');
  const retry = await bridge.command({ ...start, module_dir: moduleDir });
  assert.equal(retry.ok, true, retry.error);
  screenshot(retry.snapshot);
  assert.ok((await fs.readdir(managed)).length > 0);
  assert.equal((await bridge.command({ type: 'close' })).ok, true);
  await bridge.exited;
  assert.deepEqual(await fs.readdir(managed), []);
});

test('deep managed data path does not exceed Chromium Unix socket limits', { timeout: 45_000 }, async t => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'openide-browser-long-e2e-'));
  const managed = path.join(temporary, 'browser-runtime', 'scope-'.repeat(12), 'tmp');
  await fs.mkdir(managed, { recursive: true, mode: 0o700 });
  const bridge = client('apps/agents-window/src/browser_bridge.mjs', {
    ...environment(temporary), OPENIDE_BROWSER_TEMP_DIR: managed,
  }, temporary);
  t.after(async () => { await bridge.stop(); await fs.rm(temporary, { recursive: true, force: true }); });
  const response = await bridge.command({ type: 'start', module_dir: moduleDir, executable_path: executable, width: 320, height: 240 });
  assert.equal(response.ok, true, response.error);
  screenshot(response.snapshot);
  assert.deepEqual(await fs.readdir(managed), [], 'a short private root is used instead of the long managed path');
  assert.equal((await bridge.command({ type: 'close' })).ok, true);
  assert.equal((await bridge.exited).code, 0);
});

test('MCP proxy transports scoped localhost commands and never starts a browser itself', { timeout: 10_000 }, async t => {
  const token = 'a'.repeat(64);
  const received = [];
  const sockets = new Set();
  const broker = net.createServer(socket => {
    sockets.add(socket);
    socket.once('close', () => sockets.delete(socket));
    let bytes = '';
    socket.on('data', data => {
      bytes += data;
      const end = bytes.indexOf('\n');
      if (end < 0) return;
      const value = JSON.parse(bytes.slice(0, end));
      assert.equal(value.token, token);
      received.push(value);
      socket.end(JSON.stringify({ ok: true, result: value.op === 'list_tools'
        ? { tools: [{ name: 'browser_snapshot', inputSchema: { type: 'object' } }] }
        : { content: [{ type: 'text', text: 'localhost fixture' }] } }) + '\n');
    });
  });
  broker.listen(0, '127.0.0.1');
  await once(broker, 'listening');
  const proxy = client('apps/agents-window/src/browser_mcp.mjs', {
    ...environment(os.tmpdir()), OPENIDE_BROWSER_BROKER: `127.0.0.1:${broker.address().port}`, OPENIDE_BROWSER_TOKEN: token,
  }, root);
  t.after(async () => { await proxy.stop(); for (const socket of sockets) socket.destroy(); await new Promise(resolve => broker.close(resolve)); });
  assert.match((await proxy.request({ jsonrpc: '2.0', method: 'tools/list' })).error.message, /Initialize/);
  const initialized = await proxy.request({ jsonrpc: '2.0', method: 'initialize', params: { protocolVersion: '2025-11-25' } });
  assert.equal(initialized.result.serverInfo.name, 'openide-browser');
  const list = await proxy.request({ jsonrpc: '2.0', method: 'tools/list' });
  assert.equal(list.result.tools[0].name, 'browser_snapshot');
  const called = await proxy.request({ jsonrpc: '2.0', method: 'tools/call', params: { name: 'browser_snapshot', arguments: {} } });
  assert.equal(called.result.content[0].text, 'localhost fixture');
  assert.equal(received.length, 2);
});
