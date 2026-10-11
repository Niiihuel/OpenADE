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
import { performance } from 'node:perf_hooks';

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
  // Unsolicited lines of the live view, in arrival order.
  const events = [];
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
      if (value.event) { events.push({ ...value, at: performance.now() }); continue; }
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
    events,
    stdout: child.stdout,
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

// Pixel size of an encoded frame, from its header.
function frameSize(frame) {
  const bytes = Buffer.from(frame.data, 'base64');
  if (frame.format === 'png') return [bytes.readUInt32BE(16), bytes.readUInt32BE(20)];
  for (let at = 2; at + 9 < bytes.length; at += 2 + bytes.readUInt16BE(at + 2)) {
    if (bytes[at + 1] >= 0xc0 && bytes[at + 1] <= 0xc3) return [bytes.readUInt16BE(at + 7), bytes.readUInt16BE(at + 5)];
  }
  throw new Error('JPEG frame without a size');
}

const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const median = values => [...values].sort((a, b) => a - b)[values.length >> 1];
const percentile = (values, part) => [...values].sort((a, b) => a - b)[Math.min(values.length - 1, Math.floor(values.length * part))];

function liveFixture() {
  const rows = Array.from({ length: 80 }, (_, row) => `<div class="row"><b>Fila ${row}</b> — Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor.</div>`).join('');
  const style = `html{background:#fff}body{font:14px/1.5 sans-serif;margin:0;color:#111}.row{border:1px solid #ccc;border-radius:8px;padding:12px;margin:8px 24px}
    #spin{position:fixed;right:20px;top:20px;width:60px;height:60px;background:linear-gradient(45deg,#f06,#48f);animation:turn 1s linear infinite}
    @keyframes turn{to{transform:rotate(360deg)}}
    #hover{position:absolute;left:20px;top:20px;width:200px;height:40px;background:#dde}#hover:hover{background:#365ff4}
    #text{position:absolute;left:20px;top:100px;margin:0;font:20px/30px monospace;white-space:nowrap}
    #rows{position:absolute;top:160px;left:0;right:0}`;
  return http.createServer((request, response) => {
    response.setHeader('Content-Type', 'text/html; charset=utf-8');
    const animated = request.url.startsWith('/animated');
    response.end(`<!doctype html><html lang="es"><head><title>${animated ? 'Animada' : 'Quieta'}</title><style>${style}</style></head><body>
      ${animated ? '<div id="spin"></div>' : ''}
      <div id="hover" onmouseenter="document.title='Encima'" onmouseleave="document.title='Fuera'"></div>
      <button id="paint" style="position:absolute;left:300px;top:20px" onclick="document.body.style.background='#fde'">Pintar</button>
      <p id="text">Texto para arrastrar</p><div id="rows">${rows}</div></body></html>`);
  });
}

