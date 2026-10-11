// Offline fixtures use actual markup, status handlers, dialogs and player settings renderer.
// Component states are explicit test inputs; no account or native package operations occur.
const fs = require('node:fs');
const path = require('node:path');
const { source, html, handler } = require('./video_components_test_dom.cjs');
const root = path.resolve(__dirname, '../..');
const output = path.resolve(process.argv[2] || path.join(root, 'build/video-parameters-ui'));
if (!output.startsWith(root + path.sep)) throw new Error('UI fixture must stay inside the workspace');
fs.mkdirSync(output, { recursive: true });
const section = (start, end) => { const a = source.indexOf(start), b = source.indexOf(end, a); if (a < 0 || b < 0) throw new Error('Renderer contract changed'); return source.slice(a, b); };
const css = fs.readFileSync(path.join(root, 'src/web/mediastation.css'), 'utf8');
const components = html.slice(html.indexOf('  <section id="components-page"'), html.indexOf('  <div id="overview-scrim"'));
const script = `${handler('element')}
${section('    const videoComponentStatuses = new Map();', '    function openSettings(')}
${section('    function updateFrameInterpolationStatus(', '    function scheduleImageCacheStatsRefresh(')}
${section('    const videoEnhancementDefaults = ', '    async function setFrameInterpolation(')}
const byId = (id) => document.getElementById(id);
let interpolationComponentRestarting=false, interpolationComponentTracking=false, interpolationComponentTimer=0;
const interpolationModels = {'rife-v4.26':{label:'质量优先'},'rife-v4.26-scale0.5':{label:'均衡优先'},'rife-v4.25-lite':{label:'流畅优先'}};
let preferredInterpolationModel='rife-v4.26';
const playerPanelContent=byId('player-panel-content');
let player={started:true,loadInfo:{},interpolationEnabled:false,videoEnhancement:{vsr:true,vfg:true,nr:true,nrStyle:1,nrIntensity:0.7}};
const calls=[];
function refreshPlayerTools(){} function friendlyError(error){return error.message;} function formatBytes(value){return value+' B';}
function setFrameInterpolation(model,selection){calls.push({model,selection});}
function renderPlayerPanel(){playerPanelContent.replaceChildren();renderVideoEnhancementSettings(player);}
function closePlayerPanel(){player.enhancementDraft=null;}
async function nativeRequest(){throw new Error('Native operations are unavailable in this layout fixture');}
updateFrameInterpolationStatus({component:{installed:true,supported:true},componentStatus:'ready',gpuName:'RTX 5070 Ti',models:[{},{},{}],runtimeVersion:'TensorRT RTX'});
updateVideoComponents({components:['rtx-video','dlssnr','nvidia-vfg'].map(id=>({id,installed:true,supported:true,runtime:{playbackAvailable:true}}))});
renderPlayerPanel();
for(const card of document.querySelectorAll('[data-component-id]'))for(const button of card.querySelectorAll('[data-component-action]'))button.addEventListener('click',()=>{if(!button.disabled)showComponentDialog(card.dataset.componentId,button.dataset.operation,button);});
byId('component-dialog-close').addEventListener('click',closeComponentDialog);byId('component-dialog-cancel').addEventListener('click',closeComponentDialog);
byId('component-dialog').addEventListener('cancel',event=>{event.preventDefault();closeComponentDialog();});
window.fixture={calls,player,showComponentDialog,closeComponentDialog,updateVideoComponents,renderPlayerPanel,videoComponentStatuses};
document.documentElement.dataset.rendered='true';`;
for (const theme of ['light','dark']) for (const kind of ['components','parameters']) {
    const markup = kind === 'components' ? components.replace('components-page hidden','components-page') : components;
    const panel = `<div class="player-panel ${kind === 'components' ? 'hidden' : ''}"><header><h2>画面增强</h2><button class="icon-button" aria-label="关闭">×</button></header><div id="player-panel-content" class="player-panel-content"></div></div><button id="player-interpolation" hidden></button>`;
    fs.writeFileSync(path.join(output, `${kind}-${theme}.html`), `<!doctype html><html lang="zh-CN" data-ui-theme="${theme}"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><style>${css}\nbody{overflow:auto;padding:32px 0}.player-panel{position:relative;right:auto;bottom:auto;width:min(380px,calc(100vw - 32px));max-height:700px;margin:auto}.player-panel-content{max-height:620px}.components-page{padding-top:0}</style>${markup}${panel}<script>${script}</script></html>`, 'utf8');
}
console.log(`VIDEO_SETTINGS_UI_FIXTURE_OK ${output}`);
