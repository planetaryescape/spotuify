#!/usr/bin/env node

import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const siteRoot = resolve(scriptsDir, '..');
const changelogPath = resolve(siteRoot, '..', 'CHANGELOG.md');
const outputPath = resolve(siteRoot, 'src', 'content', 'docs', 'changelog.md');
const changelog = readFileSync(changelogPath, 'utf8');

if (!changelog.startsWith('# Changelog\n')) {
  throw new Error(`${changelogPath} must start with "# Changelog"`);
}

const body = changelog.replace(/^# Changelog\n+/, '');
const page = `---
title: "Changelog"
description: "Read what changed in every published spotuify release."
---

<!-- generated from /CHANGELOG.md; edit the source file -->

${body}`;

writeFileSync(outputPath, page);
console.log('[changelog] wrote src/content/docs/changelog.md');