for (const scale of [1, 2]) {
  test(`live view at scale ${scale}: pushed frames at device pixels, lossless when still, silent when hidden, pointer input`, { timeout: 90_000 }, async t => {
    const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'openide-browser-live-'));
    const fixture = liveFixture();
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
    const frames = () => bridge.events.filter(event => event.event === 'frame');
    const until = async (predicate, label, timeout = 5000) => {
      const deadline = performance.now() + timeout;
      for (;;) {
        const found = predicate();
        if (found) return found;
        assert.ok(performance.now() < deadline, `timed out waiting for ${label}`);
        await pause(5);
      }
    };
    const framesAfter = (mark, format) => frames().filter(frame => frame.seq > mark && (!format || frame.format === format));
    const last = () => frames().at(-1)?.seq ?? 0;
    let viewport = [640, 480];
    const expectSize = frame => assert.deepEqual(frameSize(frame), viewport.map(side => side * scale), `frame ${frame.seq} (${frame.format})`);

    const initial = await ok({ type: 'start', module_dir: moduleDir, executable_path: executable, width: 640, height: 480, scale });
    screenshot(initial);
    await ok({ type: 'navigate', url: `${origin}/animated` });
    await pause(300);
    assert.equal(frames().length, 0, 'nothing is pushed before the surface is visible');

    // Visible and animating: frames arrive on their own, at device pixels.
    let state = await ok({ type: 'stream', enabled: true });
    assert.equal(state.screenshot, undefined, 'the view does not poll PNG captures');
    await until(() => frames().length >= 20, 'frames of an animating page');
    for (const frame of frames()) {
      assert.equal(frame.format, 'jpeg', 'an animating page never waits for a lossless frame');
      assert.equal(frame.scale, scale);
      expectSize(frame);
    }
    assert.deepEqual(frames().map(frame => frame.seq), frames().map((_, index) => index + 1), 'frames arrive in order');

    // Replies are not starved by the stream.
    const replies = [];
    for (let index = 0; index < 10; index++) {
      const sent = performance.now();
      await ok({ type: 'state' });
      replies.push(performance.now() - sent);
    }
    assert.ok(Math.max(...replies) < 500, `a reply waited ${Math.max(...replies)} ms behind frames`);

    // The agent's capture stays a 1x PNG and never leaks its re-render into the view.
    let mark = last();
    const agent = await ok({ type: 'snapshot' });
    screenshot(agent);
    assert.match(agent.aria_snapshot, /Texto para arrastrar/);
    await pause(200);
    framesAfter(mark).forEach(expectSize);

    // A reader that stops reading makes Chromium skip frames, not queue them:
    // what waits is what the pipe holds, however long the reader is away.
    const perSecond = frames().length / ((frames().at(-1).at - frames()[0].at) / 1000);
    mark = last();
    bridge.stdout.pause();
    await pause(1500);
    const resumed = performance.now();
    bridge.stdout.resume();
    await until(() => frames().some(frame => frame.at > resumed + 150), 'frames after the reader resumed');
    const backlog = frames().filter(frame => frame.seq > mark && frame.at <= resumed + 40);
    const waiting = backlog.reduce((bytes, frame) => bytes + frame.data.length, 0);
    // The pipe's own buffers, and the frames Chromium had in flight.
    const typical = median(frames().map(frame => frame.data.length));
    assert.ok(waiting <= 600 * 1024 + 3 * typical, `${waiting} bytes of frames were queued behind a paused reader`);
    assert.ok(backlog.length <= perSecond * 1.5 / 2, `${backlog.length} frames waited for a reader away 1.5 s at ${perSecond.toFixed(0)} frames/s`);

    // Hidden: the screencast stops.
    await ok({ type: 'stream', enabled: false });
    mark = last();
    await pause(500);
    assert.equal(framesAfter(mark).length, 0, 'no frame is pushed while hidden');

    // Still: one lossless frame at device scale follows, and it is not a loop.
    await ok({ type: 'navigate', url: `${origin}/still` });
    await pause(400);
    mark = last();
    await ok({ type: 'stream', enabled: true });
    const lossless = await until(() => framesAfter(mark, 'png')[0], 'a lossless frame once the page is still');
    expectSize(lossless);
    await pause(1500);
    assert.equal(framesAfter(lossless.seq).length, 0, 'a still page pushes nothing after its lossless frame');
    assert.equal(framesAfter(mark, 'png').length, 1);
    if (process.env.OPENIDE_BROWSER_E2E_ARTIFACTS) {
      await fs.mkdir(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, { recursive: true });
      await fs.writeFile(path.join(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, `live-${scale}x.png`), Buffer.from(lossless.data, 'base64'));
    }

    // The agent's 1x capture draws the page at another scale for a moment.
    // The view never shows that, and it does show what the agent changed.
    mark = last();
    for (let index = 0; index < 3; index++) screenshot(await ok({ type: 'snapshot' }));
    await pause(600);
    assert.deepEqual(framesAfter(mark).map(frame => frame.format), [], 'captures of an unchanged page push nothing to the view');
    screenshot(await ok({ type: 'click_locator', selector: '#paint' }));
    const painted = await until(() => framesAfter(mark, 'png')[0], 'a lossless frame of what the agent changed');
    expectSize(painted);
    assert.notEqual(painted.data, lossless.data);
    await pause(600);
    assert.equal(framesAfter(painted.seq).length, 0);

    // Hover: a move alone changes the page, and the view shows it.
    mark = last();
    assert.equal((await bridge.command({ type: 'pointer_move', x: 60, y: 40 })).snapshot, undefined, 'a move answers without page state');
    const hovered = await until(() => framesAfter(mark, 'jpeg')[0], 'a frame of the hover state');
    expectSize(hovered);
    assert.equal((await ok({ type: 'state' })).title, 'Encima');
    await until(() => framesAfter(hovered.seq, 'png')[0], 'the lossless frame of the hover state');
    await ok({ type: 'pointer_move', x: 400, y: 400 }).catch(() => {});
    assert.equal((await ok({ type: 'state' })).title, 'Fuera');

    // Press, drag and release select text like a real pointer.
    await bridge.command({ type: 'pointer_down', x: 21, y: 115 });
    for (const x of [60, 120, 200]) await bridge.command({ type: 'pointer_move', x, y: 115 });
    state = await ok({ type: 'pointer_up', x: 258, y: 115 });
    assert.equal(state.screenshot, undefined, 'input from the window does not wait for a capture');
    assert.equal((await ok({ type: 'copy_selection' })).clipboard.text, 'Texto para arrastrar');

    // A resize keeps the CSS viewport logical and the frames at its device size.
    mark = last();
    state = await ok({ type: 'resize', width: 800, height: 600, include_aria: false, include_image: false });
    assert.deepEqual([state.viewport_width, state.viewport_height, state.screenshot], [800, 600, undefined]);
    viewport = [800, 600];
    // Chromium may still push a frame of the surface on its way to the new
    // size. The view ends on the page at that size, lossless.
    const resized = await until(() => framesAfter(mark, 'png').find(frame => frameSize(frame).join() === `${800 * scale},${600 * scale}`), 'the lossless frame at the new size');
    await pause(400);
    assert.equal(frames().at(-1).seq, resized.seq, 'nothing follows the resized page once it is still');

    assert.equal(bridge.events.filter(event => event.event !== 'frame').length, 0, 'the stream was never lost');
    assert.equal((await bridge.command({ type: 'close' })).ok, true);
    assert.equal((await bridge.exited).code, 0);
    t.diagnostic(`scale ${scale}: ${perSecond.toFixed(1)} frames/s animating at ${640 * scale} × ${480 * scale}; a reader away 1.5 s found ${backlog.length} frames (${Math.round(waiting / 1024)} KiB) waiting`);
    t.diagnostic(`scale ${scale}: ${frames().length} frames; reply during stream median ${median(replies).toFixed(1)} ms, max ${Math.max(...replies).toFixed(1)} ms; lossless ${Math.round(lossless.data.length / 1024)} KiB base64`);
  });
}

