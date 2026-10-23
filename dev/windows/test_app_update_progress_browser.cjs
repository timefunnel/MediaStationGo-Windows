// Run with Playwright available on NODE_PATH; an optional argument saves UI screenshots.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');

const root = path.resolve(__dirname, '../..');
const source = fs.readFileSync(path.join(root, 'src/web/mediastation.js'), 'utf8');
const html = fs.readFileSync(path.join(root, 'src/web/mediastation.html'), 'utf8');
const css = fs.readFileSync(path.join(root, 'src/web/mediastation.css'), 'utf8');
const functions = source.slice(source.indexOf('    function updateStatusText('), source.indexOf('    function requestUpdateCheck('));
const callbacks = source.slice(source.indexOf('    window._onAppUpdateStatus ='), source.indexOf('    function playContentTransition('));
const events = source.slice(source.indexOf("    byId('settings-install-app-update').addEventListener"), source.indexOf("    byId('settings-clear-image-cache').addEventListener"));
const dialog = html.match(/<dialog id="app-update-dialog"[\s\S]*?<\/dialog>/)[0];
const screenshotDir = process.argv[2];

(async () => {
    const browser = await chromium.launch({ executablePath: process.env.VIDEO_UI_CHROME || 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true });
    try {
        for (const theme of ['light', 'dark']) {
            for (const width of [1280, 390]) {
                const page = await browser.newPage({ viewport: { width, height: 800 } });
                const errors = [];
                page.on('pageerror', (error) => errors.push(error.message));
                await page.setContent(`<!doctype html><html data-ui-theme="${theme}"><head><meta charset="utf-8"><style>${css}</style></head><body>
                    <span id="settings-app-version"></span><p id="settings-app-update-status"></p>
                    <button id="settings-check-app-update">检查更新</button><button id="settings-download-app-update">下载</button>
                    <button id="settings-install-app-update">应用更新</button>${dialog}</body></html>`);
                await page.addScriptTag({ content: `
                    const byId = (id) => document.getElementById(id);
                    const formatBytes = (value) => String(value);
                    let appUpdateState = { status: 'ready', payload: { version: '1.1.0', packageKind: 'portable' } };
                    let appUpdateDialogTrigger = null;
                    window.installCalls = 0;
                    window.jmpInfo = { version: '1.0.0' };
                    window.jmpNative = { updateInstall() { window.installCalls++; } };
                    ${functions}\n${callbacks}\n${events}\nrefreshUpdateControls();
                ` });
                const modal = page.locator('#app-update-dialog');
                await page.locator('#settings-install-app-update').click();
                await page.keyboard.press('Escape');
                assert.equal(await modal.isVisible(), false, 'confirmation can be dismissed');
                await page.locator('#settings-install-app-update').click();
                await page.locator('#app-update-dialog-confirm').click();
                assert.equal(await modal.isVisible(), true);
                assert.equal(await modal.getAttribute('aria-busy'), 'true');
                assert.equal(await page.locator('#app-update-dialog-progress').isVisible(), true);
                assert.equal(await page.locator('#app-update-dialog-confirm').isVisible(), false);
                assert.equal(await page.locator('#app-update-dialog-close').isDisabled(), true);
                await page.keyboard.press('Escape');
                assert.equal(await modal.isVisible(), true, 'Escape must not hide active progress');
                await page.evaluate(() => confirmAppUpdate());
                assert.equal(await page.evaluate(() => window.installCalls), 1);
                const animation = await page.locator('#app-update-dialog-progress span').evaluate((element) => getComputedStyle(element).animationName);
                assert.equal(animation, 'app-update-progress');
                const bounds = await modal.boundingBox();
                assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= width, 'dialog fits the viewport');
                if (screenshotDir) {
                    fs.mkdirSync(screenshotDir, { recursive: true });
                    await page.screenshot({ path: path.join(screenshotDir, `update-progress-${theme}-${width}.png`) });
                }
                await page.evaluate(() => window._onAppUpdateStatus('installing', '{}'));
                assert.match(await page.locator('#app-update-dialog-copy').innerText(), /正在应用便携版更新/);
                await page.emulateMedia({ reducedMotion: 'reduce' });
                assert.equal(await page.locator('#app-update-dialog-progress').isVisible(), true);
                await page.evaluate(() => window._onAppUpdateStatus('error', JSON.stringify({ message: '更新包校验失败' })));
                assert.equal(await page.locator('#app-update-dialog-copy').innerText(), '更新包校验失败');
                assert.equal(await page.locator('#app-update-dialog-progress').isVisible(), false);
                await page.locator('#app-update-dialog-cancel').click();
                assert.equal(await modal.isVisible(), false);
                assert.deepEqual(errors, []);
                await page.close();
            }
        }
        console.log('APP_UPDATE_PROGRESS_BROWSER_OK light/dark/desktop/narrow/Escape/one-install/error/reduced-motion');
    } finally {
        await browser.close();
    }
})().catch((error) => { console.error(error); process.exitCode = 1; });
