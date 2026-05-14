#!/usr/bin/env node

import { existsSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const SITE_ROOT = resolve(__dirname, '..');
const DOCS_ROOT = join(SITE_ROOT, 'src', 'content', 'docs');
const DIST_ROOT = join(SITE_ROOT, 'dist');

function walk(dir, acc = []) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) walk(full, acc);
    else if (/\.(md|mdx)$/.test(entry)) acc.push(full);
  }
  return acc;
}

function stripFrontmatter(raw) {
  if (!raw.startsWith('---')) return { body: raw, title: null, description: null };
  const end = raw.indexOf('\n---', 3);
  if (end === -1) return { body: raw, title: null, description: null };
  const fm = raw.slice(3, end).trim();
  const body = raw.slice(end + 4).replace(/^\n+/, '');
  const title = (fm.match(/^title:\s*(?:"([^"]*)"|'([^']*)'|(.+))$/m) || []).slice(1).find(Boolean) || null;
  const description = (fm.match(/^description:\s*(?:"([^"]*)"|'([^']*)'|(.+))$/m) || []).slice(1).find(Boolean) || null;
  return { body, title, description };
}

if (!existsSync(DOCS_ROOT)) process.exit(0);
if (!existsSync(DIST_ROOT)) mkdirSync(DIST_ROOT, { recursive: true });

const files = walk(DOCS_ROOT).sort();
const full = ['# spotuify full docs corpus', '', 'Generated from `site/src/content/docs`.', ''];

for (const file of files) {
  const raw = readFileSync(file, 'utf8');
  const { body, title, description } = stripFrontmatter(raw);
  const slug = '/' + relative(DOCS_ROOT, file)
    .replace(/\.(md|mdx)$/, '')
    .replace(/\\/g, '/')
    .replace(/\/index$/, '');
  full.push('---', '', `# ${title || slug}`, `URL: https://spotuify.dev${slug}/`);
  if (description) full.push(`> ${description}`);
  full.push('', body.trim(), '');

  const target = slug === '/index' ? join(DIST_ROOT, 'index.md') : join(DIST_ROOT, `${slug}.md`);
  mkdirSync(dirname(target), { recursive: true });
  const page = [];
  if (title) page.push(`# ${title}`);
  if (description) page.push(`> ${description}`);
  if (page.length) page.push('');
  page.push(body.trim());
  writeFileSync(target, page.join('\n') + '\n');
}

writeFileSync(join(DIST_ROOT, 'llms-full.txt'), full.join('\n'));
console.log(`[llms] wrote ${files.length} pages`);