test('live view measurements at scale 2 with a 1280 × 800 viewport', { timeout: 90_000 }, async t => {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'openide-browser-bench-'));
  const fixture = liveFixture();
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
  const frames = () => bridge.events.filter(event => event.event === 'frame');
  await ok({ type: 'start', module_dir: moduleDir, executable_path: executable, width: 1280, height: 800, scale: 2 });
  await ok({ type: 'navigate', url: `${origin}/animated` });
  await ok({ type: 'stream', enabled: true });
  await pause(500);
  const first = frames().length;
  const started = performance.now();
  await pause(3000);
  const animated = frames().slice(first);
  const rate = animated.length / ((performance.now() - started) / 1000);
  assert.deepEqual(frameSize(animated.at(-1)), [2560, 1600]);
  assert.ok(rate >= 10, `only ${rate.toFixed(1)} frames/s on an animating page`);
  const jpeg = median(animated.map(frame => frame.data.length));

  // Wheel to first frame, on a page that is otherwise still.
  await ok({ type: 'navigate', url: `${origin}/still` });
  const latencies = [];
  const replies = [];
  for (let index = 0; index < 20; index++) {
    await pause(450);
    const before = frames().at(-1)?.seq ?? 0;
    const sent = performance.now();
    const reply = bridge.command({ type: 'scroll', dx: 0, dy: 120, include_aria: false, include_image: false })
      .then(response => { replies.push(performance.now() - sent); return response; });
    let frame;
    while (!(frame = frames().find(candidate => candidate.seq > before)) && performance.now() - sent < 2000) await pause(1);
    assert.ok(frame, 'a wheel step is followed by a frame');
    latencies.push(frame.at - sent);
    const response = await reply;
    assert.equal(response.ok, true);
    assert.equal(response.snapshot, undefined, 'the window\'s wheel does not wait for page state');
  }
  // The pointer over a still page: each move is answered in about a frame.
  const moves = [];
  for (let index = 0; index < 30; index++) {
    const sent = performance.now();
    await bridge.command({ type: 'pointer_move', x: 100 + index * 5, y: 300 });
    moves.push(performance.now() - sent);
  }
  await pause(500);
  const lossless = frames().filter(frame => frame.format === 'png').at(-1);
  assert.deepEqual(frameSize(lossless), [2560, 1600]);
  const settled = lossless.at - frames().filter(frame => frame.format === 'jpeg' && frame.seq < lossless.seq).at(-1).at;
  if (process.env.OPENIDE_BROWSER_E2E_ARTIFACTS) {
    await fs.mkdir(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, { recursive: true });
    for (const frame of [animated.at(-1), lossless]) {
      await fs.writeFile(path.join(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, `frame-2560x1600.${frame.format === 'png' ? 'png' : 'jpg'}`), Buffer.from(frame.data, 'base64'));
    }
  }
  assert.equal((await bridge.command({ type: 'close' })).ok, true);
  await bridge.exited;
  t.diagnostic(`2560 × 1600 frames: ${rate.toFixed(1)} frames/s animating, JPEG ${Math.round(jpeg / 1024)} KiB base64, lossless ${Math.round(lossless.data.length / 1024)} KiB base64, ${settled.toFixed(0)} ms after the last frame`);
  t.diagnostic(`wheel to first frame: median ${median(latencies).toFixed(1)} ms, p90 ${percentile(latencies, 0.9).toFixed(1)} ms, max ${Math.max(...latencies).toFixed(1)} ms; wheel reply median ${median(replies).toFixed(1)} ms; pointer move round trip median ${median(moves).toFixed(1)} ms`);
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

