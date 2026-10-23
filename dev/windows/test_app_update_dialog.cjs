// Verifies that applying a downloaded update uses a modal confirmation.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const root = path.resolve(__dirname, '../..');
const source = fs.readFileSync(path.join(root, 'src/web/mediastation.js'), 'utf8');
const html = fs.readFileSync(path.join(root, 'src/web/mediastation.html'), 'utf8');
const start = source.indexOf('    function updateStatusText(');
const end = source.indexOf('    function requestUpdateCheck(', start);
assert.ok(start >= 0 && end > start, 'update dialog handlers are present');

function node() {
    const classes = new Set();
    return {
        dataset: {},
        isConnected: true,
        textContent: '',
        disabled: false,
        open: false,
        focus() { this.focused = true; },
        showModal() { this.open = true; },
        close() { this.open = false; },
        classList: {
            add(name) { classes.add(name); },
            remove(name) { classes.delete(name); },
            toggle(name, enabled) { if (enabled) classes.add(name); else classes.delete(name); },
        },
    };
}

const elements = new Map([
    ['settings-app-version', node()],
    ['settings-app-update-status', node()],
    ['settings-check-app-update', node()],
    ['settings-download-app-update', node()],
    ['settings-install-app-update', node()],
    ['app-update-dialog', node()],
    ['app-update-dialog-confirm', node()],
]);
const calls = [];
const context = vm.createContext({
    byId: (id) => elements.get(id),
    formatBytes: (value) => `${value} B`,
    window: {
        jmpInfo: { version: '1.0.0' },
        jmpNative: { updateInstall() { calls.push('updateInstall'); } },
    },
});
vm.runInContext(`
let appUpdateState = { status: 'ready', payload: { version: '1.1.0', packageKind: 'portable' } };
let appUpdateDialogTrigger = null;
${source.slice(start, end)}
`, context);

const trigger = elements.get('settings-install-app-update');
context.showAppUpdateDialog(trigger);
assert.equal(elements.get('app-update-dialog').open, true);
assert.equal(elements.get('app-update-dialog-confirm').focused, true);
context.closeAppUpdateDialog();
assert.equal(elements.get('app-update-dialog').open, false);
assert.equal(trigger.focused, true);

context.showAppUpdateDialog(trigger);
context.confirmAppUpdate();
assert.deepEqual(calls, ['updateInstall']);
assert.equal(elements.get('app-update-dialog').open, false);
assert.equal(elements.get('settings-install-app-update').dataset.busy, 'true');

assert.match(html, /<dialog id="app-update-dialog"/);
assert.doesNotMatch(html, /id="settings-confirm-app-update"/);
assert.doesNotMatch(html, /id="confirm-app-update"/);
console.log('APP_UPDATE_DIALOG_TEST_OK/modal-focus/confirm-install/no-inline-confirm');
