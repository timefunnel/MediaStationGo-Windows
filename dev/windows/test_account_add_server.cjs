// Exercise the production shell using an offline bridge; never save accounts or contact servers.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');

const web = path.resolve(__dirname, '../../src/web');
const preview = fs.readFileSync(path.join(__dirname, 'frontend-preview.ps1'), 'utf8');
const shim = preview.match(/\$PreviewShim = @'\r?\n([\s\S]*?)\r?\n'@/)[1];

function accountFixture() {
    const servers = [
        { serverId: 'a'.repeat(64), baseUrl: 'https://current.example.test', clientProfile: 'infuse',
            users: [{ accountId: '1'.repeat(64), userId: 'current-user', userName: '当前用户' }] },
        { serverId: 'b'.repeat(64), baseUrl: 'https://other.example.test', clientProfile: 'senplayer',
            users: [{ accountId: '2'.repeat(64), userId: 'other-user', userName: '其他用户' }] },
    ];
    const current = { ...servers[0], ...servers[0].users[0], configured: true };
    delete current.users;
    const respond = (id, operation, payload, ok = true) => window.setTimeout(
        () => window._onMediaStationResponse(id, operation, ok, JSON.stringify(payload)), 0,
    );
    window.accountFixture = { authentications: [], updates: [], proxyModes: [], motionFrames: [] };
    new MutationObserver(records => {
        for (const record of records) for (const layer of record.addedNodes) {
            if (!(layer instanceof Element) || !layer.matches('.settings-motion-layer')) continue;
            const phase = document.getElementById('settings-drawer').classList.contains('is-closing') ? 'closing' : 'opening';
            const sample = () => {
                if (!layer.isConnected) return;
                window.accountFixture.motionFrames.push({
                    phase,
                    nav: getComputedStyle(layer.querySelector('.settings-nav')).display,
                    layout: getComputedStyle(layer.querySelector('.settings-layout')).display,
                    padding: getComputedStyle(layer.querySelector('.settings-content')).padding,
                    selected: layer.querySelector('input:checked')?.value,
                });
                requestAnimationFrame(sample);
            };
            sample();
        }
    }).observe(document.body, { childList: true });
    window.jmpNative.mediaStationSessionStatus = id => respond(id, 'session_status', {
        ...current, configured: !new URLSearchParams(location.search).has('gateway'),
    });
    window.jmpNative.mediaStationListAccounts = id => respond(id, 'list_accounts', { servers });
    window.jmpNative.mediaStationAuthenticate = (id, baseUrl, username, password, clientProfile) => {
        window.accountFixture.authentications.push({ baseUrl, username, password, clientProfile });
        respond(id, 'authenticate', { code: window.accountFixture.authenticationFailureCode || 'fixture_validation_only', message: '离线测试：账号未保存' }, false);
    };
    window.jmpNative.mediaStationUpdateAccount = (id, ...args) => {
        window.accountFixture.updates.push(args);
        respond(id, 'update_account', { code: 'fixture_validation_only', message: '离线测试：账号未保存' }, false);
    };
    window.jmpNative.mediaStationSetProxyMode = (id, mode) => {
        window.accountFixture.proxyModes.push(mode);
        respond(id, 'set_proxy_mode', { ...current, proxyMode: mode });
    };
}

async function openPage(browser, width, theme, reducedMotion) {
    const page = await browser.newPage({ viewport: { width, height: 900 } });
    page.errors = [];
    page.on('pageerror', error => page.errors.push(error.message));
    await page.emulateMedia({ reducedMotion });
    await page.route('**/*', async route => {
        const url = new URL(route.request().url());
        if (url.hostname !== 'account-ui.test') return route.abort();
        if (url.pathname === '/fixture.js') return route.fulfill({ contentType: 'application/javascript', body: shim + '\n(' + accountFixture.toString() + ')();' });
        if (url.pathname === '/') return route.fulfill({ contentType: 'text/html; charset=utf-8', body: fs.readFileSync(path.join(web, 'mediastation.html'), 'utf8').replace('<script src="mediastation.js"></script>', '<script src="/fixture.js"></script><script src="mediastation.js"></script>') });
        const asset = path.join(web, path.basename(url.pathname));
        if (!fs.existsSync(asset)) return route.fulfill({ status: 404, body: '' });
        return route.fulfill({ contentType: ({ '.js': 'application/javascript', '.css': 'text/css', '.png': 'image/png' })[path.extname(asset)], body: fs.readFileSync(asset) });
    });
    await page.goto('http://account-ui.test/');
    await page.locator('#app-shell:not(.hidden)').waitFor();
    await page.evaluate(theme => { document.documentElement.dataset.uiTheme = theme; }, theme);
    return page;
}

