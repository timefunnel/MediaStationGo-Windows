// Drive the real Canvas renderer with a deterministic clock and inspect its output.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
const start = source.indexOf('    function createDanmakuRenderer(');
const end = source.indexOf('    function handleDanmakuEvent(', start);
const rendererSource = source.slice(start, end);
const defaults = { fontSize: 25, opacity: 80, displayArea: 80, scrollSeconds: 12, fixedSeconds: 4 };

function fixture(comments, { width = 1280, height = 240, position = 0, playing = true, enabled = true, style = {} } = {}) {
    let now = 0, frameId = 0, resize;
    const frames = new Map(), stats = { text: 0, images: 0 };
    const layer = { clientWidth: width, clientHeight: height, isConnected: true, replaceChildren(canvas) { this.canvas = canvas; } };
    function canvas() {
        const result = { style: {}, setAttribute() {} };
        result.context = {
            draws: [], setTransform() {},
            clearRect() { this.draws = []; },
            measureText(text) { return { width: [...text].length * 25, actualBoundingBoxLeft: 0, actualBoundingBoxRight: [...text].length * 25, actualBoundingBoxAscent: 23, actualBoundingBoxDescent: 6 }; },
            strokeText() { stats.text += 1; },
            fillText(text, x, y) { stats.text += 1; result.text = text; this.draws.push({ text, x, y, width: this.measureText(text).width, height: 32 }); },
            drawImage(image, x, y, width, height) { stats.images += 1; this.draws.push({ text: image.text, x, y, width, height }); },
        };
        result.getContext = () => result.context;
        return result;
    }
    const context = vm.createContext({
        document: { createElement: canvas }, window: { devicePixelRatio: 1 },
        performance: { now: () => now }, danmakuStyleDefaults: defaults,
        ResizeObserver: class { constructor(callback) { resize = callback; } observe() {} },
        requestAnimationFrame(callback) { frames.set(++frameId, callback); return frameId; },
        cancelAnimationFrame(id) { frames.delete(id); },
    });
    const renderer = vm.runInContext(`${rendererSource}\ncreateDanmakuRenderer`, context)(layer);
    renderer.load({ comments, style: { ...defaults, ...style }, enabled }, position, playing, 1);
    function step(milliseconds = 0) {
        now += milliseconds;
        const callbacks = [...frames.values()]; frames.clear();
        callbacks.forEach(callback => callback(now));
        return layer.canvas.context.draws;
    }
    const visible = () => layer.canvas.context.draws.filter(entry => entry.x < layer.clientWidth && entry.x + entry.width > 0);
    return { renderer, stats, layer, step, visible, pending: () => frames.size, resize: () => resize() };
}

