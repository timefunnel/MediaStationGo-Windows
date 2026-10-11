// Read-only checks of the updated standard app; dialogs are cancelled before native mutations.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const port = Number(process.argv[2]);
assert.ok(Number.isInteger(port) && port > 0 && port < 65536);
async function main() {
    const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    const target = pages.find((page) => page.url === 'app://resources/mediastation.html');
    assert.ok(target);
    const ws = new WebSocket(target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => { const timer = setTimeout(() => reject(new Error('CDP timeout')), 6000); ws.onopen = () => { clearTimeout(timer); resolve(); }; ws.onerror = () => { clearTimeout(timer); reject(new Error('CDP failed')); }; });
    const pending = new Map(); let nextId = 0;
    ws.onmessage = ({ data }) => { const message = JSON.parse(data), task = pending.get(message.id); if (!task) return; pending.delete(message.id); clearTimeout(task.timer); if (message.error || message.result?.exceptionDetails) task.reject(new Error('Embedded UI check failed')); else task.resolve(message.result); };
    const send = (method, params) => new Promise((resolve, reject) => { const id = ++nextId, timer = setTimeout(() => { pending.delete(id); reject(new Error('Evaluation timeout')); }, 10000); pending.set(id, { resolve, reject, timer }); ws.send(JSON.stringify({ id, method, params })); });
    const evaluate = async (expression) => (await send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })).result.value;
    try {
        await evaluate(`document.getElementById('components-open').click()`);
        const state = await evaluate(`({visible:!document.getElementById('components-page').classList.contains('hidden'),actions:document.querySelectorAll('.component-card button').length,viewport:{width:innerWidth,height:innerHeight},gridBottom:document.querySelector('.component-grid').getBoundingClientRect().bottom,components:[...document.querySelectorAll('[data-component-id]')].map(card=>{const button=card.querySelector('[data-component-action="primary"]');return {id:card.dataset.componentId,text:button.textContent,disabled:button.disabled,cursor:getComputedStyle(button).cursor};})})`);
        assert.equal(state.visible, true); assert.equal(state.actions, 8);
        for (const id of ['rife', 'dlssnr', 'nvidia-vfg']) { const c = state.components.find((c) => c.id === id); assert.equal(c.text, '已安装'); assert.equal(c.disabled, true); assert.equal(c.cursor, 'default'); }
        const before = state.gridBottom;
        await evaluate(`document.querySelector('[data-component-id="rtx-video"] [data-component-action="primary"]').click()`);
        const installDialog = await evaluate(`({open:document.getElementById('component-dialog').open,title:document.getElementById('component-dialog-title').textContent,localVisible:!document.getElementById('component-dialog-local').hidden,gridBottom:document.querySelector('.component-grid').getBoundingClientRect().bottom})`);
        assert.equal(installDialog.open, true); assert.equal(installDialog.localVisible, true); assert.equal(installDialog.gridBottom, before); assert.match(installDialog.title, /安装RTX Video/);
        await evaluate(`document.getElementById('component-dialog-cancel').click();document.querySelector('[data-component-id="dlssnr"] [data-component-action="secondary"]').click()`);
        assert.match(await evaluate(`document.getElementById('component-dialog-title').textContent`), /卸载DLSSNR/);
        await evaluate(`document.getElementById('component-dialog-cancel').click()`);
        const root = path.resolve(__dirname, '../..');
        const screenshot = await send('Page.captureScreenshot', { format: 'png' });
        fs.writeFileSync(path.join(root, 'build/video-enhancement/component-settings-app.png'), Buffer.from(screenshot.data, 'base64'));
        const report = { standardApp: true, existingUserComponents: true, state, installDialog, nativeMutations: 0, mediaStarted: false };
        fs.writeFileSync(path.join(root, 'build/video-enhancement/component-settings-app.json'), JSON.stringify(report, null, 2));
        console.log('APP_COMPONENT_SETTINGS_OK installed-cursor/fixed-actions/install-source-modal/uninstall-modal/no-layout-expansion/no-native-mutations');
    } finally { ws.close(); }
}
main().catch((error) => { console.error(error); process.exitCode = 1; });
