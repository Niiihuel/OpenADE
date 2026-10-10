// OpenIDE's scoped Playwright process. Stdout is exclusively bounded JSONL.
// Public APIs: https://playwright.dev/docs/api/class-page,
// https://playwright.dev/docs/api/class-locator#locator-aria-snapshot.
import { createRequire } from 'node:module';
import path from 'node:path';
import { once } from 'node:events';
import fs from 'node:fs/promises';
import os from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { randomUUID } from 'node:crypto';

const MAX_INPUT = 64 * 1024;
const MAX_OUTPUT = 4 * 1024 * 1024;
const MAX_SCREENSHOT = 2 * 1024 * 1024;
const MAX_ARIA = 64 * 1024;
const TIMEOUT = 10_000;
let browser;
let context;
let page;
let viewport = { width: 900, height: 700 };
let shuttingDown = false;
let temporaryDirectory;
let preparedCut;
const inheritedTemporaryDirectories = [process.env.TMPDIR, process.env.TMP, process.env.TEMP, os.tmpdir()];

async function prepareTemporaryDirectory() {
  if (temporaryDirectory) return;
  const managed = process.env.OPENIDE_BROWSER_TEMP_DIR;
  const candidates = [
    ...(managed ? [managed] : inheritedTemporaryDirectories),
    ...(process.platform === 'win32' ? [] : ['/tmp']),
  ];
  for (const base of new Set(candidates.filter(Boolean))) {
    if (!path.isAbsolute(base)) continue;
    // Chromium creates Unix sockets below its temp root (typically another
    // 45 bytes). Deep data/workspace paths exceed sockaddr_un even when they
    // are valid filesystem paths. Keep the complete owned root below 48 bytes.
    if (process.platform !== 'win32' && Buffer.byteLength(path.join(base, 'oid-XXXXXX')) > 48) continue;
    try {
      // Do not recreate a stale external nix-shell directory. The Rust actor
      // creates its own private base; standalone callers may use a live temp.
      if (!(await fs.stat(base)).isDirectory()) continue;
      const directory = await fs.mkdtemp(path.join(base, 'oid-'));
      try { await fs.chmod(directory, 0o700); }
      catch (error) { await fs.rm(directory, { recursive: true, force: true }).catch(() => {}); throw error; }
      temporaryDirectory = directory;
      for (const name of ['TMPDIR', 'TMP', 'TEMP']) process.env[name] = temporaryDirectory;
      return;
    } catch { /* Try another existing root; never disable Chromium's sandbox. */ }
  }
  throw Object.assign(new Error('No se pudo crear el directorio temporal del navegador. Cerrá esta pestaña y volvé a abrirla.'), { code: 'temporary_directory_unavailable' });
}

async function removeTemporaryDirectory() {
  const directory = temporaryDirectory;
  temporaryDirectory = undefined;
  if (directory) await fs.rm(directory, { recursive: true, force: true }).catch(() => {});
}

function limited(value, bytes) {
  const buffer = Buffer.from(String(value ?? ''), 'utf8');
  if (buffer.length <= bytes) return buffer.toString('utf8');
  let end = bytes;
  while (end > 0 && (buffer[end] & 0xc0) === 0x80) end--;
  return buffer.subarray(0, end).toString('utf8');
}

async function emit(value) {
  const line = JSON.stringify(value) + '\n';
  if (Buffer.byteLength(line) > MAX_OUTPUT) throw new Error('Browser response exceeds 4 MiB');
  if (!process.stdout.write(line)) await once(process.stdout, 'drain');
}

async function close() {
  if (shuttingDown) return;
  shuttingDown = true;
  await preparedCut?.selection.dispose().catch(() => {});
  preparedCut = undefined;
  await context?.close().catch(() => {});
  await browser?.close().catch(() => {});
  await removeTemporaryDirectory();
}

for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.once(signal, () => { void close().finally(() => process.exit(0)); });
}
process.stdin.once('end', () => { void close().finally(() => process.exit(0)); });
process.stdout.on('error', () => { void close().finally(() => process.exit(0)); });

function requirePlaywright(directory) {
  const resolver = createRequire(path.join(directory, 'package.json'));
  try { return resolver('playwright'); } catch (error) {
    const failure = new Error('Playwright no está instalado en el directorio administrado.');
    failure.code = 'playwright_missing';
    throw failure;
  }
}

function validateUrl(value) {
  if (value === 'about:blank') return value;
  const url = new URL(value);
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) {
    throw new Error('Sólo se permiten URLs HTTP/HTTPS sin credenciales incrustadas.');
  }
  return url.href;
}

function target(command) {
  const locator = command.selector
    ? page.locator(command.selector)
    : page.getByRole(command.role, { name: command.name, exact: true });
  return locator;
}

