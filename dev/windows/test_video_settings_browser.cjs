// Browser validation of actual offline markup and handlers, with explicit test states.
const assert = require('node:assert/strict');
const path = require('node:path');
const fs = require('node:fs');
const { pathToFileURL } = require('node:url');
const { chromium } = require('playwright');
const root = path.resolve(__dirname, '../..');
const fixture = path.join(root, 'build/video-parameters-ui');
async function choose(page, key, value) {
    await page.locator(`[data-enhancement-key="${key}"]`).click();
    await page.locator(`#player-enhancement-${key}-options [role="option"][value="${value}"]`).click();
}
async function main() {
    const browser = await chromium.launch({ executablePath: process.env.VIDEO_UI_CHROME || 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true });
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
    const errors = []; page.on('pageerror', (error) => errors.push(error.message));
    const evidence = [];
    try {
        for (const theme of ['light', 'dark']) {
            await page.goto(pathToFileURL(path.join(fixture, `components-${theme}.html`)).href);
            await page.waitForSelector('html[data-rendered="true"]');
            const installed = page.locator('[data-component-id="dlssnr"] [data-component-action="primary"]');
            assert.equal(await installed.isDisabled(), true);
            assert.equal(await installed.evaluate((button) => getComputedStyle(button).cursor), 'default');
            assert.equal(await page.locator('.component-card button').count(), 8);
            const bottom = await page.locator('.component-grid').evaluate((grid) => grid.getBoundingClientRect().bottom);
            assert.ok(bottom < 800, 'all cards must fit the desktop viewport');
            await page.evaluate(() => fixture.updateVideoComponents({ components: [{ id: 'rtx-video', installed: false, supported: true }] }));
            const install = page.locator('[data-component-id="rtx-video"] [data-component-action="primary"]');
            await install.click();
            assert.equal(await page.locator('#component-dialog').evaluate((dialog) => dialog.open), true);
            assert.equal(await page.locator('#component-dialog-local').isVisible(), true);
            assert.equal(await page.locator('.component-grid').evaluate((grid) => grid.getBoundingClientRect().bottom), bottom, 'modal never expands the card');
            await page.keyboard.press('Escape');
            assert.equal(await install.evaluate((button) => document.activeElement === button), true, 'Escape restores focus');
            await page.screenshot({ path: path.join(fixture, `video-components-${theme}.png`) });
            await page.evaluate(() => fixture.updateVideoComponents({ components: [{ id: 'rtx-video', installed: false, supported: true, busy: true }] }));
            assert.equal(await install.evaluate((button) => getComputedStyle(button).cursor), 'progress');
            await page.evaluate(() => fixture.updateVideoComponents({ components: [{ id: 'rtx-video', installed: false, supported: true, restartRequired: true, pendingOperation: 'install' }] }));
            assert.equal(await install.textContent(), '重启生效');
            assert.equal(await page.locator('.component-card button').count(), 8);
            assert.equal(await page.locator('#component-dialog').evaluate((dialog) => dialog.open), false);
            await install.click(); assert.match(await page.locator('#component-dialog-copy').textContent(), /停止当前播放/);
            await page.locator('#component-dialog-cancel').click();
            await page.setViewportSize({ width: 480, height: 800 });
            assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
            await page.screenshot({ path: path.join(fixture, `components-${theme}-narrow.png`), fullPage: true });
            await page.setViewportSize({ width: 1280, height: 800 });
            await page.goto(pathToFileURL(path.join(fixture, `parameters-${theme}.html`)).href);
            await page.waitForSelector('html[data-rendered="true"]');
            assert.equal(await page.locator('[data-enhancement-group]').count(), 4);
            assert.equal(await page.getByRole('button', { name: '应用', exact: true }).count(), 1);
            assert.equal(await page.getByRole('button', { name: '应用', exact: true }).isDisabled(), true);
            await page.locator('[data-enhancement-group="nr"] summary').click();
            await choose(page, 'nrStyle', '2');
            assert.equal(await page.evaluate(() => fixture.calls.length), 0);
            const footer = await page.locator('.enhancement-footer').boundingBox();
            assert.ok(footer.y + footer.height <= 800, 'common apply stays visible while parameters scroll');
            await page.screenshot({ path: path.join(fixture, `video-parameters-${theme}.png`) });
            await choose(page, 'nrSkin', '1.5');
            await page.locator('[data-enhancement-key="nrUiCorrection"]').check();
            const scrolledFooter = await page.locator('.enhancement-footer').boundingBox();
            assert.equal(scrolledFooter.y, footer.y, 'scrolling to the last NR control does not move the apply footer');
            await page.getByRole('button', { name: '应用', exact: true }).click();
            assert.equal(await page.evaluate(() => fixture.calls.length), 1);
            assert.equal(await page.evaluate(() => fixture.calls[0].selection.nrStyle), 2);
            assert.equal(await page.evaluate(() => fixture.calls[0].selection.nrSkin), 1.5);
            assert.equal(await page.evaluate(() => fixture.calls[0].selection.nrUiCorrection), true);
            await page.locator('[data-enhancement-key="backend"][value="rife"]').check();
            assert.equal(await page.locator('#player-enhancement-rifeModel-options [role="option"]').count(), 3);
            assert.equal(await page.evaluate(() => fixture.player.enhancementDraft.selection.vfg), false);
            await page.setViewportSize({ width: 480, height: 800 });
            assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
            await page.screenshot({ path: path.join(fixture, `parameters-${theme}-narrow.png`) });
            await page.setViewportSize({ width: 1280, height: 800 });
            evidence.push({ theme, cards: 4, fixedActions: 8, desktopFits: true, modalNoExpansion: true, keyboardFocus: true, groupedSettings: 4, singleApply: true, narrowNoOverflow: true });
        }
        assert.deepEqual(errors, [], 'browser console errors');
        fs.writeFileSync(path.join(fixture, 'browser-validation.json'), JSON.stringify({ source: 'actual UI with offline test states', evidence }, null, 2));
        console.log('VIDEO_SETTINGS_BROWSER_OK light/dark/cursor/fixed-actions/modal/Escape-focus/groups/draft/single-apply/narrow');
    } finally { await browser.close(); }
}
main().catch((error) => { console.error(error); process.exitCode = 1; });
