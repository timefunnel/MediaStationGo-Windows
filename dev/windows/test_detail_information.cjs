// Exercises the complete production shell with an explicit, offline native bridge fixture.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');

const root = path.resolve(__dirname, '../..');
const web = path.join(root, 'src/web');
const preview = fs.readFileSync(path.join(__dirname, 'frontend-preview.ps1'), 'utf8');
const shim = preview.match(/\$PreviewShim = @'\r?\n([\s\S]*?)\r?\n'@/)[1];

function configureFixture() {
    const image = (key) => ({ key, type: 'primary' });
    const movie = {
        id: 'detail-movie', type: 'Movie', title: '远方的回声 · 界面验证样例', playable: true,
        year: 2026, communityRating: 8.5, officialRating: 'PG-13', durationMs: 8700000,
        overview: '一段来自深空的旧录音，让一名年轻的研究员重新踏上寻找父亲的旅程。沿着信号留下的线索，她逐渐发现，记忆中的那次告别，隐藏着另一个答案。',
        genres: ['科幻', '剧情', '冒险'], isFavorite: false,
        videoWidth: 3840, videoHeight: 1600, videoCodec: 'hevc', dynamicRange: 'HDR10',
        primaryImage: image('movie-poster'), backdropImage: image('movie-backdrop'),
    };
    const series = { ...movie, id: 'detail-series', type: 'Series', title: '夜航档案 · 界面验证样例', playable: false, videoWidth: 7680, videoHeight: 4320, videoCodec: 'hevc', dynamicRange: 'HDR10' };
    const anime = { ...series, id: 'detail-anime', title: '星海旅人 · 长篇动漫验证样例', genres: ['动画', '冒险'] };
    const seasonEpisodes = (item, season, count, ongoing) => Array.from({ length: count }, (_, index) => ({
        id: `${item.id}-s${season}-e${index + 1}`, type: 'Episode', title: `旅程的第 ${index + 1} 站`, playable: true,
        seriesId: item.id, seriesName: item.title, seasonId: `${item.id}-season-${season}`, parentIndexNumber: season,
        indexNumber: index + 1, durationMs: 1440000, played: index + 1 < ongoing,
        resumePositionMs: index + 1 === ongoing ? 480000 : 0,
        lastPlayedAt: index + 1 === ongoing ? '2026-10-26T01:00:00Z' : null,
        ...(season === 0 ? {} : item === anime
            ? { videoWidth: 1280, videoHeight: 720, videoCodec: 'av1', dynamicRange: 'SDR' }
            : season === 1 ? { videoWidth: 3840, videoHeight: 2160, videoCodec: 'hevc', dynamicRange: 'HDR10' }
                : { videoWidth: 1920, videoHeight: 1080, videoCodec: 'h264', dynamicRange: 'SDR' }),
        primaryImage: image(`episode-${index + 1}`),
    }));
    const episodeSets = {
        'detail-series-season-0': seasonEpisodes(series, 0, 2, 0),
        'detail-series-season-1': seasonEpisodes(series, 1, 12, 10),
        'detail-series-season-2': seasonEpisodes(series, 2, 12, 8),
        'detail-anime-season-1': seasonEpisodes(anime, 1, 181, 121),
    };
    const unknown = { ...movie, id: 'detail-unknown', title: '暂无技术信息', videoWidth: null, videoHeight: null, videoCodec: null, dynamicRange: null };
    const library = { id: 'fixture-library', type: 'CollectionFolder', title: '验证媒体库', collectionType: 'movies' };
    const sources = [
        { id: 'source-4k', width: 3840, height: 1600, videoCodec: 'hevc', dynamicRange: 'HDR10', container: 'mkv', bitDepth: 10, bitrate: 42000000, sizeBytes: 24 * 1024 ** 3, supportsDirectPlay: true },
        { id: 'source-1080', width: 1920, height: 800, videoCodec: 'h264', dynamicRange: 'SDR', container: 'mp4', bitDepth: 8, bitrate: 8000000, sizeBytes: 5 * 1024 ** 3, supportsDirectPlay: true },
        { id: 'source-unknown', supportsDirectPlay: true },
    ];
    const people = [{ id: 'director', name: '林远', type: 'Director', primaryImage: image('portrait-director') },
        ...Array.from({ length: 18 }, (_, index) => ({ id: 'person-' + index, name: ['陈星', '周宁', '叶舟'][index % 3] + (index > 2 ? ' ' + index : ''), role: '角色 ' + (index + 1), type: 'Actor', primaryImage: image('portrait-' + index) }))];
    const original = window.jmpNative.mediaStationCatalog;
    const originalImage = window.jmpNative.mediaStationImage;
    window.detailFixture = { imageRequests: [], catalogRequests: [], failSeasonOnce: false, unwatched: new URLSearchParams(location.search).has('unwatched') };
    window.jmpNative.mediaStationImage = (requestId, raw) => {
        window.detailFixture.imageRequests.push(JSON.parse(raw).key);
        return originalImage(requestId, raw);
    };
    window.jmpNative.mediaStationCatalog = (requestId, operation, raw) => {
        const input = JSON.parse(raw || '{}');
        window.detailFixture.catalogRequests.push({ operation, ...input });
        let payload;
        if (operation === 'home') payload = { libraries: [library], resume: window.detailFixture.unwatched ? [] : [episodeSets['detail-series-season-2'][7]], latest: [movie, series, anime, unknown], latestSections: [{ library, items: [movie, series, anime, unknown] }] };
        else if (operation === 'detail') {
            const item = [movie, series, anime, unknown].find((card) => card.id === input.mediaId);
            const seasons = item === series ? [0, 1, 2].map((indexNumber) => ({ id: `${item.id}-season-${indexNumber}`, indexNumber, episodeCount: indexNumber ? 12 : 2 }))
                : item === anime ? [{ id: 'detail-anime-season-1', indexNumber: 1, episodeCount: 181 }] : [];
            payload = { item, sources: item === movie ? sources : [], people: item === unknown ? [] : people, seasons, episodes: [], seasonCount: item === series ? 2 : item === anime ? 1 : 0, episodeCount: item === series ? 24 : item === anime ? 181 : 0 };
        } else if (operation === 'series_episodes') {
            if (window.detailFixture.failSeasonOnce && input.seasonId === 'detail-series-season-1') {
                window.detailFixture.failSeasonOnce = false;
                window.setTimeout(() => window._onMediaStationResponse(requestId, operation, false, JSON.stringify({ code: 'fixture_failure', message: '剧集读取失败验证' })), 0);
                return;
            }
            const episodes = episodeSets[input.seasonId] || [];
            payload = { episodes: window.detailFixture.unwatched ? episodes.map((episode) => ({ ...episode, played: false, resumePositionMs: 0, lastPlayedAt: null })) : episodes };
        } else return original(requestId, operation, raw);
        window.setTimeout(() => window._onMediaStationResponse(requestId, operation, true, JSON.stringify(payload)), 0);
    };
}

