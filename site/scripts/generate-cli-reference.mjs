#!/usr/bin/env node

import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '..', '..');
const SNAPSHOT_DIR = join(REPO_ROOT, 'tests', 'snapshots');
const OUT_DIR = join(REPO_ROOT, 'site', 'src', 'content', 'docs', 'reference', 'cli');
const PREFIX = 'cli_help__cli_help_';
const GENERATED = '<!-- generated: spotuify-cli-reference -->';

const GLOBAL_OPTIONS = [
  '      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]',
  '      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead',
  '  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable',
  '  -h, --help                     Print help',
];

const EXPECTED_PAGES = [
  'index',
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
  'status',
  'devices',
  'search',
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
  'playlist',
  'playlist-plan',
  'playlist-create',
  'playlist-tracks',
  'playlist-play',
  'playlist-add',
  'playlist-add-current',
  'library',
  'library-tracks',
  'lyrics',
  'lyrics-show',
  'lyrics-fetch',
  'lyrics-offset',
  'viz',
  'viz-enable',
  'viz-disable',
  'viz-source',
  'viz-status',
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

const COMMAND_EXAMPLES = {
  index: [
    'spotuify',
    'spotuify play "imagine dragons" --format json',
    'spotuify search "luther vandross" --type track --format ids',
  ],
  onboard: ['spotuify onboard'],
  login: ['spotuify login', 'spotuify login --redirect-uri http://127.0.0.1:8888/callback'],
  logout: ['spotuify logout'],
  doctor: ['spotuify doctor', 'spotuify doctor --format json'],
  daemon: ['spotuify daemon status', 'spotuify daemon start --foreground'],
  'daemon-start': ['spotuify daemon start', 'spotuify daemon start --foreground'],
  'daemon-stop': ['spotuify daemon stop'],
  'daemon-restart': ['spotuify daemon restart'],
  'daemon-status': ['spotuify daemon status --format json'],
  'daemon-install-service': ['spotuify daemon install-service'],
  'daemon-uninstall-service': ['spotuify daemon uninstall-service'],
  status: ['spotuify status', 'spotuify status --format json | jq .playback'],
  devices: ['spotuify devices', 'spotuify devices --format ids'],
  search: [
    'spotuify search "luther vandross" --type track',
    'spotuify search "quiet storm" --source local --format jsonl',
    'spotuify search "imagine dragons" --play --index 1',
  ],
  'resolve-tracks': ['spotuify resolve-tracks --from plan.json --format jsonl > candidates.jsonl'],
  queue: ['spotuify queue', 'spotuify queue --format json'],
  'queue-add': [
    'spotuify queue add spotify:track:...',
    'spotuify queue add --search "never too much"',
    'spotuify search "luther vandross" --format ids | spotuify queue add --format json',
  ],
  playlists: ['spotuify playlists', 'spotuify playlists --format ids'],
  play: ['spotuify play "imagine dragons"', 'spotuify play "ambient coding music" --type playlist'],
  'play-uri': ['spotuify play-uri spotify:track:...', 'spotuify search "never too much" --format ids | head -n 1 | xargs spotuify play-uri'],
  next: ['spotuify next'],
  previous: ['spotuify previous'],
  pause: ['spotuify pause'],
  resume: ['spotuify resume'],
  toggle: ['spotuify toggle'],
  seek: ['spotuify seek +15s', 'spotuify seek 2m'],
  volume: ['spotuify volume 70'],
  shuffle: ['spotuify shuffle toggle', 'spotuify shuffle on'],
  repeat: ['spotuify repeat off', 'spotuify repeat track'],
  transfer: ['spotuify transfer spotuify-hume', 'spotuify devices --format ids | head -n 1 | xargs spotuify transfer'],
  playlist: ['spotuify playlist tracks "Quiet Storm"', 'spotuify playlist add "Quiet Storm" spotify:track:... --dry-run'],
  'playlist-plan': ['spotuify playlist plan "exile and returning home" --format json > plan.json'],
  'playlist-create': [
    'spotuify playlist create "Exile and Return" --from candidates.jsonl --dry-run',
    'spotuify playlist create "Exile and Return" --from candidates.jsonl --yes --format json',
  ],
  'playlist-tracks': ['spotuify playlist tracks "Quiet Storm" --format jsonl'],
  'playlist-play': ['spotuify playlist play "Quiet Storm"'],
  'playlist-add': ['spotuify playlist add "Quiet Storm" spotify:track:... --dry-run', 'spotuify playlist add "Quiet Storm" --ids tracks.txt --yes'],
  'playlist-add-current': ['spotuify playlist add-current "Coding"'],
  library: ['spotuify library tracks'],
  'library-tracks': ['spotuify library tracks --limit 50 --format json'],
  lyrics: ['spotuify lyrics show', 'spotuify lyrics fetch spotify:track:...'],
  'lyrics-show': ['spotuify lyrics show', 'spotuify lyrics show --track spotify:track:... --format json'],
  'lyrics-fetch': ['spotuify lyrics fetch spotify:track:...'],
  'lyrics-offset': ['spotuify lyrics offset spotify:track:... +50ms'],
  viz: ['spotuify viz status', 'spotuify viz enable'],
  'viz-enable': ['spotuify viz enable'],
  'viz-disable': ['spotuify viz disable'],
  'viz-source': ['spotuify viz source auto', 'spotuify viz source loopback'],
  'viz-status': ['spotuify viz status --format json'],
  like: ['spotuify like current', 'spotuify like spotify:track:... --format json'],
  save: ['spotuify save current', 'spotuify save spotify:album:...'],
  logs: ['spotuify logs path', 'spotuify logs tail 200 --follow'],
  'logs-path': ['spotuify logs path'],
  'logs-tail': ['spotuify logs tail 200', 'spotuify logs tail --follow --format jsonl'],
  config: ['spotuify config path', 'spotuify config get player.backend'],
  'config-path': ['spotuify config path'],
  'config-init': ['spotuify config init'],
  'config-get': ['spotuify config get client_id'],
  'config-set': ['spotuify config set player.bitrate 320'],
  analytics: ['spotuify analytics events --limit 20', 'spotuify analytics top --kind artists'],
  'analytics-events': ['spotuify analytics events --limit 50 --format jsonl'],
  'analytics-top': ['spotuify analytics top --kind tracks --since 30d --limit 25'],
  'analytics-habits': ['spotuify analytics habits --window week --format json'],
  'analytics-search': ['spotuify analytics search --mode normalized --limit 20'],
  'analytics-rediscovery': ['spotuify analytics rediscovery --gap 90d'],
  'analytics-rebuild': ['spotuify analytics rebuild', 'spotuify analytics rebuild --since 2026-05-01T00:00:00Z'],
  'analytics-prune': ['spotuify analytics prune', 'spotuify analytics prune --apply'],
  'analytics-export': ['spotuify analytics export --target listenbrainz --since 2026-01-01'],
  'analytics-import': ['spotuify analytics import --target lastfm'],
  ops: ['spotuify ops log', 'spotuify ops undo --dry-run'],
  'ops-log': ['spotuify ops log --limit 20 --format json'],
  'ops-show': ['spotuify ops show 018f... --diff'],
  'ops-undo': ['spotuify ops undo --dry-run', 'spotuify ops undo 018f... --yes'],
  'ops-redo': ['spotuify ops redo 018f...'],
  generate: ['spotuify generate completions zsh > _spotuify', 'spotuify generate man-page > spotuify.1'],
  'generate-completions': ['spotuify generate completions zsh > _spotuify'],
  'generate-man-page': ['spotuify generate man-page > spotuify.1'],
  reload: ['spotuify reload'],
  reconnect: ['spotuify reconnect'],
  'bug-report': ['spotuify bug-report --log-lines 500 --output spotuify-report.tar.gz'],
  reindex: ['spotuify reindex --format json'],
  cache: ['spotuify cache status', 'spotuify cache repair'],
  'cache-status': ['spotuify cache status --format json'],
  'cache-reset': ['spotuify cache reset --confirm'],
  'cache-repair': ['spotuify cache repair --format json'],
  sync: ['spotuify sync', 'spotuify sync library --format json'],
};

const EXTRA_HELP = {
  'daemon-install-service': manualHelp({
    about: 'Install the platform auto-start service for the spotuify daemon',
    usage: 'spotuify daemon install-service [OPTIONS]',
  }),
  'daemon-uninstall-service': manualHelp({
    about: 'Remove the platform auto-start service for the spotuify daemon',
    usage: 'spotuify daemon uninstall-service [OPTIONS]',
  }),
  lyrics: manualHelp({
    about: 'Synced lyrics operations',
    usage: 'spotuify lyrics [OPTIONS] <COMMAND>',
    commands: [
      ['show', 'Print lyrics for the current or specified track'],
      ['fetch', 'Force-refresh cached lyrics for a Spotify track URI'],
      ['offset', 'Save a per-track lyrics timing offset'],
      ['help', 'Print this message or the help of the given subcommand(s)'],
    ],
  }),
  'lyrics-show': manualHelp({
    about: 'Print lyrics for the current or specified track',
    usage: 'spotuify lyrics show [OPTIONS]',
    options: [
      '      --track <TRACK>    Spotify track URI. Defaults to the current now-playing track',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'lyrics-fetch': manualHelp({
    about: 'Force-refresh cached lyrics for a Spotify track URI',
    usage: 'spotuify lyrics fetch [OPTIONS] <TRACK_URI>',
    args: ['  <TRACK_URI>  Spotify track URI'],
    options: ['      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]'],
  }),
  'lyrics-offset': manualHelp({
    about: 'Save a per-track lyrics timing offset',
    usage: 'spotuify lyrics offset [OPTIONS] <TRACK_URI> <OFFSET>',
    args: [
      '  <TRACK_URI>  Spotify track URI',
      '  <OFFSET>     Offset in milliseconds, with optional ms suffix',
    ],
    options: ['      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]'],
  }),
  'analytics-top': manualHelp({
    about: 'Top-N most-played tracks / artists / albums / playlists',
    usage: 'spotuify analytics top [OPTIONS]',
    options: [
      '      --kind <KIND>      tracks, artists, albums, or playlists [default: tracks]',
      '      --since <SINCE>    Time window: 7d, 30d, 90d, 365d, or all [default: 30d]',
      '      --limit <LIMIT>    Maximum rows to print [default: 25]',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'analytics-habits': manualHelp({
    about: 'Habit metrics bucketed by day / week / month',
    usage: 'spotuify analytics habits [OPTIONS]',
    options: [
      '      --window <WINDOW>  day, week, or month [default: week]',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'analytics-search': manualHelp({
    about: 'Search history with raw or normalized query mode',
    usage: 'spotuify analytics search [OPTIONS]',
    options: [
      '      --mode <MODE>      raw or normalized [default: raw]',
      '      --limit <LIMIT>    Maximum rows to print [default: 50]',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'analytics-rediscovery': manualHelp({
    about: 'Tracks worth re-discovering',
    usage: 'spotuify analytics rediscovery [OPTIONS]',
    options: [
      '      --gap <GAP>        Rediscovery gap: 30d, 90d, or 365d [default: 90d]',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'analytics-rebuild': manualHelp({
    about: 'Recompute derived listen facts from analytics_events',
    usage: 'spotuify analytics rebuild [OPTIONS]',
    options: [
      '      --since <SINCE>    ISO timestamp to rebuild from; omit for full rebuild',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'analytics-prune': manualHelp({
    about: 'Apply retention prune to raw events and progress samples',
    usage: 'spotuify analytics prune [OPTIONS]',
    options: [
      '      --apply            Actually delete rows. Without this flag, print a dry-run report',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'analytics-export': manualHelp({
    about: 'Export qualified listens to ListenBrainz or Last.fm',
    usage: 'spotuify analytics export [OPTIONS]',
    options: [
      '      --target <TARGET>  Export target [possible values: listenbrainz, lastfm]',
      '      --since <SINCE>    ISO timestamp to export from',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'analytics-import': manualHelp({
    about: 'Import historical scrobbles from ListenBrainz or Last.fm',
    usage: 'spotuify analytics import [OPTIONS]',
    options: [
      '      --target <TARGET>  Import target [possible values: listenbrainz, lastfm]',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  ops: manualHelp({
    about: 'Inspect, undo, or redo recorded operations',
    usage: 'spotuify ops [OPTIONS] <COMMAND>',
    commands: [
      ['log', 'List recorded operations, newest first'],
      ['show', 'Inspect a single operation by id'],
      ['undo', 'Undo a recorded operation; defaults to the last reversible op'],
      ['redo', 'Redo a previously-undone operation'],
      ['help', 'Print this message or the help of the given subcommand(s)'],
    ],
  }),
  'ops-log': manualHelp({
    about: 'List recorded operations, newest first',
    usage: 'spotuify ops log [OPTIONS]',
    options: [
      '      --limit <LIMIT>    Maximum rows to print [default: 20]',
      '      --since <SINCE>    Relative time or ISO timestamp',
      '      --source <SOURCE>  cli, tui, mcp, agent, or daemon-internal',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'ops-show': manualHelp({
    about: 'Inspect a single operation by id',
    usage: 'spotuify ops show [OPTIONS] <ID>',
    args: ['  <ID>  Operation id'],
    options: [
      '      --diff             Render a human-readable diff of what undo would do',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'ops-undo': manualHelp({
    about: 'Undo a recorded operation; defaults to the last reversible op',
    usage: 'spotuify ops undo [OPTIONS] [ID]',
    args: ['  [ID]  Operation id. Omit to undo the last reversible op'],
    options: [
      '      --dry-run          Predict the reversal without executing',
      '      --yes              Skip confirmation prompts',
      '      --force            Override snapshot-id conflict detection',
      '      --since <SINCE>    Bulk-undo every reversible op newer than this',
      '      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]',
    ],
  }),
  'ops-redo': manualHelp({
    about: 'Redo a previously-undone operation',
    usage: 'spotuify ops redo [OPTIONS] [ID]',
    args: ['  [ID]  Operation id. Omit to redo the last undone op'],
    options: ['      --format <FORMAT>  Output format [default: table] [possible values: table, json, jsonl, csv, ids]'],
  }),
  generate: manualHelp({
    about: 'Emit shell completions or a man page',
    usage: 'spotuify generate [OPTIONS] <COMMAND>',
    commands: [
      ['completions', 'Emit shell completion script to stdout'],
      ['man-page', 'Emit man-page source to stdout'],
      ['help', 'Print this message or the help of the given subcommand(s)'],
    ],
  }),
  'generate-completions': manualHelp({
    about: 'Emit shell completion script to stdout',
    usage: 'spotuify generate completions [OPTIONS] <SHELL>',
    args: ['  <SHELL>  Shell to generate completions for'],
  }),
  'generate-man-page': manualHelp({
    about: 'Emit roff man page source to stdout',
    usage: 'spotuify generate man-page [OPTIONS]',
  }),
  reload: manualHelp({
    about: 'Ask the running daemon to reload config.toml',
    usage: 'spotuify reload [OPTIONS]',
  }),
  reconnect: manualHelp({
    about: 'Force the daemon to rebuild its upstream Spotify session',
    usage: 'spotuify reconnect [OPTIONS]',
  }),
  'bug-report': manualHelp({
    about: 'Bundle a redacted diagnostic tarball for bug reports',
    usage: 'spotuify bug-report [OPTIONS]',
    options: [
      '      --log-lines <LOG_LINES>  Last N log lines to include [default: 200]',
      '      --output <OUTPUT>        Output path. Defaults to ./spotuify-bug-report-<ts>.tar.gz',
    ],
  }),
};

function manualHelp({ about, usage, args = [], options = [], commands = [] }) {
  const lines = [about, '', `Usage: ${usage}`];
  if (args.length) lines.push('', 'Arguments:', ...args);
  if (commands.length) {
    lines.push('', 'Commands:');
    const width = Math.max(...commands.map(([name]) => name.length));
    for (const [name, desc] of commands) lines.push(`  ${name.padEnd(width)}  ${desc}`);
  }
  lines.push('', 'Options:');
  if (options.length) lines.push(...options);
  lines.push(...GLOBAL_OPTIONS);
  return normalizeText(lines.join('\n'));
}

function normalizeText(value) {
  return value.replaceAll('—', '-').replaceAll('–', '-');
}

function snapshotBody(raw) {
  const match = raw.match(/^---[\s\S]*?\n---\n([\s\S]*)$/);
  return normalizeText((match ? match[1] : raw).trimEnd());
}

function titleFor(slug, help) {
  if (slug === 'index') return 'CLI Reference';
  const usage = (help.match(/^Usage:\s+(.+)$/m) || [])[1];
  if (usage) return usage.replace(/\s+\[OPTIONS\].*$/, '').replace(/\s+<COMMAND>.*$/, '');
  return `spotuify ${slug.replaceAll('-', ' ')}`;
}

function descriptionFor(help) {
  return help.split('\n').find((line) => line.trim() && !line.startsWith('Usage:'))?.trim() || 'spotuify command reference.';
}

function writePage(slug, help) {
  const normalizedHelp = normalizeText(help);
  const title = titleFor(slug, normalizedHelp);
  const description = descriptionFor(normalizedHelp).replaceAll('"', '\\"');
  const examples = COMMAND_EXAMPLES[slug] || [`spotuify ${slug.replaceAll('-', ' ')}`];
  const body = [
    '---',
    `title: "${title}"`,
    `description: "${description}"`,
    '---',
    '',
    GENERATED,
    '',
    '## When to use it',
    '',
    descriptionFor(normalizedHelp),
    '',
    '## Examples',
    '',
    '```bash',
    ...examples,
    '```',
    '',
    '## Help',
    '',
    '```text',
    normalizedHelp,
    '```',
    '',
  ].join('\n');
  writeFileSync(join(OUT_DIR, `${slug}.md`), body);
}

function cleanGeneratedOutput() {
  mkdirSync(OUT_DIR, { recursive: true });
  for (const entry of readdirSync(OUT_DIR)) {
    if (!entry.endsWith('.md') || entry === 'concepts.md') continue;
    const file = join(OUT_DIR, entry);
    const text = readFileSync(file, 'utf8');
    if (text.includes(GENERATED) || entry !== 'concepts.md') rmSync(file);
  }
}

function main() {
  cleanGeneratedOutput();
  const pages = new Map();

  for (const entry of readdirSync(SNAPSHOT_DIR)) {
    if (!entry.startsWith(PREFIX) || !entry.endsWith('.snap')) continue;
    const name = entry.slice(PREFIX.length, -'.snap'.length);
    const slug = name === 'root' ? 'index' : name.replaceAll('_', '-');
    const help = snapshotBody(readFileSync(join(SNAPSHOT_DIR, entry), 'utf8'));
    pages.set(slug, help);
  }

  for (const [slug, help] of Object.entries(EXTRA_HELP)) {
    if (!pages.has(slug)) pages.set(slug, help);
  }

  for (const [slug, help] of [...pages.entries()].sort(([a], [b]) => a.localeCompare(b))) {
    writePage(slug, help);
  }

  const missing = EXPECTED_PAGES.filter((page) => !pages.has(page));
  if (missing.length) {
    console.error(`[cli-reference] missing pages: ${missing.join(', ')}`);
    process.exit(1);
  }

  console.log(`[cli-reference] wrote ${pages.size} pages`);
}

main();