// ---- Agent tools: refs, explicit actions, page observers and bounded output ----

const toolsPage = `<!doctype html><html lang="es"><head><title>Herramientas</title><style>
  body{font:16px sans-serif;margin:16px}button,input,select{font:inherit;margin:2px}
  #drag,#drop{display:inline-block;width:120px;height:32px;border:1px solid #333}iframe{width:200px;height:48px}
  </style></head><body><h1>Herramientas</h1>
  <button id="a" onclick="out.textContent='A pulsado'">Borrar</button>
  <button id="b" onclick="out.textContent='B pulsado'">Borrar</button>
  <button id="temp" onclick="this.remove()">Efímero</button>
  <input aria-label="Correo">
  <button id="menu" onmouseenter="out.textContent='Sobre el menú'">Menú</button>
  <select aria-label="Color" onchange="out.textContent='Color '+this.value"><option value="r">Rojo</option><option value="g">Verde</option></select>
  <label><input type="checkbox" id="terms" onchange="out.textContent='Acepto '+this.checked"> Acepto</label>
  <div id="drag" draggable="true" ondragstart="event.dataTransfer.setData('text/plain','carta')">Carta</div>
  <div id="drop" ondragover="event.preventDefault()" ondrop="event.preventDefault();this.textContent='Soltada '+event.dataTransfer.getData('text/plain')">Zona</div>
  <button id="later" onclick="setTimeout(()=>{out.textContent='Listo tarde'},300)">Tardar</button>
  <button id="confirm" onclick="out.textContent='Confirmó '+confirm('¿Borrar todo?')">Confirmar</button>
  <button id="prompt" onclick="out.textContent='Respondió '+prompt('¿Nombre?','x')">Preguntar</button>
  <p id="out">Sin cambios</p><p style="display:none">Texto oculto</p>
  <iframe src="/frame" title="Marco"></iframe>
  <script>const out=document.querySelector('#out')</script><script src="/missing.js"></script>
  <script>console.log('carga',{n:1});console.warn('aviso');console.error('falla');console.debug('depura');
  fetch('/api/items?token=visible',{method:'POST',body:'cuerpo secreto',headers:{'X-Secret':'cabecera secreta'}});
  setTimeout(()=>{throw new Error('sin capturar')},0)</script></body></html>`;