async function capture(includeAria, includeImage = true) {
  const snapshot = {
    url: limited(page.url(), 4096),
    title: limited(await page.title().catch(() => ''), 2048),
    viewport_width: viewport.width,
    viewport_height: viewport.height,
    loading: false,
  };
  try {
    const session = await context.newCDPSession(page);
    try {
      const history = await session.send('Page.getNavigationHistory');
      snapshot.can_go_back = history.currentIndex > 0;
      snapshot.can_go_forward = history.currentIndex + 1 < history.entries.length;
    } finally { await session.detach().catch(() => {}); }
  } catch {
    // History metadata is optional if a page is changing/crashing. It cannot
    // suppress the viewport capture or turn a successful click into a retry.
    snapshot.can_go_back = snapshot.can_go_forward = false;
  }
  if (includeImage) try {
    let png;
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        png = await page.screenshot({ type: 'png', fullPage: false, scale: 'css', timeout: 5000 });
        break;
      } catch (error) {
        // A freshly navigated/resized Chromium surface may not yet have a
        // compositor frame. Retry only the read-only capture, never the input
        // command that preceded it (which may already have taken effect).
        if (attempt || !/Page\.captureScreenshot.*Unable to capture screenshot/.test(String(error?.message))) throw error;
        await delay(80);
      }
    }
    if (png.length > MAX_SCREENSHOT) throw new Error('La captura supera el límite de 2 MiB.');
    snapshot.screenshot = { mime_type: 'image/png', base64: png.toString('base64') };
  } catch (error) {
    snapshot.capture_error = limited(error?.message ?? error, 1024);
  }
  if (includeAria) {
    try {
      // Stable public API since Playwright 1.49. No private snapshot engine,
      // eval from callers or opaque Chromium/browser-desktop APIs are exposed.
      snapshot.aria_snapshot = limited(await page.locator('body').ariaSnapshot({ timeout: 3000 }), MAX_ARIA);
    } catch (error) {
      snapshot.aria_error = limited(error?.message ?? error, 1024);
    }
  }
  return snapshot;
}

async function copySelection(cut) {
  await preparedCut?.selection.dispose().catch(() => {});
  preparedCut = undefined;
  const selection = await page.evaluateHandle(({ cut, maximum }) => {
    const element = document.activeElement;
    const input = element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement;
    if (input && element instanceof HTMLInputElement && element.type === 'password') {
      throw new Error('Los campos de contraseña no se copian ni se cortan.');
    }
    const editable = input && typeof element.selectionStart === 'number' && typeof element.selectionEnd === 'number';
    if (cut && (!editable || element.readOnly || element.disabled)) {
      throw new Error('Cortar sólo está disponible en campos de texto editables.');
    }
    const start = editable ? element.selectionStart : 0;
    const end = editable ? element.selectionEnd : 0;
    const text = editable ? element.value.slice(start, end) : (window.getSelection()?.toString() || '');
    if (!text) throw new Error('Seleccioná el texto que querés copiar.');
    if (text.length > maximum || new TextEncoder().encode(text).length > maximum || text.includes('\0')) {
      throw new Error('La selección supera el límite de 16 KiB; copiá una parte más pequeña.');
    }
    if (cut && element.value.length > 256 * 1024) {
      throw new Error('Este campo es demasiado extenso para cortarlo de forma segura.');
    }
    return { element: cut ? element : null, value: cut ? element.value : null, start, end, text };
  }, { cut, maximum: 16 * 1024 });
  let retained = false;
  try {
    const text = await selection.evaluate(value => value.text);
    if (cut) {
      const token = randomUUID();
      preparedCut = { token, selection };
      retained = true;
      return { text, cut_token: token };
    }
    return { text };
  } finally {
    if (!retained) await selection.dispose();
  }
}

async function confirmCut(token) {
  const prepared = preparedCut;
  preparedCut = undefined;
  if (!prepared || prepared.token !== token) {
    await prepared?.selection.dispose().catch(() => {});
    throw new Error('El corte ya no corresponde a la selección actual.');
  }
  try {
    await prepared.selection.evaluate(saved => {
      const element = saved.element;
      const unchanged = () => element?.isConnected && document.activeElement === element
        && !element.readOnly && !element.disabled && element.value === saved.value
        && element.selectionStart === saved.start && element.selectionEnd === saved.end;
      if (!unchanged()) throw new Error('El campo o su selección cambiaron; no se cortó texto.');
      const before = new InputEvent('beforeinput', { bubbles: true, cancelable: true, inputType: 'deleteByCut' });
      if (!element.dispatchEvent(before) || !unchanged()) throw new Error('La página canceló el corte; no se cortó texto.');
      element.setRangeText('', saved.start, saved.end, 'start');
      element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteByCut' }));
    });
  } finally { await prepared.selection.dispose().catch(() => {}); }
}

