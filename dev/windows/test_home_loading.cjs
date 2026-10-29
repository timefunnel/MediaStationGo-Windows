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
        const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="1280" height="720"><rect width="1280" height="720" fill="#31546a"/><text x="100" y="600" fill="white" font-size="48">首页加载验证样例</text></svg>';
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
        const widths = process.env.HOME_UI_WIDTH ? [Number(process.env.HOME_UI_WIDTH)] : [320, 375, 414, 768, 1280];
        const independent = await openPage(browser, 1280, 1, 'dark');
        await independent.evaluate(() => window.homeLoadingFixture.releaseImages());
        await independent.locator('.home-loading .home-hero:not(.hero-pending) .hero-slide.active img').waitFor();
        await independent.evaluate(() => { window.earlyHero = document.querySelector('.home-hero:not(.home-skeleton-hero)'); });
        assert.equal(await independent.locator('.home-loading').count(), 1, 'banner must be visible while home metadata is still pending');
        await independent.evaluate(() => {
            const respond = window._onMediaStationResponse;
            let homes = 0;
            window._onMediaStationResponse = (id, operation, ok, json) => {
                if (operation === 'home' && ok) {
                    const data = JSON.parse(json); data.cache = { status: 'hit' };
                    if (++homes > 1) data.resume[0].resumePositionMs += 1000;
                    json = JSON.stringify(data);
                }
                const response = respond(id, operation, ok, json);
                if (homes > 1) window.homeFixtureUpdated = true;
                return response;
            };
            window.homeLoadingFixture.releaseHome();
        });
        await independent.locator('.home-loading').waitFor({ state: 'detached' });
        assert.ok(await independent.evaluate(() => document.querySelector('.home-hero:not(.home-skeleton-hero)') === window.earlyHero), 'home metadata must preserve the loaded banner and its image');
        await independent.waitForFunction(() => window.homeFixtureUpdated);
        assert.ok(await independent.evaluate(() => document.querySelector('.home-hero:not(.home-skeleton-hero)') === window.earlyHero), 'later home refreshes must preserve the current banner');
        assert.equal(await independent.locator('.home-skeleton-hero').count(), 0, 'home synchronization must not blank an already loaded banner');
        assert.deepEqual(independent.errors, []); await independent.close();

        const earlyNavigation = await openPage(browser, 1280, 1, 'dark');
        await earlyNavigation.evaluate(() => window.homeLoadingFixture.releaseImages());
        await earlyNavigation.locator('.home-loading .hero-slide.active .hero-primary').click();
        await earlyNavigation.locator('.detail-layout').waitFor();
        await earlyNavigation.evaluate(() => window.homeLoadingFixture.releaseHome());
        assert.equal(await earlyNavigation.locator('.home-featured').count(), 0, 'late home metadata must not replace a detail page opened from the early banner');
        assert.deepEqual(earlyNavigation.errors, []); await earlyNavigation.close();
        const stalledBanner = await openPage(browser, 1280, 1, 'light');
        await stalledBanner.evaluate(() => window.homeLoadingFixture.releaseHome());
        await stalledBanner.locator('.home-loading').waitFor({ state: 'detached' });
        await stalledBanner.locator('.home-band img.image-ready').first().waitFor({ timeout: 5000 });
        assert.equal(await stalledBanner.locator('.home-skeleton-hero').count(), 1, 'a stalled banner must leave covers usable once its short priority budget expires');
        assert.deepEqual(stalledBanner.errors, []); await stalledBanner.close();
        for (const width of widths) for (const scale of [1, 1.25, 1.5, 2]) for (const theme of ['light', 'dark']) {
            const page = await openPage(browser, width, scale, theme);
            assert.equal(await page.locator('.loading-grid').count(), 0, 'home must not show the library grid skeleton');
            assert.equal(await page.locator('.home-loading .home-featured .home-skeleton-row').count(), 1);
            assert.equal(await page.locator('.home-loading .home-latest-band .home-skeleton-row').count(), 1);
            assert.equal(await page.locator('.home-loading button, .home-loading [tabindex]').count(), 0, 'skeletons must not enter the keyboard focus order');
            const frame = await page.locator('.home-skeleton-hero').boundingBox();
            const overflow = await page.locator('#content').evaluate(node => node.scrollWidth - node.clientWidth);
            assert.ok(overflow <= 1);
            if (process.argv[2] && scale === 1 && [375, 1280].includes(width)) await page.screenshot({ path: path.join(process.argv[2], `home-skeleton-${theme}-${width}.png`) });
            await page.evaluate(() => window.homeLoadingFixture.releaseHome());
            await page.locator('.home-loading').waitFor({ state: 'detached' });
            assert.equal(await page.locator('.home-skeleton-hero').count(), 1, 'artwork loading retains the banner space after metadata arrives');
            assert.deepEqual(await page.locator('.home-skeleton-hero').boundingBox(), frame);
            const bandBefore = await page.locator('.home-featured > .home-band').boundingBox();
            await page.evaluate(() => window.homeLoadingFixture.releaseImages());
            await page.locator('.home-hero:not(.hero-pending):not(.home-skeleton-hero) .hero-slide.active').waitFor();
            assert.equal(await page.locator('.home-skeleton-hero').count(), 0);
            const artwork = await page.locator('.hero-slide.active .hero-image').evaluate(image => {
                const bounds = image.getBoundingClientRect();
                // CSS bounds can be fractional; compare integer display pixels.
                return { width: Math.round(bounds.width * devicePixelRatio), height: Math.round(bounds.height * devicePixelRatio), naturalWidth: image.naturalWidth, naturalHeight: image.naturalHeight };
            });
            assert.ok(artwork.width <= artwork.naturalWidth * 2 && artwork.height <= artwork.naturalHeight * 2, 'banner artwork must respect the enlargement limit');
            const bandAfter = await page.locator('.home-featured > .home-band').boundingBox();
            assert.ok(Math.abs(bandAfter.y - bandBefore.y) < 1, 'banner reveal must not shift the content rows');
            assert.equal(await page.locator('.home-band .media-section').count(), 1, 'home must only retain the continue-watching section');
            assert.match(await page.locator('.home-band h2').textContent(), /^继续观看/);
            assert.ok(await page.locator('.home-latest-band [data-row-key^="latest"]').count() > 0, 'recent additions must remain available below the fold');
            if (process.argv[2] && scale === 1 && [375, 1280].includes(width)) await page.screenshot({ path: path.join(process.argv[2], `home-loaded-${theme}-${width}.png`) });
            if (width === 1280) {
                const featured = await page.locator('.home-featured').boundingBox();
                const recent = await page.locator('.home-latest-band').boundingBox();
                assert.ok(featured.y + featured.height <= 801, 'banner and continue-watching must fit the first desktop screen: ' + JSON.stringify(featured));
                assert.ok(recent.y >= 799, 'recent additions must begin below the first screen');
                const banner = await page.locator('.home-hero:not(.home-skeleton-hero)').boundingBox();
                assert.ok(banner.height >= 400 || scale >= 2, 'standard-density desktop banner must be visibly larger');
                await page.locator('#content').evaluate(node => { node.scrollTop = node.clientHeight; });
                const scrolled = await page.locator('.home-latest-band').boundingBox();
                assert.ok(scrolled.y < 800, 'scrolling must reveal recent additions');
            }
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
        const emptyResume = await openPage(browser, 1280, 1.5, 'light');
        await emptyResume.evaluate(() => {
            const respond = window._onMediaStationResponse;
            window._onMediaStationResponse = (id, operation, ok, json) => {
                if (operation === 'home' && ok) { const data = JSON.parse(json); data.resume = []; json = JSON.stringify(data); }
                return respond(id, operation, ok, json);
            };
            window.homeLoadingFixture.releaseHome(); window.homeLoadingFixture.releaseImages();
        });
        await emptyResume.locator('.home-hero:not(.hero-pending):not(.home-skeleton-hero) .hero-slide.active').waitFor();
        await emptyResume.locator('.home-loading').waitFor({ state: 'detached' });
        assert.equal(await emptyResume.locator('.home-band .media-section').count(), 0);
        assert.equal(await emptyResume.locator('.home-band .empty-state').textContent(), '暂无继续观看的作品');
        assert.deepEqual(emptyResume.errors, []);
        await emptyResume.close();
        const resizing = await openPage(browser, 1280, 1.5, 'light');
        await resizing.evaluate(() => { window.homeLoadingFixture.releaseHome(); window.homeLoadingFixture.releaseImages(); });
        await resizing.locator('.home-hero:not(.hero-pending):not(.home-skeleton-hero) .hero-slide.active').waitFor();
        await resizing.locator('.home-loading').waitFor({ state: 'detached' });
        await resizing.evaluate(() => { window.resizingHero = document.querySelector('.home-featured .home-hero:not(.home-skeleton-hero)'); });
        const sizes = [[2560,1377],[1920,1080],[1440,900],[1280,800],[1024,700],[1280,600],[900,650],[768,600],[600,700],[375,700],[375,600],[1024,500],[1280,800]];
        for (const [width, height] of sizes) {
            await resizing.setViewportSize({ width, height });
            await resizing.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(resolve)))));
            await resizing.locator('.home-hero:not(.hero-pending):not(.home-skeleton-hero) .hero-slide.active').waitFor();
            const layout = await resizing.evaluate(() => {
                const content = document.getElementById('content'), hero = document.querySelector('.home-featured .home-hero:not(.home-skeleton-hero)');
                const image = hero.querySelector('.hero-slide.active img'), bounds = image.getBoundingClientRect(), frame = hero.getBoundingClientRect();
                const command = hero.querySelector('.hero-primary').getBoundingClientRect(), featured = document.querySelector('.home-featured').getBoundingClientRect();
                const subtitle = document.querySelector('.home-band .card-subtitle').getBoundingClientRect();
                return { sameHero: hero === window.resizingHero, overflow: content.scrollWidth - content.clientWidth, resumeBottom: subtitle.bottom,
                    frame: frame.toJSON(), command: command.toJSON(), featured: featured.toJSON(), recentTop: document.querySelector('.home-latest-band').getBoundingClientRect().y,
                    width: Math.round(bounds.width * devicePixelRatio), height: Math.round(bounds.height * devicePixelRatio), naturalWidth: image.naturalWidth, naturalHeight: image.naturalHeight,
                };
            });
            assert.ok(layout.sameHero, 'resizing must preserve the current home and its recommendation selection');
            assert.ok(layout.overflow <= 1, 'resized home must not overflow horizontally');
            assert.ok(layout.command.top >= layout.frame.top && layout.command.bottom <= layout.frame.bottom, 'banner actions must remain inside the banner');
            assert.ok(layout.resumeBottom <= height + 1, 'continue-watching artwork and text must remain fully visible: ' + JSON.stringify({width,height,layout}));
            assert.ok(layout.width <= layout.naturalWidth * 2 && layout.height <= layout.naturalHeight * 2);
            assert.ok(layout.recentTop >= height - 1, 'recent additions must remain below the first screen after resizing');
            assert.ok(layout.featured.bottom <= height + 1, 'the first screen must exactly fit the new window height: ' + JSON.stringify({width,height,layout}));
            if (process.argv[2] && [600,900,2560].includes(width)) await resizing.screenshot({ path: path.join(process.argv[2], `home-resized-${width}-${height}.png`) });
        }
        assert.deepEqual(resizing.errors, []); await resizing.close();
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
        console.log(JSON.stringify({ result: 'HOME_LOADING_OK', scenarios, resizeScenarios: sizes.length, independentBanner: true, earlyNavigation: true, delayedMetadata: true, delayedArtwork: true, stableBannerSpace: true, cachedReturn: true, navigation: true, emptyResume: true, failureCleanup: true }));
    } finally { await browser.close(); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