function noOverlaps(f, label) {
    const entries = f.visible();
    for (let i = 0; i < entries.length; i += 1) {
        for (const right of entries.slice(i + 1)) {
            const left = entries[i];
            assert.ok(left.x + left.width <= right.x || right.x + right.width <= left.x
                || left.y + left.height <= right.y || right.y + right.height <= left.y,
            `${label}: ${left.text} overlaps ${right.text}`);
        }
    }
}
const comment = (text, time = 0, mode = 1) => ({ text, time, mode, color: 0xFFFFFF });
const cases = {
    'mixed directions and fixed comments share physical rows'() {
        const f = fixture(Array.from({ length: 40 }, (_, i) => comment(`弹幕${i}`, 0, [1, 4, 5, 6][i % 4])), { style: { displayArea: 100, fixedSeconds: 8 } });
        for (let i = 0; i < 420; i += 1) { f.step(1000 / 60); noOverlaps(f, `frame ${i}`); }
        assert.ok(f.stats.images > 0 || f.stats.text > 0);
    },
    'seek and delayed frames preserve chronological lane admission'() {
        const comments = Array.from({ length: 100 }, (_, i) => comment('弹'.repeat(2 + i % 13), i * 0.11));
        const f = fixture(comments, { height: 160, position: 4500 });
        f.step(); noOverlaps(f, 'initial seek');
        f.step(1100); noOverlaps(f, 'delayed frame');
        f.renderer.updatePlayback({ kind: 'seeked', positionMs: 8000 }, true);
        f.step(); noOverlaps(f, 'explicit seek');
        f.layer.clientWidth = 740; f.layer.clientHeight = 170; f.resize();
        f.step(); noOverlaps(f, 'resize');
    },
    'position sampling never jumps the animation backwards'() {
        const f = fixture([comment('时钟')]);
        f.step(1000);
        const before = f.visible()[0].x;
        f.renderer.updatePlayback({ kind: 'position', positionMs: 920 }, true);
        f.step();
        assert.ok(Math.abs(f.visible()[0].x - before) < 0.01, 'small clock corrections must not move text instantly');
        let previous = before;
        for (let i = 0; i < 30; i += 1) {
            f.step(1000 / 60); const current = f.visible()[0].x;
            assert.ok(current < previous, 'scrolling remains monotonic during clock correction'); previous = current;
        }
        f.renderer.updatePlayback({ kind: 'metadata', positionMs: 0 }, true);
        f.step(); assert.ok(Math.abs(f.visible()[0].x - previous) < 0.01, 'metadata snapshots cannot reset the timeline');
    },
    'glyphs are rasterized once while frames reuse cached images'() {
        const f = fixture([comment('缓存文字')]); f.step(1000);
        const rasterizations = f.stats.text;
        for (let i = 0; i < 60; i += 1) f.step(1000 / 60);
        assert.equal(f.stats.text, rasterizations, 'animation must not stroke/fill text every frame');
        assert.ok(f.stats.images >= 60);
    },
    'pause buffering disable and clear stop animation'() {
        const f = fixture([comment('播放状态')]); f.step(1000);
        f.renderer.updatePlayback({ kind: 'buffering', positionMs: 0 }, false);
        const frozen = f.visible()[0].x; f.step(500);
        assert.equal(f.visible()[0].x, frozen); assert.equal(f.pending(), 0);
        f.renderer.updatePlayback({ kind: 'buffering' }, true); f.step(100);
        assert.ok(f.visible()[0].x < frozen);
        f.renderer.updatePlayback({ kind: 'paused', positionMs: 0 }, false);
        assert.equal(f.pending(), 0);
        f.renderer.setEnabled(false); assert.equal(f.visible().length, 0);
        f.renderer.setEnabled(true); assert.ok(f.visible().length > 0);
        f.renderer.clear(); assert.equal(f.visible().length, 0); assert.equal(f.pending(), 0);
        const disabled = fixture([comment('关闭时加载')], { enabled: false });
        disabled.step(1000); assert.equal(disabled.visible().length, 0); assert.equal(disabled.pending(), 0);
    },
};
let failed = 0;
for (const [name, run] of Object.entries(cases)) {
    try { run(); console.log(`PASS ${name}`); }
    catch (error) { failed += 1; console.error(`FAIL ${name}: ${error.message}`); }
}
if (failed) process.exitCode = 1;

