import { build } from 'esbuild';
import { copyFile, cp, mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const root = fileURLToPath(new URL('.', import.meta.url));
const out = join(root, 'dist');
if (process.version !== 'v25.6.0') throw new Error('本地弹幕随包运行时固定为 Node.js 25.6.0，请使用该版本构建');
await mkdir(out, { recursive: true });
await build({ entryPoints: [join(root, 'worker.mjs')], outfile: join(out, 'worker.cjs'),
  bundle: true, platform: 'node', format: 'cjs', target: 'node22', logLevel: 'warning' });
await copyFile(process.execPath, join(out, process.platform === 'win32' ? 'node.exe' : 'node'));
await copyFile(join(root, 'node_modules/danmu-api-server/LICENSE'), join(out, 'SOURCE-LICENSE.txt'));
await copyFile(join(dirname(process.execPath), 'LICENSE'), join(out, 'NODE-LICENSE.txt'));
// Ship editable sources and locked dependency versions alongside the IPC worker.
const source = join(out, 'source');
await mkdir(source, { recursive: true });
for (const file of await readdir(root)) {
  if (file.endsWith('.mjs') || file.startsWith('package') || file === 'README.md')
    await copyFile(join(root, file), join(source, file));
}
await cp(join(root, 'node_modules/danmu-api-server'), join(source, 'upstream'), { recursive: true });
const notices = [];
async function collectNotices(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) await collectNotices(path);
    else if (/^(licen[sc]e|copying|notice)(\.|$)/i.test(entry.name))
      notices.push(`\n--- ${path.slice(root.length)} ---\n${await readFile(path, 'utf8')}`);
  }
}
await collectNotices(join(root, 'node_modules'));
await writeFile(join(out, 'DEPENDENCY-LICENSES.txt'), notices.join('\n'), 'utf8');
