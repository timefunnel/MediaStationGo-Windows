const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
function deferred() {
    let resolve, reject;
    const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
    return { promise, resolve, reject };
}
async function settle() { for (let i = 0; i < 8; i += 1) await Promise.resolve(); }
function fixture({ theme = 'light', reducedMotion = false, supported = true } = {}) {
    const colors = { light: '#f4f5f7', dark: '#14171c' };
    const nativeColors = [], errors = [], toasts = [], frames = new Map(), properties = new Map();
    const button = { attrs: {}, setAttribute(key, value) { this.attrs[key] = value; },
        removeAttribute(key) { delete this.attrs[key]; },
        getBoundingClientRect: () => ({ left: 1079, top: 11, width: 42, height: 42 }) };
    const ready = deferred(), finished = deferred(), animationFinished = deferred();
    let frameId = 0, progress = 0, animations = 0, skips = 0, canceled = false;
    const animation = { effect: { getComputedTiming: () => ({ progress }) }, finished: animationFinished.promise,
        cancel() { canceled = true; } };
    const root = { dataset: { uiTheme: theme }, style: {
        setProperty: (key, value) => properties.set(key, value), removeProperty: (key) => properties.delete(key),
    }, animate() { animations += 1; return animation; } };
    const transition = { ready: ready.promise, finished: finished.promise, skipTransition() { skips += 1; } };
    const context = vm.createContext({
        uiThemeTransition: null, uiThemeSettingKey: 'theme', byId: () => button,
        validUiTheme: (value) => value === 'light' || value === 'dark', prefersReducedMotion: () => reducedMotion,
        getComputedStyle: () => ({ getPropertyValue: () => colors[root.dataset.uiTheme] }),
        document: { documentElement: root, querySelectorAll: () => [],
            ...(supported ? { startViewTransition(update) { update(); return transition; } } : {}) },
        window: { innerWidth: 1200, innerHeight: 800, localStorage: { setItem() {} },
            jmpNative: { themeColor: (color) => nativeColors.push(color) },
            requestAnimationFrame(fn) { frames.set(++frameId, fn); return frameId; },
            cancelAnimationFrame(id) { frames.delete(id); } },
        console: { error: (error) => errors.push(error) }, showToast: (message) => toasts.push(message),
    });
    for (const name of ['syncUiThemeTitlebar', 'animateUiThemeTitlebar', 'applyUiTheme', 'toggleUiTheme']) {
        const start = Math.max(source.indexOf(`    function ${name}(`), source.indexOf(`    async function ${name}(`));
        assert.ok(start >= 0, `missing ${name}`);
        const next = source.slice(start + 1).search(/^    (?:async )?function /m);
        vm.runInContext(source.slice(start, start + 1 + next), context);
    }
    return { context, root, button, nativeColors, frames, properties, errors, toasts, ready, finished, animationFinished,
        get animations() { return animations; }, get skips() { return skips; }, get canceled() { return canceled; },
        frame(value) {
            progress = value;
            const callbacks = [...frames.values()]; frames.clear();
            for (const callback of callbacks) callback();
        } };
}

async function checkAnimatedTheme(theme) {
    const f = fixture({ theme });
    const target = theme === 'light' ? 'dark' : 'light';
    const targetColor = target === 'dark' ? '#14171c' : '#f4f5f7';
    const pending = f.context.toggleUiTheme();
    assert.equal(f.root.dataset.uiTheme, target);
    assert.equal(f.nativeColors.length, 0, 'the snapshot update must not switch the native titlebar immediately');
    assert.equal(f.button.attrs['aria-disabled'], 'true');
    await f.context.toggleUiTheme();
    assert.equal(f.root.dataset.uiTheme, target, 'repeated clicks during the reveal must be ignored');
    f.ready.resolve(); await settle();
    assert.equal(f.animations, 1);
    f.frame(0); f.frame(0.01);
    assert.equal(f.nativeColors.length, 0, 'keep the old titlebar until the circle reaches the top edge');
    f.frame(0.5);
    assert.equal(f.nativeColors.length, 1);
    assert.notEqual(f.nativeColors[0], targetColor, 'the titlebar must have an intermediate color during the reveal');
    const halfway = parseInt(f.nativeColors[0].slice(1, 3), 16);
    f.frame(0.75);
    const later = parseInt(f.nativeColors.at(-1).slice(1, 3), 16);
    assert.ok(target === 'dark' ? later < halfway : later > halfway);
    f.frame(1);
    assert.equal(f.nativeColors.at(-1), targetColor);
    f.finished.resolve(); await settle();
    assert.equal(f.context.uiThemeTransition !== null, true, 'cleanup must also wait for the reveal animation');
    f.animationFinished.resolve(); await pending;
    assert.equal(f.frames.size, 0);
    assert.equal(f.canceled, true, 'finished snapshot animations must not affect the next toggle');
    assert.equal(f.context.uiThemeTransition, null);
    assert.equal(f.button.attrs['aria-disabled'], undefined);
    assert.equal(f.root.dataset.uiThemeTransition, undefined);
    assert.equal(f.properties.size, 0);
    assert.equal(f.errors.length, 0);
    assert.equal(f.toasts.length, 0);
}

(async () => {
    await checkAnimatedTheme('light');
    await checkAnimatedTheme('dark');
    for (const options of [{ reducedMotion: true }, { supported: false }]) {
        const f = fixture(options);
        await f.context.toggleUiTheme();
        assert.deepEqual(f.nativeColors, ['#14171c']);
        assert.equal(f.frames.size, 0);
    }
    const settings = fixture();
    settings.context.applyUiTheme('dark', true);
    assert.deepEqual(settings.nativeColors, ['#14171c'], 'settings without an animation must still update immediately');
    const skipped = fixture();
    const pending = skipped.context.toggleUiTheme();
    skipped.ready.reject(new Error('snapshot unavailable')); await pending;
    assert.deepEqual(skipped.nativeColors, ['#14171c']);
    assert.equal(skipped.skips, 1);
    assert.equal(skipped.errors.length, 1);
    assert.equal(skipped.toasts.length, 1);
    assert.equal(skipped.frames.size, 0);
    assert.equal(skipped.context.uiThemeTransition, null);
    const interrupted = fixture();
    const interruptedPending = interrupted.context.toggleUiTheme();
    interrupted.ready.resolve(); await settle(); interrupted.frame(0.4);
    interrupted.animationFinished.reject(new Error('animation canceled')); await interruptedPending;
    assert.equal(interrupted.nativeColors.at(-1), '#14171c');
    assert.equal(interrupted.frames.size, 0, 'canceling the reveal must stop native color updates');
    assert.equal(interrupted.context.uiThemeTransition, null);
    console.log('THEME_TRANSITION_TEST_OK: native titlebar follows the reveal in both directions; immediate modes and interrupted animations stay synchronized');
})().catch((error) => { console.error(error); process.exitCode = 1; });
