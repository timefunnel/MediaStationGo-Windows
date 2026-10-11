// Offline layout fixture built from the actual player panel renderer and CSS.
// It does not represent a running player or installed user components.
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '../..');
const output = path.resolve(process.argv[2] || path.join(root, 'build/video-parameters-ui'));
if (!output.startsWith(root + path.sep)) throw new Error('UI fixture must stay inside the workspace');
fs.mkdirSync(output, { recursive: true });
const source = fs.readFileSync(path.join(root, 'src/web/mediastation.js'), 'utf8');
const section = (start, end) => {
    const from = source.indexOf(start), to = source.indexOf(end, from);
    if (from < 0 || to < 0) throw new Error('Actual panel renderer contract changed');
    return source.slice(from, to);
};
const css = fs.readFileSync(path.join(root, 'src/web/mediastation.css'), 'utf8');
const renderer = section("        if (kind === 'interpolation') {", "        byId('player-panel-title').textContent = '播放信息';");
// element is adjacent to the next helper; search past its declaration.
const from = source.indexOf('    function element(tag,');
const elementSource = source.slice(from, source.indexOf('    function ', from + 20));
const trackSource = section('    function createTrackOption(', '    function createPlayerEpisodeOption(');
const defaultsSource = section('    const videoEnhancementDefaults = ', '    async function setFrameInterpolation(');
for (const theme of ['light', 'dark']) {
    const script = `${elementSource}\n${trackSource}\n${defaultsSource}\n
const byId = (id) => document.getElementById(id);
const playerPanelContent = byId('player-panel-content');
const player = { interpolationEnabled: false, interpolationChanging: false, videoEnhancement: { nr: true, nrStyle: 1, nrIntensity: 0.7 } };
const info = {};
const interpolationModels = { 'rife-v4.26': { label: 'RIFE v4.26', name: '原尺寸 · 固定 2 倍插帧' } };
const videoComponentStatuses = new Map(['rtx-video','nvidia-vfg','dlssnr'].map((id) => [id, { runtime: { playbackAvailable: true } }]));
function setFrameInterpolation() {}
function render(kind) { ${renderer} }
render('interpolation');
const forms = document.querySelectorAll('details'); forms[2].open = true;
playerPanelContent.scrollTop = forms[2].offsetTop - 100;
document.documentElement.dataset.rendered = 'true';`;
    fs.writeFileSync(path.join(output, `${theme}.html`), `<!doctype html><html lang="zh-CN" data-ui-theme="${theme}"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><style>${css}
body{padding:24px;overflow:auto} .player-panel{position:relative;right:auto;bottom:auto;width:360px;max-height:760px;margin:auto} .player-panel-content{max-height:690px}
</style><div class="player-panel"><header><h2 id="player-panel-title"></h2></header><div id="player-panel-content" class="player-panel-content"></div></div><button id="player-interpolation" data-component-ready="true" hidden></button><script>${script}</script></html>`, 'utf8');
}
console.log(`VIDEO_PARAMETERS_UI_FIXTURE_OK ${output}`);