async function openAccounts(page) {
    if (!await page.locator('#account-open').isVisible()) await page.locator('#sidebar-toggle').click();
    await page.locator('#account-open').click();
    await page.locator('#saved-accounts-list .saved-account-server').nth(1).waitFor();
}

async function assertNewServer(page) {
    await page.locator('#login-form:not(.hidden)').waitFor();
    assert.equal(await page.locator('#server-url').inputValue(), '', 'adding a server must not inherit the current server address');
    assert.ok(await page.locator('#server-url').isEditable(), 'new server address must be editable');
    assert.equal(await page.locator('input[name="emby-client-profile"]:checked').inputValue(), 'mediastation_windows', 'new servers must use the default identity');
    assert.equal(await page.locator('input[name="emby-client-profile"]:disabled').count(), 0);
    assert.equal(await page.locator('#username').inputValue(), '');
    assert.equal(await page.locator('#password').inputValue(), '');
    assert.equal(await page.locator('#login-title').textContent(), '添加服务器');
}

async function assertConnectionSettings(page, changeProxy = false) {
    const formState = () => ({
        values: ['server-url', 'username', 'password'].map(id => document.getElementById(id).value),
        readOnly: document.getElementById('server-url').readOnly,
        profiles: [...document.querySelectorAll('input[name="emby-client-profile"]')].map(input => [input.value, input.checked, input.disabled]),
        error: document.getElementById('login-error').textContent,
    });
    const before = await page.evaluate(formState);
    await page.evaluate(() => { window.accountFixture.motionFrames = []; });
    await page.locator('#login-settings-open').click();
    const settings = page.locator('#settings-drawer');
    await settings.waitFor({ state: 'visible', timeout: 2000 });
    await page.waitForFunction(() => !document.getElementById('settings-drawer').classList.contains('is-animating'));
    assert.equal(await settings.getAttribute('role'), 'dialog');
    assert.equal(await settings.getAttribute('aria-modal'), 'true');
    assert.equal(await settings.evaluate(panel => panel.classList.contains('settings-page')), false);
    assert.ok(await settings.locator('[data-settings-section="network"].settings-section').isVisible());
    assert.equal(await settings.locator('.settings-nav').isVisible(), false);
    if (changeProxy) {
        await settings.locator('label').filter({ has: page.locator('input[name="settings-proxy-mode"][value="system"]') }).click();
        await page.waitForFunction(() => document.getElementById('settings-proxy-status').dataset.state === 'ready');
        assert.deepEqual(await page.evaluate(() => window.accountFixture.proxyModes), ['system']);
    }
    const selected = await settings.locator('input[name="settings-proxy-mode"]:checked').inputValue();
    await settings.locator('.settings-preferences .drawer-close').click();
    await settings.waitFor({ state: 'hidden' });
    const frames = await page.evaluate(() => window.accountFixture.motionFrames);
    const reducedMotion = await page.evaluate(() => matchMedia('(prefers-reduced-motion: reduce)').matches);
    for (const phase of ['opening', 'closing']) {
        const phaseFrames = frames.filter(frame => frame.phase === phase);
        assert.equal(phaseFrames.length > 0, !reducedMotion, `${phase} animation must be exercised when motion is enabled`);
        for (const frame of phaseFrames) {
            assert.equal(frame.nav, 'none', `${phase} animation must never show the full settings navigation`);
            assert.equal(frame.layout, 'block', `${phase} animation must retain the connection-only layout`);
            assert.equal(frame.padding, '30px', `${phase} animation must retain the settled content spacing`);
            if (phase === 'closing') assert.equal(frame.selected, selected, 'the animation retains the selected proxy mode');
        }
    }
    assert.equal(await page.locator('.settings-motion-layer').count(), 0, 'completed animations leave no duplicate settings card');
    assert.deepEqual(await page.evaluate(formState), before, 'opening settings must preserve the login form and its lock state');
    assert.ok(await page.locator('#login-settings-open').evaluate(button => document.activeElement === button));
}