async function openFixture(page, mediaId = 'detail-movie', query = '') {
    await page.goto('http://detail-ui.test/' + query);
    await page.locator(`.media-card[data-media-id="${mediaId}"]`).first().click();
    await page.waitForSelector('.detail-title');
    await page.locator('#content').evaluate((node) => Promise.all(node.getAnimations().map((animation) => animation.finished)));
}

async function backdropMetrics(page) {
    await page.waitForSelector('.detail-backdrop-image.image-ready');
    return page.locator('.detail-backdrop-image').evaluate((node) => {
        const style = getComputedStyle(node);
        return { width: node.offsetWidth, height: node.offsetHeight, image: style.backgroundImage, size: style.backgroundSize, position: style.backgroundPosition, transform: style.transform };
    });
}

async function assertEpisodeWheelScrolling(page, label) {
    const stage = page.locator('.episode-stage');
    const scrollPoint = () => page.evaluate(() => {
        const content = document.getElementById('content');
        const stage = document.querySelector('.episode-stage');
        const canvas = content.getBoundingClientRect();
        content.scrollTop += Math.max(0, stage.getBoundingClientRect().top - canvas.bottom + 100);
        const box = stage.getBoundingClientRect();
        return { x: box.left + box.width / 2, y: (Math.max(box.top, canvas.top) + Math.min(box.bottom, canvas.bottom)) / 2 };
    });
    await page.locator('#content').evaluate((node) => { node.scrollTop = 0; });
    assert.equal(await stage.evaluate((node) => node.scrollHeight - node.clientHeight), 0, `${label}: pagination must not introduce a nested vertical scroller`);
    let point = await scrollPoint();
    await page.mouse.move(point.x, point.y);
    const beforeDown = await page.locator('#content').evaluate((node) => node.scrollTop);
    await page.mouse.wheel(0, 240);
    await page.waitForFunction((before) => document.getElementById('content').scrollTop > before, beforeDown, { timeout: 2000 }).catch(async () => {
        const state = await page.evaluate(({ point, beforeDown }) => {
            const content = document.getElementById('content'), stage = document.querySelector('.episode-stage');
            const ancestors = [];
            for (let node = document.elementFromPoint(point.x, point.y); node; node = node.parentElement) {
                const style = getComputedStyle(node);
                ancestors.push({ tag: node.tagName, class: node.className, overflow: style.overflowY, overscroll: style.overscrollBehaviorY, top: node.scrollTop, max: node.scrollHeight - node.clientHeight });
            }
            return { width: innerWidth, beforeDown, pageTop: content.scrollTop, pageMax: content.scrollHeight - content.clientHeight, innerTop: stage.scrollTop, innerMax: stage.scrollHeight - stage.clientHeight, point, ancestors, bounds: stage.getBoundingClientRect().toJSON() };
        }, { point, beforeDown });
        assert.fail(`${label}: wheel over the episode area must scroll the detail page at the lower boundary: ${JSON.stringify(state)}`);
    });
    await page.waitForTimeout(200);
    point = await scrollPoint();
    await page.mouse.move(point.x, point.y);
    const beforeUp = await page.locator('#content').evaluate((node) => node.scrollTop);
    await page.mouse.wheel(0, -180);
    await page.waitForFunction((before) => document.getElementById('content').scrollTop < before, beforeUp, { timeout: 2000 }).catch(() => {
        assert.fail(`${label}: wheel over the episode area must scroll the detail page at the upper boundary`);
    });
    await page.waitForTimeout(200);
    await page.locator('#content').evaluate((node) => { node.scrollTop = 0; });
}

