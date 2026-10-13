// Browser regression against a disposable local server, with synthetic language responses.
// Build: cargo build --release -p af-mcp --bin af-server
// Browser setup: npm install --no-save --prefix target/browser-tests playwright
// Run: PLAYWRIGHT_MODULE=target/browser-tests/node_modules/playwright/index.mjs node scripts/test-sysop-scroll.mjs
// SYSOP_JS=web/sysop.js tests source before rebuilding; omit it to test the shipped binary.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { mkdir, mkdtemp, readFile, open } from 'node:fs/promises';
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE
  ? pathToFileURL(resolve(process.env.PLAYWRIGHT_MODULE)).href : 'playwright');
await mkdir(resolve(root, 'target'), { recursive: true });
const dir = await mkdtemp(resolve(root, 'target/sysop-scroll-'));
const socket = createServer();
await new Promise(resolve => socket.listen(0, '127.0.0.1', resolve));
const port = socket.address().port;
await new Promise(resolve => socket.close(resolve));
const url = `http://127.0.0.1:${port}`;
const key = randomBytes(16).toString('hex');
const env = { ...process.env };
for (const name of Object.keys(env)) if (name.startsWith('AF_') || name === 'DATABASE_URL') delete env[name];
const log = await open(resolve(dir, 'server.log'), 'w');
const server = spawn(resolve(root, 'target/release/af-server'), ['serve', '--db', resolve(dir, 'world.redb'),
  '--bind', `127.0.0.1:${port}`, '--admin-token', key, '--sectors', '80', '--tick-secs', '3600', '--backup-every-mins', '0'],
  { cwd: root, env, stdio: ['ignore', log.fd, log.fd] });
