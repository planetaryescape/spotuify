#!/usr/bin/env node

import { existsSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const siteRoot = resolve(__dirname, '..');
const docsRoot = join(siteRoot, 'src', 'content', 'docs');
const cliRoot = join(docsRoot, 'reference', 'cli');

const requiredCliPages = [
  'index',
  'concepts',
  'onboard',
  'login',
  'logout',
  'doctor',
  'daemon',
  'daemon-start',
  'daemon-stop',
  'daemon-restart',
  'daemon-status',
  'daemon-install-service',
  'daemon-uninstall-service',
  'auth',
  'auth-bearer',
  'mcp',
  'status',
  'devices',
  'search',
  'search-page',
  'resolve-tracks',
  'queue',
  'queue-add',
  'playlists',
  'play',
  'play-uri',
  'next',
  'previous',
  'pause',
  'resume',
  'toggle',
  'seek',
  'volume',
  'shuffle',
  'repeat',
  'transfer',
  'audio-outputs',
  'audio-output',
  'playlist',
  'playlist-plan',
  'playlist-create',
  'playlist-tracks',
  'playlist-play',
  'playlist-add',
  'playlist-add-current',
  'playlist-unfollow',
  'playlist-set-image',
  'library',
  'library-tracks',
  'lyrics',
  'lyrics-show',
  'lyrics-follow',
  'lyrics-fetch',
  'lyrics-export',
  'lyrics-offset',
  'refresh-media',
  'viz',
  'viz-enable',
  'viz-disable',
  'viz-source',
  'viz-status',
  'hooks',
  'hooks-test',
  'mpris',
  'mpris-status',
  'like',
  'save',
  'logs',
  'logs-path',
  'logs-tail',
  'config',
  'config-path',
  'config-init',
  'config-get',
  'config-set',
  'analytics',
  'analytics-events',
  'analytics-top',
  'analytics-habits',
  'analytics-search',
  'analytics-rediscovery',
  'analytics-rebuild',
  'analytics-prune',
  'analytics-export',
  'analytics-import',
  'ops',
  'ops-log',
  'ops-show',
  'ops-undo',
  'ops-redo',
  'generate',
  'generate-completions',
  'generate-man-page',
  'reload',
  'reconnect',
  'bug-report',
  'reindex',
  'cache',
  'cache-status',
  'cache-reset',
  'cache-repair',
  'sync',
];

function* walk(dir) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) yield* walk(full);
    else if (/\.(md|mdx)$/.test(entry)) yield full;
  }
}

let failed = false;

for (const page of requiredCliPages) {
  const file = join(cliRoot, `${page}.md`);
  if (!existsSync(file)) {
    console.error(`[docs] missing CLI page: reference/cli/${page}.md`);
    failed = true;
    continue;
  }
  const text = readFileSync(file, 'utf8');
  if (page !== 'concepts' && !text.includes('Usage: spotuify')) {
    console.error(`[docs] CLI page lacks usage block: reference/cli/${page}.md`);
    failed = true;
  }
}

for (const file of walk(docsRoot)) {
  const text = readFileSync(file, 'utf8');
  if (/\b(showcase|pivotal|delve|tapestry|game[- ]changer)\b/i.test(text)) {
    console.error(`[docs] humanizer banned word in ${file}`);
    failed = true;
  }
  if (text.includes('—')) {
    console.error(`[docs] em dash in ${file}`);
    failed = true;
  }
  if (/```bash[\s\S]*?\bspotuify [^\n]*--format ids[\s\S]*?\|\s*xargs\s+-r/.test(text)) {
    console.error(`[docs] non-portable xargs -r in ${file}`);
    failed = true;
  }
}

if (failed) process.exit(1);
console.log(`[docs] ok (${requiredCliPages.length} CLI pages checked)`);
