// Complete production page with an offline bridge that releases catalog and artwork separately.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');
const web = path.resolve(__dirname, '../../src/web');
const shim = fs.readFileSync(path.join(__dirname, 'frontend-preview.ps1'), 'utf8').match(/\$PreviewShim = @'\r?\n([\s\S]*?)\r?\n'@/)[1];

function holdResponses() {
    const catalog = window.jmpNative.mediaStationCatalog;
    const image = window.jmpNative.mediaStationImage;
    const heldHomes = [], heldImages = [];
    let homeReady = false, imagesReady = false, imagesFail = false;
    const respondError = (args, operation) => window.setTimeout(() => window._onMediaStationResponse(args[0], operation, false, JSON.stringify({ code: 'fixture_error', message: '离线样例请求失败' })), 0);
    const releaseImage = (args) => {
        if (imagesFail) return respondError(args, 'image');
        const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080"><rect width="1920" height="1080" fill="#31546a"/><text x="100" y="900" fill="white" font-size="64">首页加载验证样例</text></svg>';
        window.setTimeout(() => window._onMediaStationResponse(args[0], 'image', true, JSON.stringify({ key: JSON.parse(args[1]).key, dataUrl: 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg) })), 0);
    };
    window.jmpNative.mediaStationCatalog = (...args) => {
        if (args[1] === 'home' && !homeReady) heldHomes.push(args);
        else catalog(...args);
    };
    window.jmpNative.mediaStationImage = (...args) => {
        if (Number(args[2]) >= 1280) {
            if (!imagesReady) heldImages.push(args);
            else releaseImage(args);
        } else image(...args);
    };
    window.homeLoadingFixture = {
        releaseHome(fail = false) {
            homeReady = true;
            heldHomes.splice(0).forEach(args => fail ? respondError(args, 'home') : catalog(...args));
        },
        releaseImages(fail = false) {
            imagesReady = true; imagesFail = fail;
            heldImages.splice(0).forEach(releaseImage);
        },
    };
}

async function openPage(browser, width, scale, theme) {
    const page = await browser.newPage({ viewport: { width, height: 800 }, deviceScaleFactor: scale });
    page.errors = [];
    page.on('pageerror', error => page.errors.push(error.message));
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await page.route('**/*', async route => {
        const url = new URL(route.request().url());
        if (url.hostname !== 'home-loading.test') return route.abort();
        if (url.pathname === '/preview-shim.js') return route.fulfill({ contentType: 'application/javascript', body: shim + '\n(' + holdResponses.toString() + ')();' });
        if (url.pathname === '/') return route.fulfill({ contentType: 'text/html; charset=utf-8', body: fs.readFileSync(path.join(web, 'mediastation.html'), 'utf8').replace('<script src="mediastation.js"></script>', '<script src="/preview-shim.js"></script><script src="mediastation.js"></script>') });
        const asset = path.join(web, path.basename(url.pathname));
        if (!fs.existsSync(asset)) return route.fulfill({ status: 404, body: '' });
        return route.fulfill({ contentType: ({ '.js': 'application/javascript', '.css': 'text/css', '.jpg': 'image/jpeg', '.png': 'image/png' })[path.extname(asset)], body: fs.readFileSync(asset) });
    });
    await page.goto('http://home-loading.test/');
    await page.locator('.home-loading').waitFor();
    await page.evaluate(value => { document.documentElement.dataset.uiTheme = value; }, theme);
    return page;
}

async function main() {
    const browser = await chromium.launch({ executablePath: process.env.VIDEO_UI_CHROME || 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true });
    let scenarios = 0;
    try {
        for (const width of [320, 375, 414, 768, 1280]) for (const scale of [1, 1.5]) for (const theme of ['light', 'dark']) {
            const page = await openPage(browser, width, scale, theme);
            assert.equal(await page.locator('.loading-grid').count(), 0, 'home must not show the library grid skeleton');
            assert.equal(await page.locator('.home-loading .home-skeleton-row').count(), 2);
            assert.equal(await page.locator('.home-loading button, .home-loading [tabindex]').count(), 0, 'skeletons must not enter the keyboard focus order');
            const frame = await page.locator('.home-skeleton-hero').boundingBox();
            const overflow = await page.locator('#content').evaluate(node => node.scrollWidth - node.clientWidth);
            assert.ok(overflow <= 1);
            if (process.argv[2] && scale === 1 && [375, 1280].includes(width)) await page.screenshot({ path: path.join(process.argv[2], `home-skeleton-${theme}-${width}.png`) });
            await page.evaluate(() => window.homeLoadingFixture.releaseHome());
            await page.locator('.home-loading').waitFor({ state: 'detached' });
            assert.equal(await page.locator('.home-skeleton-hero').count(), 1, 'artwork loading retains the banner space after metadata arrives');
            assert.deepEqual(await page.locator('.home-skeleton-hero').boundingBox(), frame);
            const bandBefore = await page.locator('#content > .page-band').boundingBox();
            await page.evaluate(() => window.homeLoadingFixture.releaseImages());
            await page.locator('.home-hero:not(.hero-pending):not(.home-skeleton-hero) .hero-slide.active').waitFor();
            assert.equal(await page.locator('.home-skeleton-hero').count(), 0);
            const bandAfter = await page.locator('#content > .page-band').boundingBox();
            assert.ok(Math.abs(bandAfter.y - bandBefore.y) < 1, 'banner reveal must not shift the content rows');
            assert.deepEqual(page.errors, []);
            scenarios++;
            await page.close();
        }
        const away = await openPage(browser, 1280, 1, 'light');
        await away.evaluate(() => window.homeLoadingFixture.releaseHome());
        await away.locator('.home-loading').waitFor({ state: 'detached' });
        await away.locator('#nav-library').click();
        await away.evaluate(() => window.homeLoadingFixture.releaseImages());
        await away.locator('.library-card').first().waitFor();
        assert.equal(await away.locator('.home-skeleton-hero').count(), 0, 'late artwork must not restore a home skeleton after navigation');
        await away.locator('#nav-home').click();
        await away.locator('.home-hero:not(.hero-pending):not(.home-skeleton-hero) .hero-slide.active').waitFor();
        assert.equal(await away.locator('.home-loading').count(), 0, 'cached home content must not revert to a full skeleton');
        assert.deepEqual(away.errors, []);
        await away.close();
        for (const failure of ['home', 'artwork']) {
            const page = await openPage(browser, 1280, 1, 'dark');
            await page.evaluate(fail => window.homeLoadingFixture.releaseHome(fail), failure === 'home');
            await page.locator('.home-loading').waitFor({ state: 'detached' });
            if (failure === 'artwork') await page.evaluate(() => window.homeLoadingFixture.releaseImages(true));
            await page.locator('.home-skeleton-hero').waitFor({ state: 'detached' });
            if (failure === 'home') await page.locator('.empty-state').waitFor();
            assert.deepEqual(page.errors, []);
            await page.close();
        }
        console.log(JSON.stringify({ result: 'HOME_LOADING_OK', scenarios, delayedMetadata: true, delayedArtwork: true, stableBannerSpace: true, cachedReturn: true, navigation: true, failureCleanup: true }));
    } finally { await browser.close(); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