// One fixture server and one production bridge per test, as in the cases above.
async function agentBrowser(t) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'openide-browser-tools-e2e-'));
  const fixture = http.createServer((req, res) => {
    res.setHeader('Content-Type', 'text/html; charset=utf-8');
    const url = new URL(req.url, 'http://fixture');
    if (url.pathname === '/redirect') { res.writeHead(302, { Location: '/tools' }).end(); return; }
    if (url.pathname === '/missing.js') { res.writeHead(404).end('no'); return; }
    if (url.pathname === '/api/items') {
      res.writeHead(200, { 'Content-Type': 'application/json', 'X-Reply-Secret': 'cabecera de respuesta' }).end('{"secret":"respuesta secreta"}');
      return;
    }
    if (url.pathname === '/frame') { res.end(`<!doctype html><title>Marco</title><button onclick="this.textContent='Dentro pulsado'">Dentro</button>`); return; }
    if (url.pathname === '/tall') { res.end('<!doctype html><title>Alta</title><body style="margin:0"><div style="height:1500px;background:linear-gradient(#fff,#369)">Arriba</div><p>Fondo</p>'); return; }
    // Noise does not compress: the full page is far above the capture limit.
    if (url.pathname === '/noise') {
      res.end(`<!doctype html><title>Ruido</title><body style="margin:0"><canvas width="640" height="2400"></canvas><script>
        const context=document.querySelector('canvas').getContext('2d'),image=context.createImageData(640,2400);
        for(let i=0;i<image.data.length;i++)image.data[i]=i%4===3?255:Math.random()*256;context.putImageData(image,0,0)</script>`);
      return;
    }
    if (url.pathname === '/quiet') { res.end('<!doctype html><title>Silencio</title><h1>Silencio</h1>'); return; }
    res.end(toolsPage);
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
  // A tool's own answer: an ordinary reply that carries `output`.
  const output = async command => {
    const snapshot = await ok(command);
    assert.ok(snapshot.output, `${command.type} returns output`);
    return snapshot.output;
  };
  const failure = async (command, pattern) => {
    const snapshot = await ok(command);
    assert.match(snapshot.output?.error ?? '', pattern, `${command.type} is answered as a tool failure`);
    assert.equal(snapshot.screenshot, undefined);
    assert.equal(snapshot.aria_snapshot, undefined);
    return snapshot.output.error;
  };
  const evaluate = async (expression, target = {}) => {
    const result = await output({ type: 'evaluate', expression, ...target });
    assert.equal(result.error, undefined, result.error);
    return result;
  };
  assert.equal((await ok({ type: 'start', module_dir: moduleDir, executable_path: executable, width: 640, height: 480 })).url, 'about:blank');
  return { origin, bridge, ok, output, failure, evaluate };
}

function refOf(snapshot, label) {
  const found = new RegExp(`${label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}[^\n]*\\[ref=((?:f\\d+)?e\\d+)\\]`).exec(snapshot.aria_snapshot);
  assert.ok(found, `${label} carries a ref in:\n${snapshot.aria_snapshot}`);
  return found[1];
}

test('agent tools act on snapshot refs and answer stale or ambiguous targets without touching the page', { timeout: 120_000 }, async t => {
  const { origin, ok, output, failure, evaluate } = await agentBrowser(t);
  // Every agent-facing result carries the AI-mode tree, so an action returns fresh refs.
  let snapshot = await ok({ type: 'navigate', url: origin + '/tools' });
  assert.match(snapshot.aria_snapshot, /button "Efímero" \[ref=e\d+\]/);
  assert.ok(Buffer.byteLength(snapshot.aria_snapshot) <= 64 * 1024);
  snapshot = await ok({ type: 'snapshot' });
  const heading = refOf(snapshot, 'heading "Herramientas"');
  const second = [...snapshot.aria_snapshot.matchAll(/button "Borrar" \[ref=(e\d+)\]/g)].map(match => match[1]);
  assert.equal(second.length, 2, 'both buttons have their own ref');

  // A ref names exactly one of two elements that share role and name.
  snapshot = await ok({ type: 'click_locator', ref: second[1] });
  assert.match(snapshot.aria_snapshot, /B pulsado/);
  assert.equal(refOf(snapshot, 'heading "Herramientas"'), heading, 'an element keeps its ref across snapshots');
  const ambiguous = await failure({ type: 'click_locator', role: 'button', name: 'Borrar' }, /coincide con 2 elementos/);
  assert.match(ambiguous, /browser_snapshot/);
  await failure({ type: 'click_locator', selector: '#absent' }, /coincide con 0 elementos/);
  assert.equal((await evaluate('document.querySelector("#out").textContent')).text, '"B pulsado"', 'a refused target changed nothing');

  // User input omits ARIA and leaves the agent's refs usable.
  const quiet = await ok({ type: 'click', x: 600, y: 440, include_aria: false });
  assert.equal(quiet.aria_snapshot, undefined);
  await ok({ type: 'fill', ref: refOf(snapshot, 'textbox "Correo"'), text: 'ada@example.test' });
  assert.equal((await evaluate('el => el.value', { selector: 'input[aria-label=Correo]' })).text, '"ada@example.test"');

  // A removed element's ref goes stale; so do refs that never existed.
  const temporary = refOf(snapshot, 'button "Efímero"');
  snapshot = await ok({ type: 'click_locator', ref: temporary });
  assert.doesNotMatch(snapshot.aria_snapshot, /Efímero/);
  const stale = await failure({ type: 'click_locator', ref: temporary }, new RegExp(`La referencia ${temporary} no corresponde`));
  assert.match(stale, /Tomá una captura nueva con browser_snapshot/);
  await failure({ type: 'fill', ref: 'e99999', text: 'x' }, /La referencia e99999 no corresponde/);
  await failure({ type: 'hover', ref: 'f9e1' }, /La referencia f9e1 no corresponde/);
  await failure({ type: 'drag', from: { ref: heading }, to: { ref: 'e99999' } }, /La referencia e99999 no corresponde/);
  await failure({ type: 'evaluate', expression: 'el => { window.touched = true; }', ref: temporary }, /no corresponde/);
  assert.equal((await evaluate('window.touched')).text, 'undefined', 'the script of a stale target never ran');

  // Refs reach inside an iframe.
  const inner = refOf(snapshot, 'button "Dentro"');
  assert.match(inner, /^f\d+e\d+$/);
  snapshot = await ok({ type: 'click_locator', ref: inner });
  assert.match(snapshot.aria_snapshot, /Dentro pulsado/);

  // Explicit actions, each by a different way of naming its element.
  snapshot = await ok({ type: 'hover', ref: refOf(snapshot, 'button "Menú"') });
  assert.match(snapshot.aria_snapshot, /Sobre el menú/);
  snapshot = await ok({ type: 'select_option', role: 'combobox', name: 'Color', values: ['Verde'] });
  assert.match(snapshot.aria_snapshot, /Color g/);
  snapshot = await ok({ type: 'check', selector: '#terms', checked: true });
  assert.match(snapshot.aria_snapshot, /Acepto true/);
  assert.match(snapshot.aria_snapshot, /checkbox "Acepto" \[checked\]/);
  snapshot = await ok({ type: 'check', ref: refOf(snapshot, 'checkbox "Acepto"'), checked: false });
  assert.match(snapshot.aria_snapshot, /Acepto false/);
  snapshot = await ok({ type: 'drag', from: { selector: '#drag' }, to: { selector: '#drop' } });
  assert.match(snapshot.aria_snapshot, /Soltada carta/);

  // Waiting: text that arrives, text already gone, a fixed pause, and a wait that runs out.
  await ok({ type: 'click_locator', selector: '#later' });
  snapshot = await ok({ type: 'wait_for', text: 'Listo tarde' });
  assert.match(snapshot.aria_snapshot, /Listo tarde/);
  await ok({ type: 'wait_for', text_gone: 'Soltada nunca' });
  await ok({ type: 'wait_for', text_gone: 'Texto oculto' });
  let started = Date.now();
  await ok({ type: 'wait_for', seconds: 0.3 });
  assert.ok(Date.now() - started >= 290);
  started = Date.now();
  await failure({ type: 'wait_for', text: 'Texto oculto' }, /no apareció después de 10 s/);
  assert.ok(Date.now() - started >= 9_000, 'hidden text does not count as visible');

  // Evaluate: values, functions, an element, errors as text and a bounded result.
  assert.deepEqual(await evaluate('document.title'), { text: '"Herramientas"', data: { truncated: false } });
  assert.equal((await evaluate('() => ({ n: 1, list: [1, "dos"] })')).text, '{"n":1,"list":[1,"dos"]}');
  assert.equal((await evaluate('async () => { await new Promise(done => setTimeout(done, 20)); return 7; }')).text, '7');
  assert.equal((await evaluate('el => el.textContent', { ref: heading })).text, '"Herramientas"');
  assert.equal((await evaluate('el => el.id', { role: 'button', name: 'Confirmar' })).text, '"confirm"');
  assert.equal((await evaluate('document.querySelector("#out").click()')).text, 'undefined');
  assert.equal((await evaluate('typeof process + "/" + typeof require')).text, '"undefined/undefined"', 'no Node access from a page script');
  assert.equal(await failure({ type: 'evaluate', expression: '(() => { throw new Error("explota"); })()' }, /explota/), 'Error: explota');
  await failure({ type: 'evaluate', expression: 'noExiste + 1' }, /^ReferenceError: noExiste is not defined$/);
  await failure({ type: 'evaluate', expression: 'function (' }, /^SyntaxError/);
  const oversized = await evaluate('() => "é".repeat(60000)');
  assert.equal(oversized.data.truncated, true);
  assert.ok(Buffer.byteLength(oversized.text) <= 16 * 1024 && Buffer.byteLength(oversized.text) > 16 * 1024 - 4);
  assert.ok(!oversized.text.includes('\ufffd'), 'the cut never splits a character');
  const nested = depth => `() => { let value = 1; for (let level = 0; level < ${depth}; level++) value = [value]; return value; }`;
  assert.equal((await evaluate(nested(60))).text, '['.repeat(60) + '1' + ']'.repeat(60), 'nested page data travels as text');
  await failure({ type: 'evaluate', expression: nested(500) }, /Cannot serialize result/);
  started = Date.now();
  await failure({ type: 'evaluate', expression: 'new Promise(() => {})' }, /El script no terminó en 10 s/);
  assert.ok(Date.now() - started < 20_000);
  snapshot = await ok({ type: 'snapshot' });
  assert.match(snapshot.title, /Herramientas/, 'the page is still served after an abandoned script');
});

test('agent tools report console, network, dialogs and screenshots within their bounds', { timeout: 120_000 }, async t => {
  const { origin, ok, output, failure, evaluate } = await agentBrowser(t);
  const consoleEntries = async level => (await output({ type: 'console_messages', ...(level ? { level } : {}) })).data.console;
  const requests = async () => (await output({ type: 'network_requests' })).data.network;
  await ok({ type: 'navigate', url: origin + '/tools' });
  await ok({ type: 'wait_for', seconds: 0.5 });

  // Console: levels, uncaught errors and where each message came from.
  let log = await consoleEntries();
  assert.equal(log.omitted, 0);
  assert.ok(log.entries.some(entry => entry.level === 'info' && entry.type === 'log' && /^carga/.test(entry.text) && entry.location === `${origin}/tools:20:19`), JSON.stringify(log.entries));
  assert.ok(log.entries.some(entry => entry.level === 'warning' && entry.text === 'aviso'));
  assert.ok(log.entries.some(entry => entry.level === 'error' && entry.type === 'error' && entry.text === 'falla'));
  assert.ok(log.entries.some(entry => entry.level === 'error' && entry.type === 'pageerror' && /sin capturar/.test(entry.text)));
  assert.ok(log.entries.some(entry => entry.level === 'error' && /404/.test(entry.text) && entry.location.startsWith(`${origin}/missing.js`)));
  assert.ok(!log.entries.some(entry => entry.text === 'depura'), 'debug is below the default level');
  assert.ok((await consoleEntries('debug')).entries.some(entry => entry.level === 'debug' && entry.text === 'depura'));
  const errors = await consoleEntries('error');
  assert.ok(errors.entries.length >= 3 && errors.entries.every(entry => entry.level === 'error'));
  const response = await ok({ type: 'console_messages' });
  assert.equal(response.screenshot, undefined, 'reading the console takes no capture');
  assert.equal(response.aria_snapshot, undefined);

  // Network: method, URL, status, type and failure, and nothing else.
  await evaluate('fetch("http://127.0.0.1:1/caido").catch(() => "sin conexión")');
  await evaluate('fetch("/api/items?q=" + "z".repeat(2000)).then(reply => reply.status)');
  let network = await requests();
  const find = pattern => network.entries.find(entry => pattern.test(entry.url));
  assert.deepEqual(network.entries[0], { method: 'GET', url: `${origin}/tools`, resource_type: 'document', status: 200 });
  assert.deepEqual(find(/token=visible/), { method: 'POST', url: `${origin}/api/items?token=visible`, resource_type: 'fetch', status: 200 }, 'URLs are returned as they are');
  assert.equal(find(/missing\.js/).status, 404);
  assert.equal(find(/\/frame$/).resource_type, 'document');
  assert.match(find(/caido/).failure, /^net::ERR_/);
  assert.equal(find(/caido/).status, undefined);
  assert.equal(Buffer.byteLength(find(/q=zzz/).url), 512, 'a long URL is cut at its bound');
  for (const entry of network.entries) {
    assert.deepEqual(Object.keys(entry).filter(key => !['method', 'url', 'resource_type', 'status', 'failure'].includes(key)), []);
  }
  assert.doesNotMatch(JSON.stringify(network), /cuerpo secreto|cabecera secreta|respuesta secreta|cabecera de respuesta|X-Secret/i);

  // Both buffers are bounded: the newest entries that fit, and a count of the rest.
  await evaluate('() => { for (let i = 0; i < 450; i++) console.log(`linea ${i} ` + "y".repeat(180)); console.log("ñ".repeat(4000)); }');
  log = await consoleEntries();
  assert.ok(log.omitted > 250, `omitted ${log.omitted}`);
  assert.ok(log.entries.length > 20 && log.entries.length < 200);
  assert.ok(Buffer.byteLength(JSON.stringify(log.entries)) <= 12 * 1024 + 2);
  const newest = log.entries.at(-1);
  assert.ok(Buffer.byteLength(newest.text) <= 1024 && /^ñ+$/.test(newest.text), 'the newest entry is kept and its text is cut');
  assert.match(log.entries.at(-2).text, /^linea 449 /);
  await evaluate('() => Promise.all(Array.from({ length: 260 }, (_, i) => fetch("/api/items?n=" + i)))');
  network = await requests();
  assert.ok(network.omitted > 100, `omitted ${network.omitted}`);
  assert.ok(Buffer.byteLength(JSON.stringify(network.entries)) <= 12 * 1024 + 2);
  assert.ok(network.entries.every(entry => entry.status === 200));

  // Dialogs are dismissed by default and each one is recorded.
  let snapshot = await ok({ type: 'click_locator', selector: '#confirm' });
  assert.match(snapshot.aria_snapshot, /Confirmó false/);
  const dialogs = async () => (await consoleEntries('warning')).entries.filter(entry => entry.type === 'dialog');
  assert.deepEqual((await dialogs()).at(-1), { level: 'warning', type: 'dialog', dialog: 'confirm', action: 'dismissed', text: '¿Borrar todo?' });
  // Armed for the next dialog only.
  assert.deepEqual((await output({ type: 'handle_dialog', accept: true, prompt_text: 'Ada 🧠' })).data, { next_dialog: 'accept' });
  snapshot = await ok({ type: 'click_locator', selector: '#prompt' });
  assert.match(snapshot.aria_snapshot, /Respondió Ada 🧠/);
  assert.deepEqual((await dialogs()).at(-1), { level: 'warning', type: 'dialog', dialog: 'prompt', action: 'accepted', text: '¿Nombre?' });
  snapshot = await ok({ type: 'click_locator', selector: '#confirm' });
  assert.match(snapshot.aria_snapshot, /Confirmó false/, 'the answer was used once');
  await ok({ type: 'handle_dialog', accept: true });
  snapshot = await ok({ type: 'click_locator', selector: '#confirm' });
  assert.match(snapshot.aria_snapshot, /Confirmó true/);

  // A navigation starts both buffers over and disarms an unused answer; its redirect does not.
  await ok({ type: 'handle_dialog', accept: true });
  await ok({ type: 'navigate', url: origin + '/redirect' });
  await ok({ type: 'wait_for', seconds: 0.5 });
  network = await requests();
  assert.deepEqual(network.entries.slice(0, 2).map(entry => [entry.url, entry.status]), [[`${origin}/redirect`, 302], [`${origin}/tools`, 200]]);
  assert.equal(network.omitted, 0);
  assert.ok(!find(/caido/) && !network.entries.some(entry => /n=\d/.test(entry.url)));
  log = await consoleEntries('debug');
  assert.ok(!log.entries.some(entry => /^linea/.test(entry.text) || entry.type === 'dialog'));
  assert.ok(log.entries.some(entry => /^carga/.test(entry.text)));
  snapshot = await ok({ type: 'click_locator', selector: '#confirm' });
  assert.match(snapshot.aria_snapshot, /Confirmó false/, 'an answer armed for the previous page is forgotten');
  await ok({ type: 'navigate', url: origin + '/quiet' });
  assert.deepEqual(await consoleEntries('debug'), { omitted: 0, entries: [] });

  // Screenshots: viewport, element and full page as PNG at CSS scale.
  const capture = async command => {
    const snapshot = await ok({ type: 'take_screenshot', ...command });
    assert.equal(snapshot.output.error, undefined, snapshot.output.error);
    assert.equal(snapshot.screenshot, undefined, 'only the requested image travels in this reply');
    const bytes = Buffer.from(snapshot.output.image.base64, 'base64');
    assert.equal(snapshot.output.image.mime_type, 'image/png');
    assert.deepEqual(bytes.subarray(0, 8), Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
    assert.ok(bytes.length <= 2 * 1024 * 1024);
    assert.deepEqual(snapshot.output.data.screenshot, { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20), bytes: bytes.length });
    return { ...snapshot.output.data.screenshot, bytes };
  };
  await ok({ type: 'navigate', url: origin + '/tall' });
  const viewport = await capture({});
  assert.deepEqual([viewport.width, viewport.height], [640, 480]);
  const page = await capture({ full_page: true });
  assert.equal(page.width, 640);
  assert.ok(page.height >= 1500, `full page height ${page.height}`);
  snapshot = await ok({ type: 'snapshot' });
  const element = await capture({ ref: refOf(snapshot, 'paragraph') });
  assert.ok(element.width <= 640 && element.height > 0 && element.height < 100, `element ${element.width}x${element.height}`);
  if (process.env.OPENIDE_BROWSER_E2E_ARTIFACTS) {
    await fs.mkdir(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, { recursive: true });
    await fs.writeFile(path.join(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, 'browser-tool-full-page.png'), page.bytes);
    await fs.writeFile(path.join(process.env.OPENIDE_BROWSER_E2E_ARTIFACTS, 'browser-tool-element.png'), element.bytes);
  }
  await failure({ type: 'take_screenshot', ref: 'e99999' }, /La referencia e99999 no corresponde/);
  // An element that cannot be captured is this tool's answer, not a page error.
  await ok({ type: 'evaluate', expression: '() => { document.querySelector("p").style.display = "none"; }' });
  const hidden = await failure({ type: 'take_screenshot', selector: 'p' }, /^Timeout [\d.]+ms exceeded/);
  assert.doesNotMatch(hidden, /\n/);
  // Above the limit the capture is refused with a way forward, and the page stays usable.
  await ok({ type: 'navigate', url: origin + '/noise' });
  const refused = await ok({ type: 'take_screenshot', full_page: true });
  assert.match(refused.output.error, /supera el límite de 2 MiB; capturá el viewport o un elemento/);
  assert.equal(refused.output.image, undefined);
  const noisy = await capture({});
  assert.deepEqual([noisy.width, noisy.height], [640, 480]);
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
  for (const phrase of ['browser_snapshot', '[ref=e12]', 'snapshot again after a navigation', 'over browser_evaluate', 'untrusted data']) {
    assert.ok(initialized.result.instructions.includes(phrase), `server instructions carry: ${phrase}`);
  }
  const list = await proxy.request({ jsonrpc: '2.0', method: 'tools/list' });
  assert.equal(list.result.tools[0].name, 'browser_snapshot');
  const called = await proxy.request({ jsonrpc: '2.0', method: 'tools/call', params: { name: 'browser_snapshot', arguments: {} } });
  assert.equal(called.result.content[0].text, 'localhost fixture');
  assert.equal(received.length, 2);
});