let serverError;
server.on('error', error => { serverError = error; });
async function until(test, message, timeout = 10000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    if (await test()) return;
    await delay(50);
  }
  assert.fail(message);
}
let browser;
try {
  await until(async () => {
    if (serverError) throw serverError;
    if (server.exitCode !== null) throw new Error(`Test server exited; see ${dir}/server.log`);
    try { return (await fetch(`${url}/healthz`)).ok; } catch { return false; }
  }, 'Test server did not start');
  browser = await chromium.launch({ headless: true });
  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    const context = await browser.newContext({ viewport });
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    if (process.env.SYSOP_JS) {
      const body = await readFile(resolve(process.env.SYSOP_JS), 'utf8');
      await page.route('**/sysop.js', route => route.fulfill({ contentType: 'text/javascript', body }));
    }
    const version = (n, lines) => ({ version: n, call_sign: 'Test Captain', tick: n, season: 1,
      explanation: `Explanation for version ${n}.\n`.repeat(lines), examples: 'Example text.' });
    const language = { name: 'Zeta', creator: 1, creator_name: 'Test Captain', share: 'public', adopters: [],
      versions: [version(1, 100), version(2, 1)] };
    let languages = [language], failed = false, polls = 0, heldRequest = null, holdNext = false;
    page.on('response', r => { if (new URL(r.url()).pathname === '/api/sysop/state' && r.ok()) polls++; });
    await page.route('**/api/sysop/languages', async route => {
      const json = structuredClone({ languages });
      if (holdNext) {
        holdNext = false;
        await new Promise(resolve => { heldRequest = resolve; });
      }
      await route.fulfill({ status: failed ? 503 : 200, json: failed ? { error: 'Temporary test error' } : json });
    });
    // The retirement preview (above the list, in the same scroller), fetched with ?preview=1.
    const candidate = (id) => ({ id, name: `Lang ${id}`, creator_name: 'Test Captain', share: 'captains', adopters: 0,
      tagged: 0, idle_days: 40, would: 'retire' });
    let candidates = [1, 2, 3, 4, 5].map(candidate);
    await page.route('**/api/sysop/languages?*', route => route.fulfill({ json: { ...structuredClone({ languages }),
      preview: { source: 'rules', stale_days: 14, retire_days: 30, grace_days: 30,
        counts: { active: 1, stale: 0, retire: candidates.length }, candidates: structuredClone(candidates) } } }));
    await page.goto(`${url}/sysop#languages`);
    // WebKit (Safari, the iPad and iPhone console) has no CSS scroll anchoring; turn it off here so
    // Chromium checks the anchoring the console does by hand.
    await page.addStyleTag({ content: '* { overflow-anchor: none !important; }' });
    await page.locator('#gate-key').fill(key);
    await page.locator('#gate-go').click();
    await page.locator('#lg-list details').first().waitFor();
    const oldVersion = () => page.locator('#lg-list details').filter({ has: page.locator('summary', { hasText: /^v1 / }) }).last();
    const position = () => page.locator('#m-languages .scroll').evaluate(el => el.scrollTop);
    const refresh = () => page.locator('#lg-refresh').evaluate(el => el.click());
    const old = await oldVersion().elementHandle();
    await old.evaluate(el => {
      el.open = true;
      el.querySelector('summary').focus();
      document.querySelector('#m-languages .scroll').scrollTop = 1200;
    });
    const top = await position();
    assert.ok(top > 500, 'Fixture must be scrolled into a long older explanation');
    const pollStart = polls;
    await until(() => polls >= pollStart + 2, 'Two background state polls did not arrive');
    await delay(100);
    assert.ok(await old.evaluate(el => el.isConnected), 'Unchanged polls must retain language DOM');
    assert.ok(await oldVersion().evaluate(el => el.open), 'An expanded older version must stay open');
    assert.ok(Math.abs(await position() - top) < 2, 'Background polls must preserve scroll');
    assert.ok(await old.evaluate(el => el.contains(document.activeElement)), 'Background polls must preserve keyboard focus');

    // Changed data: an extra card and a new version above the text being read.
    const anchorTop = (await oldVersion().boundingBox()).y;
    language.versions.push(version(3, 8));
    languages.unshift({ ...language, name: 'Alpha', creator: 2, versions: [version(1, 15)] });
    await refresh();
    await until(async () => await page.locator('#lg-list details').count() === 4, 'Refreshed language versions did not appear');
    assert.ok(await oldVersion().evaluate(el => el.open), 'Refresh must retain expanded older versions');
    assert.ok(Math.abs((await oldVersion().boundingBox()).y - anchorTop) < 2, 'Inserted content must not move the reading anchor');
    assert.ok(await oldVersion().evaluate(el => el.contains(document.activeElement)), 'Refresh must restore focus without scrolling');

    // A user-collapsed latest version remains collapsed, even after a metadata update.
    const latest = page.locator('#lg-list summary').filter({ hasText: /^v3 / });
    await latest.evaluate(el => { el.parentElement.open = false; });
    language.adopters.push(3);
    await refresh();
    await until(async () => (await page.locator('#lg-list').textContent()).includes('1 adopter'), 'Metadata update missing');
    assert.equal(await latest.evaluate(el => el.parentElement.open), false);
    const beforeError = (await oldVersion().boundingBox()).y;
    failed = true;
    await refresh();
    await page.getByText('Temporary test error', { exact: true }).waitFor();
    assert.ok(await oldVersion().evaluate(el => el.open), 'A fetch error must retain cached explanations');
    assert.ok(Math.abs((await oldVersion().boundingBox()).y - beforeError) < 2, 'Error display must preserve the reading anchor');
    failed = false;
    await refresh();
    await page.getByText('Temporary test error', { exact: true }).waitFor({ state: 'hidden' });
    assert.ok(Math.abs((await oldVersion().boundingBox()).y - beforeError) < 2, 'Recovery must preserve the reading anchor');

    // A slow earlier refresh must not overwrite newer data or reset its view state.
    holdNext = true;
    await refresh();
    await until(() => heldRequest !== null, 'Delayed request did not start');
    language.adopters.push(4);
    await refresh();
    await until(async () => (await page.locator('#lg-list').textContent()).includes('2 adopters'), 'Newer refresh missing');
    const oldReply = page.waitForResponse(r => new URL(r.url()).pathname === '/api/sysop/languages');
    heldRequest();
    await oldReply;
    await delay(100);
    assert.ok((await page.locator('#lg-list').textContent()).includes('2 adopters'), 'Stale reply overwrote a newer refresh');
    assert.ok(await oldVersion().evaluate(el => el.open));

    // The preview: selecting candidates keeps focus and the reading anchor, and a preview that
    // shrinks above the list (a retired candidate leaves it) does not move the version being read.
    const preview = () => page.locator('#lg-pv-form').evaluate(f => f.requestSubmit());
    await preview();
    await until(async () => await page.locator('#lg-pv-out input[type=checkbox]').count() === 5, 'Preview candidates did not appear');
    const pvAnchor = (await oldVersion().boundingBox()).y;
    const box2 = await page.locator('#lg-pv-out input[aria-label="Select #2"]').elementHandle();
    await box2.evaluate(el => { el.focus({ preventScroll: true }); el.click(); });
    assert.ok(await box2.evaluate(el => el.isConnected && el.checked && document.activeElement === el), 'Toggling a candidate must keep its checkbox and focus');
    assert.equal(await page.locator('#lg-pv-out button.danger').textContent(), 'Retire selected (1)');
    await page.locator('#lg-pv-out input[aria-label="Select #4"]').evaluate(el => { el.focus({ preventScroll: true }); el.click(); });
    assert.equal(await page.locator('#lg-pv-out button.danger').textContent(), 'Retire selected (2)');
    assert.ok(Math.abs((await oldVersion().boundingBox()).y - pvAnchor) < 2, 'Selecting candidates must not move the reading anchor');
    candidates = [candidate(5)];
    await preview();
    await until(async () => await page.locator('#lg-pv-out input[type=checkbox]').count() === 1, 'The shrunken preview did not appear');
    assert.ok(Math.abs((await oldVersion().boundingBox()).y - pvAnchor) < 2, 'A shrinking preview must not move the reading anchor');
    assert.ok(await oldVersion().evaluate(el => el.open), 'A preview refresh must leave the language list alone');
    assert.deepEqual(errors, [], 'Console must not throw during refresh');
    console.log(`PASS ${viewport.width}x${viewport.height}: polling, changed data, expansion, focus, scroll anchor, errors, stale replies, retirement preview`);
    await context.close();
  }
} finally {
  if (browser) await browser.close();
  if (server.exitCode === null && !serverError) {
    const exited = new Promise(resolve => server.once('exit', resolve));
    server.kill('SIGINT');
    await Promise.race([exited, delay(5000)]);
    if (server.exitCode === null) server.kill('SIGKILL');
  }
  await log.close();
}