async function main() {
    const browser = await chromium.launch({ executablePath: process.env.VIDEO_UI_CHROME || 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true });
    let scenarios = 0;
    try {
        for (const scale of [1, 1.5]) {
            for (const width of [320, 375, 414, 768, 1280]) {
                const page = await browser.newPage({ viewport: { width, height: 800 }, deviceScaleFactor: scale });
                await page.emulateMedia({ reducedMotion: 'reduce' });
                const errors = [];
                page.on('pageerror', (error) => errors.push(error.message));
                await page.route('**/*', async (route) => {
                    const url = new URL(route.request().url());
                    if (url.hostname !== 'detail-ui.test') return route.abort();
                    if (url.pathname === '/preview-shim.js') return route.fulfill({ contentType: 'application/javascript', body: shim + '\n(' + configureFixture.toString() + ')();' });
                    if (url.pathname === '/') {
                        const html = fs.readFileSync(path.join(web, 'mediastation.html'), 'utf8').replace('<script src="mediastation.js"></script>', '<script src="/preview-shim.js"></script><script src="mediastation.js"></script>');
                        return route.fulfill({ contentType: 'text/html; charset=utf-8', body: html });
                    }
                    const asset = path.join(web, path.basename(url.pathname));
                    if (!fs.existsSync(asset)) return route.fulfill({ status: 404, body: '' });
                    const types = { '.js': 'application/javascript', '.css': 'text/css', '.png': 'image/png', '.jpg': 'image/jpeg' };
                    return route.fulfill({ contentType: types[path.extname(asset)], body: fs.readFileSync(asset) });
                });
                await openFixture(page);
                await page.waitForSelector('.person-portrait');
                for (const theme of ['light', 'dark']) {
                    await page.evaluate((value) => { document.documentElement.dataset.uiTheme = value; }, theme);
                    assert.equal(await page.locator('.detail-poster .media-quality-badge').innerText(), '4K');
                    assert.equal(await page.locator('.detail-media-disclosure').getAttribute('open'), null);
                    assert.equal(await page.locator('.detail-media-specs').isVisible(), false);
                    await page.locator('summary.detail-media-heading').click();
                    assert.match(await page.locator('.detail-media-info').innerText(), /3840 × 1600/);
                    assert.match(await page.locator('.detail-media-info').innerText(), /42\.0 Mbps/);
                    await page.locator('summary.detail-media-heading').click();
                    assert.equal(await page.locator('.people-section').getAttribute('open'), '', 'cast is expanded by default');
                    assert.equal(await page.locator('.people-section .detail-credits').count(), 1, 'credits stay within the cast section');
                    assert.equal(await page.locator('.person-portrait').count(), 19);
                    const geometry = await page.evaluate(() => {
                        const content = document.getElementById('content');
                        const source = document.querySelector('.detail-media-info').getBoundingClientRect();
                        const actions = document.querySelector('.detail-actions').getBoundingClientRect();
                        const spans = [...document.querySelectorAll('.detail-copy > .hero-meta > span')];
                        const overlaps = spans.slice(1).some((span, index) => {
                            const dot = getComputedStyle(span, '::before');
                            const box = span.getBoundingClientRect(), previous = spans[index].getBoundingClientRect();
                            return Math.abs(previous.top - box.top) < 2 && box.left + parseFloat(dot.left) < previous.right;
                        });
                        return { overflow: content.scrollWidth - content.clientWidth, sourceRight: source.right, sourceBottom: source.bottom, actionsBottom: actions.bottom, viewport: innerWidth, overlaps };
                    });
                    assert.ok(geometry.overflow <= 1 && geometry.sourceRight <= width, `horizontal overflow: ${JSON.stringify({ width, scale, theme, geometry })}`);
                    assert.equal(geometry.overlaps, false, 'metadata separators must keep clear of the text');
                    if (width === 1280) assert.ok(geometry.sourceBottom < 800 && geometry.actionsBottom < 800, 'playback and technical details fit the first viewport');
                    if (process.argv[2] && scale === 1 && [375, 1280].includes(width)) {
                        fs.mkdirSync(process.argv[2], { recursive: true });
                        await page.screenshot({ path: path.join(process.argv[2], `detail-information-${theme}-${width}.png`) });
                    }
                    scenarios += 1;
                }
                await page.locator('.detail-media-info .meta-switch').click();
                await page.locator('[data-source-id="source-1080"]').click();
                assert.equal(await page.locator('.detail-poster .media-quality-badge').innerText(), '1080p');
                await page.locator('summary.detail-media-heading').click();
                assert.match(await page.locator('.detail-media-info').innerText(), /1920 × 800/);
                assert.match(await page.locator('.detail-media-info').innerText(), /8\.0 Mbps/);
                assert.doesNotMatch(await page.locator('.detail-media-info').innerText(), /4K|HEVC|HDR10|24 GB/);
                await page.locator('summary.detail-media-heading').click();
                await page.locator('.detail-media-info .meta-switch').click();
                await page.locator('[data-source-id="source-unknown"]').click();
                assert.equal(await page.locator('.detail-poster .media-quality-badge').count(), 0);
                assert.equal(await page.locator('.detail-media-badge').count(), 0, 'unknown selected version must not inherit other source metadata');
                await page.locator('.detail-media-info .meta-switch').click();
                await page.locator('[data-source-id="source-4k"]').click();
                assert.equal(await page.locator('.detail-poster .media-quality-badge').innerText(), '4K');
                const movieBackdrop = await backdropMetrics(page);
                await page.locator('.people-heading').focus();
                await page.keyboard.press('Enter');
                assert.equal(await page.locator('.people-section').getAttribute('open'), null, 'default-expanded cast can still collapse');
                assert.equal(await page.locator('.person-portrait').first().isVisible(), false);
                assert.deepEqual(await backdropMetrics(page), movieBackdrop, 'collapsing cast preserves backdrop framing');
                await page.keyboard.press('Enter');
                await page.waitForSelector('.person-portrait');
                assert.deepEqual(await backdropMetrics(page), movieBackdrop, 'expanding cast preserves backdrop framing');
                assert.equal(await page.locator('.detail-credits .detail-person-link').count(), 4);
                assert.equal(await page.locator('.person-card').count(), 19);
                assert.ok((await page.locator('.person-portrait').first().boundingBox()).width <= 56);
                const backdrop = await page.locator('.app-backdrop-layer').evaluateAll((nodes) => nodes.map((node) => node.style.backgroundImage));
                await page.locator('.person-card').first().focus();
                await page.waitForTimeout(150);
                assert.deepEqual(await page.locator('.app-backdrop-layer').evaluateAll((nodes) => nodes.map((node) => node.style.backgroundImage)), backdrop, 'cast focus must preserve the work backdrop');
                if (process.argv[2] && scale === 1 && width === 1280) await page.screenshot({ path: path.join(process.argv[2], 'detail-cast-expanded-dark-1280.png') });
                await page.locator('.people-heading').focus();
                await page.keyboard.press('Enter');
                assert.equal(await page.locator('.people-section').getAttribute('open'), null);
                await openFixture(page, 'detail-unknown');
                assert.equal(await page.locator('.detail-media-info').isVisible(), false);
                assert.equal(await page.locator('.detail-poster .media-quality-badge').count(), 0);
                assert.equal(await page.locator('.detail-credits').isVisible(), false);
                await openFixture(page, 'detail-series');
                assert.match(await page.locator('.detail-copy > .hero-meta').innerText(), /2 季[\s\S]*24 集/);
                await page.waitForSelector('.episodes-grid .media-card');
                assert.equal(await page.locator('.detail-media-info').getAttribute('data-media-id'), 'detail-series-s2-e8');
                assert.match(await page.locator('.detail-media-info').innerText(), /本集片源[\s\S]*第 2 季 · 第 8 集[\s\S]*1080p[\s\S]*SDR[\s\S]*H\.264/);
                assert.equal(await page.locator('.detail-poster .media-quality-badge').innerText(), '1080p');
                await page.locator('summary.detail-media-heading').click();
                assert.match(await page.locator('.detail-media-info').innerText(), /1920 × 1080/);
                assert.doesNotMatch(await page.locator('.detail-media-info').innerText(), /8K|HDR10|3840|码率|位深|文件大小/);
                await page.locator('summary.detail-media-heading').click();
                assert.equal(await page.locator('.people-section').getAttribute('open'), '', 'series cast is expanded by default');
                assert.equal(await page.locator('.episodes-grid .media-card').count(), 4, 'thumbnail view is bounded to one page');
                assert.equal(await page.locator('.episodes-grid [data-media-id="detail-series-s2-e8"]').count(), 1, 'the first page contains the resume episode');
                assert.match(await page.locator('.detail-playback-context').innerText(), /第 2 季 · 第 8 集/);
                assert.equal(await page.locator('.detail-actions .primary-command').innerText(), '继续观看');
                const seriesGeometry = await page.locator('.episodes').boundingBox();
                if (width === 1280) assert.ok(seriesGeometry.y + seriesGeometry.height < 760, 'episode selection fits the desktop first viewport');
                if (process.argv[2] && scale === 1 && [375, 1280].includes(width)) await page.screenshot({ path: path.join(process.argv[2], `detail-series-dark-${width}.png`) });
                const requestCount = await page.evaluate(() => window.detailFixture.catalogRequests.length);
                const seriesBackdrop = await backdropMetrics(page);
                await page.locator('[data-episode-view="numbers"]').click();
                assert.deepEqual(await backdropMetrics(page), seriesBackdrop, `view switching must preserve backdrop framing (${width}px, scale ${scale})`);
                await page.locator('[data-episode-view="cards"]').click();
                assert.deepEqual(await backdropMetrics(page), seriesBackdrop, 'switching back to thumbnails preserves backdrop framing');
                await page.locator('[data-episode-view="numbers"]').click();
                assert.equal(await page.locator('.episode-number-button').count(), 12);
                assert.equal(await page.locator('.episode-number-button.is-in-progress').innerText(), '8');
                assert.equal(await page.evaluate(() => window.detailFixture.catalogRequests.length), requestCount, 'local view changes must not fetch or resolve media');
                if (scale === 1 && [375, 1280].includes(width)) await assertEpisodeWheelScrolling(page, 'series');
                await page.locator('.episode-number-button').first().focus();
                await page.keyboard.press('ArrowRight');
                assert.match(await page.locator(':focus').getAttribute('data-media-id'), /s2-e2$/);
                await page.locator('.episode-number-button').first().focus();
                await page.keyboard.press('ArrowUp');
                assert.equal(await page.locator(':focus').getAttribute('data-episode-view'), 'numbers');
                await page.locator('.season-picker-button').focus();
                await page.keyboard.press('ArrowDown');
                assert.equal(await page.locator(':focus').getAttribute('data-season-id'), 'detail-series-season-2');
                await page.keyboard.press('Escape');
                assert.equal(await page.locator('.season-picker-button').getAttribute('aria-expanded'), 'false');
                await page.evaluate(() => { window.detailFixture.failSeasonOnce = true; });
                await page.locator('.season-picker-button').click();
                await page.locator('[data-season-id="detail-series-season-1"]').click();
                await page.getByText('本季剧集读取失败', { exact: true }).waitFor();
                assert.equal(await page.locator('.detail-actions .primary-command').count(), 0, 'failed season loading must not retain a previous season playback target');
                assert.equal(await page.locator('.detail-playback-context').isVisible(), false);
                assert.equal(await page.locator('.detail-media-info').isVisible(), false, 'failed season loading must clear the previous episode parameters');
                assert.equal(await page.locator('.detail-poster .media-quality-badge').count(), 0);
                await page.locator('.episode-stage').getByRole('button', { name: '重试', exact: true }).click();
                await page.waitForSelector('.episode-number-button');
                assert.equal(await page.locator('.episode-number-button').count(), 12);
                assert.match(await page.locator('.detail-playback-context').innerText(), /第 1 季 · 第 10 集/);
                assert.equal(await page.locator('.detail-media-info').getAttribute('data-media-id'), 'detail-series-s1-e10');
                assert.match(await page.locator('.detail-media-info').innerText(), /第 1 季 · 第 10 集[\s\S]*4K[\s\S]*HDR10[\s\S]*HEVC/);
                assert.equal(await page.locator('.detail-poster .media-quality-badge').innerText(), '4K');
                await page.locator('.season-picker-button').click();
                await page.locator('[data-season-id="detail-series-season-2"]').click();
                assert.equal(await page.locator('.detail-media-info').getAttribute('data-media-id'), 'detail-series-s2-e8', 'cached season uses the same episode as the playback action');
                assert.equal(await page.locator('.detail-poster .media-quality-badge').innerText(), '1080p');
                await page.locator('.season-picker-button').click();
                await page.locator('[data-season-id="detail-series-season-0"]').click();
                await page.waitForFunction(() => document.querySelectorAll('.episode-number-button').length === 2);
                assert.match(await page.locator('.season-picker-label').innerText(), /特别篇/);
                assert.equal(await page.locator('.detail-media-info').isVisible(), false, 'missing episode parameters must not borrow series or other season data');
                assert.equal(await page.locator('.detail-poster .media-quality-badge').count(), 0);
                await openFixture(page, 'detail-anime');
                await page.waitForSelector('.episode-number-button');
                assert.equal(await page.locator('.people-section').getAttribute('open'), '', 'anime cast is expanded by default');
                assert.equal(await page.locator('.episode-number-button').count(), 50);
                assert.equal(await page.locator('.episode-range-button').count(), 4);
                assert.equal(await page.locator('.episode-range-button[aria-pressed="true"]').innerText(), '101–150');
                assert.equal(await page.locator('.episode-number-button.is-in-progress').innerText(), '121');
                assert.match(await page.locator('.detail-playback-context').innerText(), /第 121 集/);
                assert.equal(await page.locator('.detail-media-info').getAttribute('data-media-id'), 'detail-anime-s1-e121');
                assert.match(await page.locator('.detail-media-info').innerText(), /第 121 集[\s\S]*720p[\s\S]*SDR[\s\S]*AV1/);
                const animeBackdrop = await backdropMetrics(page);
                if (scale === 1 && [375, 1280].includes(width)) await assertEpisodeWheelScrolling(page, 'anime');
                if (process.argv[2] && scale === 1 && [375, 1280].includes(width)) await page.screenshot({ path: path.join(process.argv[2], `detail-anime-dark-${width}.png`) });
                await page.getByRole('button', { name: '151–181', exact: true }).click();
                assert.equal(await page.locator('.episode-number-button').count(), 31);
                assert.deepEqual(await backdropMetrics(page), animeBackdrop, 'range paging preserves backdrop framing');
                assert.equal(await page.locator(':focus').innerText(), '151–181', 'range changes preserve keyboard focus');
                await page.locator('[data-episode-view="cards"]').click();
                assert.equal(await page.locator('.episodes-grid .media-card').count(), 4);
                assert.deepEqual(await backdropMetrics(page), animeBackdrop, 'anime view switching preserves backdrop framing');
                if (scale === 1 && [375, 1280].includes(width)) await assertEpisodeWheelScrolling(page, 'thumbnails');
                if (width === 1280 && scale === 1) {
                    await page.setViewportSize({ width: 1280, height: 640 });
                    await page.waitForFunction(() => document.querySelector('.detail-backdrop-image').offsetHeight === 576);
                    assert.equal((await backdropMetrics(page)).height, 576, 'backdrop follows window resizing');
                    await page.setViewportSize({ width: 1280, height: 800 });
                    await page.waitForFunction(() => document.querySelector('.detail-backdrop-image').offsetHeight === 736);
                    assert.deepEqual(await backdropMetrics(page), animeBackdrop, 'restoring window size restores the artwork framing');
                    await openFixture(page, 'detail-series', '?unwatched');
                    await page.waitForSelector('.episodes-grid .media-card');
                    assert.equal(await page.locator('.detail-actions .primary-command').innerText(), '开始观看');
                    assert.match(await page.locator('.detail-playback-context').innerText(), /第 1 季 · 第 1 集/);
                    assert.equal(await page.locator('.detail-media-info').getAttribute('data-media-id'), 'detail-series-s1-e1', 'a series with no viewing history shows the first regular season episode parameters');
                    await page.locator('.detail-back').click();
                    await page.locator('.media-card[data-media-id="detail-series"]').first().click();
                    await page.waitForSelector('.episodes-grid .media-card');
                    assert.equal(await page.locator('.detail-media-info').getAttribute('data-media-id'), 'detail-series-s1-e1', 'cached detail rendering keeps episode parameters in sync before attachment');
                }
                assert.deepEqual(errors, []);
                await page.close();
            }
        }
        console.log(JSON.stringify({ result: 'DETAIL_INFORMATION_OK', scenarios, sourceSwitch: true, missingMetadata: true, episodeParameters: true, unwatchedFirstEpisode: true, defaultExpandedCast: true, keyboardDisclosure: true, stableBackdrop: true, nativeEpisodeScrolling: true }));
    } finally { await browser.close(); }
}

main().catch((error) => { console.error(error); process.exitCode = 1; });
