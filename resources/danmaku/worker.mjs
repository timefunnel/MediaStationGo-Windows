import { run, safeError } from './sources.mjs';
import { DiskCache, loadDanmaku, searchCandidates } from './local.mjs';

// stdin/stdout only: no HTTP server, MSG credentials or media files enter this process.
console.log = console.info = console.debug = console.warn = console.error = () => {};
async function main() {
  let text = '';
  process.stdin.setEncoding('utf8');
  for await (const chunk of process.stdin) {
    text += chunk;
    if (text.length > 65536) throw new Error('弹幕请求过大');
  }
  const input = JSON.parse(text);
  const cache = new DiskCache(process.argv[2]);
  const result = input.action === 'candidates' ? await searchCandidates(input, cache, run) : await loadDanmaku(input, cache, run);
  process.stdout.write(JSON.stringify(result));
}
main().catch(error => {
  process.stdout.write(JSON.stringify({ status: 'error', message: safeError(error.message) }));
  process.exitCode = 1;
});