async function browserChecks() {
    const { chromium } = require('playwright');
    const browser = await chromium.launch({ executablePath: process.env.VIDEO_UI_CHROME || 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true });
    try {
        for (const [width, height, pixelRatio] of [[740, 420, 1], [1920, 1080, 1], [3840, 2160, 2]]) {
            const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: pixelRatio });
            const result = await page.evaluate(async ({ rendererSource, defaults, width, height }) => {
                document.body.style.margin = '0';
                document.body.style.background = '#17202b';
                const layer = document.createElement('div');
                layer.style.cssText = `position:relative;width:${width}px;height:${height}px;overflow:hidden`;
                document.body.append(layer);
                await document.fonts.ready;
                let now = 0, id = 0, textCalls = 0, drawn = [];
                const callbacks = new Map();
                Object.defineProperty(performance, 'now', { configurable: true, value: () => now });
                window.requestAnimationFrame = callback => { callbacks.set(++id, callback); return id; };
                window.cancelAnimationFrame = key => callbacks.delete(key);
                const fill = CanvasRenderingContext2D.prototype.fillText;
                CanvasRenderingContext2D.prototype.fillText = function (...args) { textCalls += 1; return fill.apply(this, args); };
                const draw = CanvasRenderingContext2D.prototype.drawImage;
                CanvasRenderingContext2D.prototype.drawImage = function (image, x, y, width, height) {
                    if (this.canvas === layer.firstChild) drawn.push({ x, y, width, height });
                    return draw.call(this, image, x, y, width, height);
                };
                const create = new Function('danmakuStyleDefaults', `${rendererSource}; return createDanmakuRenderer;`)(defaults);
                const renderer = create(layer);
                const comments = Array.from({ length: 900 }, (_, i) => ({
                    time: i * 0.025, mode: [1, 4, 5, 6][i % 4], color: i % 2 ? 0xFFFFFF : 0x80FFD0,
                    text: `${i} ${['中文弹幕测试', 'A long scrolling comment', '😀🎬🍿', 'Wi fj punctuation!?'][i % 4]}`,
                }));
                const advance = milliseconds => {
                    now += milliseconds; drawn = [];
                    const queued = [...callbacks.values()]; callbacks.clear(); queued.forEach(callback => callback(now));
                };
                let checked = 0, peak = 0;
                const check = () => {
                    const visible = drawn.filter(entry => entry.x < width && entry.x + entry.width > 0);
                    peak = Math.max(peak, visible.length);
                    for (let i = 0; i < visible.length; i += 1) {
                        const a = visible[i];
                        if (a.y < 0 || a.y + a.height > height) throw new Error('glyphs escaped the viewport');
                        for (const b of visible.slice(i + 1)) {
                            checked += 1;
                            if (a.x + a.width > b.x && b.x + b.width > a.x && a.y + a.height > b.y && b.y + b.height > a.y) throw new Error('real glyph rectangles overlap');
                        }
                    }
                };
                renderer.load({ enabled: true, style: { ...defaults, displayArea: 100, fixedSeconds: 8 }, comments }, 0, true, 1);
                for (let frame = 0; frame < 180; frame += 1) { advance(1000 / 12); check(); }
                renderer.updatePlayback({ kind: 'seeked', positionMs: 18500 }, true); advance(0); check();
                advance(1200); check();
                renderer.setRate(2); advance(500); check();
                renderer.clear();
                // With one accepted comment, subsequent frames must perform no
                // additional text rasterization in the real Canvas context.
                renderer.load({ enabled: true, style: defaults, comments: [{ time: 0, text: '重复使用文字缓存', mode: 1 }] }, 0, true, 1);
                advance(1000); const before = textCalls;
                for (let i = 0; i < 60; i += 1) advance(1000 / 60);
                if (textCalls !== before) throw new Error('glyphs were rerasterized during animation');
                renderer.clear();
                return { checkedPairs: checked, peakVisible: peak, textCacheVerified: true };
            }, { rendererSource, defaults, width, height });
            assert.ok(result.checkedPairs > 100, 'dense comments exercised collision checks');
            console.log(`BROWSER ${width}x${height} DPR=${pixelRatio} ${JSON.stringify(result)}`);
            await page.close();
        }
        const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
        const cadence = await page.evaluate(async ({ rendererSource, defaults }) => {
            const layer = document.createElement('div'); layer.style.cssText = 'width:1920px;height:1080px'; document.body.append(layer);
            const create = new Function('danmakuStyleDefaults', `${rendererSource}; return createDanmakuRenderer;`)(defaults);
            const renderer = create(layer), intervals = [], durations = [];
            const raf = requestAnimationFrame.bind(window); let previous = 0;
            window.requestAnimationFrame = callback => raf(time => {
                if (previous) intervals.push(time - previous); previous = time;
                const start = performance.now(); callback(time); durations.push(performance.now() - start);
            });
            renderer.load({ enabled: true, style: defaults, comments: Array.from({ length: 1000 }, (_, i) => ({ time: i * 0.01, text: `密集弹幕移动测试 ${i}`, mode: 1, color: 0xFFFFFF })) }, 0, true, 1);
            await new Promise(resolve => setTimeout(resolve, 2200)); renderer.clear();
            const percentile = (values, fraction) => values.sort((a, b) => a - b)[Math.floor((values.length - 1) * fraction)];
            return { frames: durations.length, frameIntervalP50Ms: percentile(intervals, 0.5), frameIntervalP95Ms: percentile(intervals, 0.95), rendererP95Ms: percentile(durations.slice(1), 0.95) };
        }, { rendererSource, defaults });
        assert.ok(cadence.frames >= 60, 'the browser produced a sustained animation');
        console.log(`BROWSER_CADENCE ${JSON.stringify(cadence)}`);
    } finally { await browser.close(); }
}
if (process.argv.includes('--browser')) browserChecks().catch(error => { console.error(error); process.exitCode = 1; });
