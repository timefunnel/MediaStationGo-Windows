const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.js'), 'utf8');
const html = fs.readFileSync(path.join(__dirname, '../../src/web/mediastation.html'), 'utf8');
function handler(name) {
    const match = new RegExp(`^    (?:async )?function ${name}\\(`, 'm').exec(source);
    assert.ok(match, name);
    const rest = source.slice(match.index + match[0].length);
    const next = /\n    (?:async )?function /.exec(rest);
    assert.ok(next, `next handler after ${name}`);
    return source.slice(match.index, match.index + match[0].length + next.index);
}
function node(tag = 'div', className = '', textContent = '') {
    const classes = new Set(className.split(' ').filter(Boolean));
    const n = { tag, children: [], dataset: {}, disabled: false, textContent, events: {}, isConnected: true,
        append(...children) { for (const child of children) { if (child.parentNode) child.parentNode.children = child.parentNode.children.filter((c) => c !== child); child.parentNode = this; this.children.push(child); } },
        replaceChildren(...children) { this.children = []; this.append(...children); },
        insertBefore(child, reference) { if (child.parentNode) child.parentNode.children = child.parentNode.children.filter((c) => c !== child); child.parentNode = this; this.children.splice(this.children.indexOf(reference), 0, child); },
        setAttribute(name, value) { this[name] = value; },
        addEventListener(name, callback) { this.events[name] = callback; },
        focus() { this.focused = true; }, showModal() { this.open = true; }, close() { this.open = false; },
        querySelectorAll(selector) { return descendants(this).filter((child) => matches(child, selector)); },
        querySelector(selector) { return this.querySelectorAll(selector)[0] || null; },
        classList: { add(name) { classes.add(name); }, remove(name) { classes.delete(name); }, contains(name) { return classes.has(name); }, toggle(name, on) { on ? classes.add(name) : classes.delete(name); } },
    };
    Object.defineProperty(n, 'parentElement', { get() { return this.parentNode; } });
    Object.defineProperty(n, 'className', { set(value) { classes.clear(); value.split(' ').filter(Boolean).forEach((c) => classes.add(c)); } });
    return n;
}
function descendants(n) { return n.children.flatMap((child) => [child, ...descendants(child)]); }
function matches(n, selector) {
    if (selector.startsWith('.')) return n.classList.contains(selector.slice(1));
    const match = /^\[data-([\w-]+)(?:="([^"]+)")?\]$/.exec(selector);
    if (!match) return n.tag === selector;
    const key = match[1].replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    return match[2] === undefined ? key in n.dataset : n.dataset[key] === match[2];
}
function harness() {
    const elements = new Map(), cards = new Map(), timers = new Map(), requests = [];
    const byId = (id) => { assert.ok(html.includes(`id="${id}"`), id); if (!elements.has(id)) elements.set(id, node()); return elements.get(id); };
    for (const id of ['rife', 'rtx-video', 'dlssnr', 'nvidia-vfg']) {
        const card = node(); card.dataset.componentId = id;
        for (const action of ['primary', 'secondary']) { const button = node('button'); button.dataset.componentAction = action; card.append(button); }
        cards.set(id, card);
    }
    let timerId = 0;
    const context = vm.createContext({ byId, document: { createElement: node, querySelector(selector) { return cards.get(selector.match(/"([^"]+)"/)[1]); } },
        window: { clearTimeout(id) { timers.delete(id); }, setTimeout(fn, delay) { const id = ++timerId; timers.set(id, { fn, delay }); return id; } },
        nativeRequest: async (...args) => { requests.push(args); throw new Error('unexpected request'); },
        interpolationComponentRestarting: false, interpolationComponentTracking: false, interpolationComponentTimer: 0,
        interpolationModels: { 'rife-v4.26': { name: 'RIFE 质量优先', label: '质量优先' }, 'rife-v4.26-scale0.5': { name: 'RIFE 均衡优先', label: '均衡优先' }, 'rife-v4.25-lite': { name: 'RIFE 流畅优先', label: '流畅优先' } },
        preferredInterpolationModel: 'rife-v4.26', formatBytes: (v) => `${v} B`, friendlyError: (e) => e.message, refreshPlayerTools() {},
    });
    const start = source.indexOf('    const videoComponentStatuses = new Map();');
    vm.runInContext(source.slice(start, source.indexOf('    function openSettings(', start)), context);
    const rife = source.indexOf('    function updateFrameInterpolationStatus(');
    vm.runInContext(source.slice(rife, source.indexOf('    function scheduleImageCacheStatsRefresh(', rife)), context);
    return { context, byId, cards, timers, requests };
}
module.exports = { source, html, handler, node, harness, descendants };