async function execute(command) {
  switch (command.type) {
    case 'start': {
      if (browser) return capture(true);
      await prepareTemporaryDirectory();
      const playwright = requirePlaywright(command.module_dir);
      if (typeof playwright.chromium?.launch !== 'function') {
        throw Object.assign(new Error('La instalación de Playwright no ofrece Chromium.'), { code: 'playwright_missing' });
      }
      viewport = { width: command.width, height: command.height };
      try {
        browser = await playwright.chromium.launch({
          headless: true, chromiumSandbox: true, timeout: 20_000,
          ...(command.executable_path ? { executablePath: command.executable_path } : {}),
        });
        context = await browser.newContext({ viewport, deviceScaleFactor: 1, acceptDownloads: false });
        context.setDefaultTimeout(TIMEOUT);
        context.setDefaultNavigationTimeout(TIMEOUT);
        page = await context.newPage();
        if (typeof page.locator('body').ariaSnapshot !== 'function') {
          throw Object.assign(new Error('Se requiere Playwright 1.49 o posterior.'), { code: 'playwright_outdated' });
        }
        context.on('page', popup => {
          // A single panel has one authoritative page. Extra windows are
          // closed rather than silently receiving an unobserved agent scope.
          if (popup !== page) void popup.close().catch(() => {});
        });
        page.on('dialog', dialog => { void dialog.dismiss().catch(() => {}); });
      } catch (error) {
        await context?.close().catch(() => {});
        await browser?.close().catch(() => {});
        browser = context = page = undefined;
        await removeTemporaryDirectory();
        if (/executable.*exist|playwright install|browser.*not found/i.test(String(error?.message))) {
          error.code = 'browser_missing';
        }
        throw error;
      }
      return capture(true);
    }
    case 'navigate': await page.goto(validateUrl(command.url), { waitUntil: 'domcontentloaded', timeout: TIMEOUT }); break;
    case 'back': await page.goBack({ waitUntil: 'domcontentloaded', timeout: TIMEOUT }); break;
    case 'forward': await page.goForward({ waitUntil: 'domcontentloaded', timeout: TIMEOUT }); break;
    case 'reload': await page.reload({ waitUntil: 'domcontentloaded', timeout: TIMEOUT }); break;
    case 'click': await page.mouse.click(command.x, command.y); break;
    case 'click_locator': {
      const locator = target(command);
      if (await locator.count() !== 1) throw new Error('El destino debe identificar exactamente un elemento; tomá una nueva captura.');
      await locator.click({ timeout: TIMEOUT });
      break;
    }
    case 'type': await page.keyboard.insertText(command.text); break;
    case 'fill': {
      const locator = target(command);
      if (await locator.count() !== 1) throw new Error('El destino debe identificar exactamente un campo.');
      await locator.fill(command.text, { timeout: TIMEOUT });
      break;
    }
    case 'press': await page.keyboard.press(command.key); break;
    case 'copy_selection': {
      const clipboard = await copySelection(false);
      return { ...await capture(false, false), clipboard };
    }
    case 'prepare_cut': {
      const clipboard = await copySelection(true);
      return { ...await capture(false, false), clipboard };
    }
    case 'confirm_cut': await confirmCut(command.token); break;
    case 'scroll': await page.mouse.wheel(command.dx, command.dy); break;
    case 'resize': viewport = { width: command.width, height: command.height }; await page.setViewportSize(viewport); break;
    case 'snapshot': return capture(true);
    case 'refresh': return capture(false);
    case 'close': await close(); return { closed: true };
    default: throw new Error('Comando de navegador desconocido.');
  }
  return capture(command.include_aria !== false);
}

// Consume one command at a time. Pausing stdin applies backpressure instead
// of accumulating promises when an action or screenshot is slow.
let input = Buffer.alloc(0);
let processing = false;
async function drain() {
  if (processing) return;
  processing = true;
  process.stdin.pause();
  try {
    for (;;) {
      const newline = input.indexOf(10);
      if (newline < 0) break;
      const line = input.subarray(0, newline);
      input = input.subarray(newline + 1);
      let request;
      try {
        request = JSON.parse(line.toString('utf8'));
        if (!Number.isSafeInteger(request.id) || request.id < 1 || !request.command) throw new Error('Invalid browser request');
        const snapshot = await execute(request.command);
        await emit({ id: request.id, ok: true, snapshot });
      } catch (error) {
        let snapshot;
        if (page && !page.isClosed()) snapshot = await capture(false).catch(() => undefined);
        await emit({ id: request?.id ?? 0, ok: false, code: limited(error?.code ?? 'command_failed', 64), error: limited(error?.message ?? error, 2048), snapshot });
      }
      if (shuttingDown) { process.exit(0); return; }
    }
  } catch (error) {
    process.stderr.write(limited(error?.message ?? error, 1024) + '\n');
    await close();
    process.exit(1);
  } finally {
    processing = false;
    if (!shuttingDown) process.stdin.resume();
  }
}
process.stdin.on('data', chunk => {
  if (input.length + chunk.length > MAX_INPUT) {
    process.stderr.write('Browser input exceeds 64 KiB\n');
    void close().finally(() => process.exit(1));
    return;
  }
  input = Buffer.concat([input, chunk]);
  void drain();
});