async function main() {
    const browser = await chromium.launch({ executablePath: process.env.VIDEO_UI_CHROME || 'C:/Program Files/Google/Chrome/Application/chrome.exe', headless: true });
    try {
        for (const motion of ['no-preference', 'reduce']) for (const theme of ['light', 'dark']) for (const width of [1280, 390]) {
            const page = await openPage(browser, width, theme, motion);
            if (!await page.locator('#settings-open').isVisible()) await page.locator('#sidebar-toggle').click();
            await page.locator('#settings-open').click();
            await page.locator('#settings-drawer.settings-page').waitFor();
            await page.locator('.settings-nav [data-settings-section="appearance"]').click();
            await openAccounts(page);
            const current = page.locator('#saved-accounts-list .saved-account-server').filter({ hasText: 'current.example.test' });
            await current.locator('.saved-account-server-add').click();
            assert.equal(await page.locator('#server-url').inputValue(), 'https://current.example.test');
            assert.equal(await page.locator('#server-url').isEditable(), false);
            assert.equal(await page.locator('input[name="emby-client-profile"]:checked').inputValue(), 'infuse');
            assert.equal(await page.locator('input[name="emby-client-profile"]:disabled').count(), 3);
            await page.locator('#login-cancel').click();

            await openAccounts(page);
            await page.locator('#change-account-button').click();
            await assertNewServer(page);
            assert.equal(await page.locator('#change-account-button').textContent(), '添加服务器');
            await page.locator('#server-url').fill('https://new.example.test:8920/emby');
            await page.locator('#username').fill('new-user');
            await page.locator('#password').fill('fixture-password');
            await page.locator('.login-advanced summary').click();
            await page.locator('input[name="emby-client-profile"][value="senplayer"]').locator('..').click();
            if (process.argv[2]) {
                fs.mkdirSync(process.argv[2], { recursive: true });
                await page.screenshot({ path: path.join(process.argv[2], `add-server-${theme}-${width}-${motion}.png`) });
            }
            await page.locator('#login-submit').click();
            await page.waitForFunction(() => document.getElementById('login-error').textContent.includes('离线测试'));
            assert.deepEqual(await page.evaluate(() => window.accountFixture.authentications), [{ baseUrl: 'https://new.example.test:8920/emby', username: 'new-user', password: 'fixture-password', clientProfile: 'senplayer' }]);
            assert.ok(await page.locator('#server-url').isEditable(), 'authentication failure must not lock the new server');
            assert.equal(await page.locator('input[name="emby-client-profile"]:disabled').count(), 0);
            await assertConnectionSettings(page, true);
            await page.evaluate(() => { window.accountFixture.authenticationFailureCode = 'authentication_forbidden'; });
            await page.locator('#login-submit').click();
            await page.waitForFunction(() => document.getElementById('login-error').textContent.includes('HTTP 403'));
            assert.doesNotMatch(await page.locator('#login-error').textContent(), /密码不正确/);
            await page.evaluate(() => { window.accountFixture.authenticationFailureCode = 'invalid_credentials'; });
            await page.locator('#login-submit').click();
            await page.waitForFunction(() => document.getElementById('login-error').textContent.includes('HTTP 401'));
            await page.evaluate(() => { window.accountFixture.authenticationFailureCode = 'authentication_transport_failed'; });
            await page.locator('#login-submit').click();
            await page.waitForFunction(() => document.getElementById('login-error').textContent.includes('尚未完成账号验证'));
            await page.locator('#login-cancel').click();
            assert.ok(await page.locator('#settings-drawer.settings-page').isVisible(), 'cancel restores the existing settings page after the login modal borrowed it');
            assert.ok(await page.locator('#settings-drawer .settings-nav').isVisible());

            await openAccounts(page);
            const other = page.locator('#saved-accounts-list .saved-account-server').filter({ hasText: 'other.example.test' });
            await other.locator('.saved-account-server-add').click();
            assert.equal(await page.locator('#server-url').inputValue(), 'https://other.example.test');
            assert.equal(await page.locator('#server-url').isEditable(), false);
            assert.equal(await page.locator('input[name="emby-client-profile"]:checked').inputValue(), 'senplayer');
            await assertConnectionSettings(page);
            await page.locator('#login-cancel').click();
            await openAccounts(page);
            await other.locator('button[title="修改账号"]').click();
            assert.equal(await page.locator('#server-url').inputValue(), 'https://other.example.test');
            assert.equal(await page.locator('#server-url').isEditable(), false);
            assert.equal(await page.locator('#username').inputValue(), '其他用户');
            assert.equal(await page.locator('input[name="emby-client-profile"]:checked').inputValue(), 'senplayer');
            assert.equal(await page.locator('input[name="emby-client-profile"]:disabled').count(), 3);
            await assertConnectionSettings(page);
            await page.locator('#login-cancel').click();
            await openAccounts(page);
            await page.locator('#change-account-button').click();
            await assertNewServer(page);
            await assertConnectionSettings(page);
            await page.locator('#login-cancel').click();

            await page.goto('http://account-ui.test/?gateway');
            await page.locator('#login-account-panel:not(.hidden)').waitFor();
            await page.locator('#login-add-account').click();
            await assertNewServer(page);
            await assertConnectionSettings(page);
            await page.locator('#login-cancel').click();
            await page.locator('#login-account-panel:not(.hidden)').waitFor();
            assert.deepEqual(page.errors, []);
            await page.close();
        }
        console.log('ACCOUNT_ADD_SERVER_OK: new address and identity; per-server users; edit existing account; lock reset; submitted parameters; 401/403 errors; connection settings preserve form and app page; proxy selection; saved-account gateway; opening/closing animation frames; normal/reduced motion; light/dark desktop/narrow');
    } finally { await browser.close(); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });
