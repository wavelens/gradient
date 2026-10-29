/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const NOTES = join(ROOT, '../docs/whats-new');
const TARGET = join(ROOT, 'src/app/core/whats-new/release-notes.generated.ts');

const escapeHtml = (text: string): string =>
  text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

function inline(text: string): string {
  return escapeHtml(text)
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/\[([^\]]+)\]\(((?:https?:\/\/|\/)[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
}

function toHtml(markdown: string): string {
  const html: string[] = [];
  let list: string[] = [];
  let paragraph: string[] = [];
  const flush = (): void => {
    if (list.length) html.push(`<ul>${list.map((item) => `<li>${item}</li>`).join('')}</ul>`);
    if (paragraph.length) html.push(`<p>${inline(paragraph.join(' '))}</p>`);
    list = [];
    paragraph = [];
  };
  for (const line of markdown.split('\n').map((l) => l.trim())) {
    if (line.startsWith('<!--')) continue;
    if (line === '') {
      flush();
    } else if (line.startsWith('## ')) {
      flush();
      html.push(`<h4>${inline(line.slice(3))}</h4>`);
    } else if (line.startsWith('- ')) {
      if (paragraph.length) flush();
      list.push(inline(line.slice(2)));
    } else if (list.length) {
      list[list.length - 1] += ` ${inline(line)}`;
    } else {
      paragraph.push(line);
    }
  }
  flush();
  return html.join('');
}

function render(): string {
  const files = existsSync(NOTES) ? readdirSync(NOTES).filter((file) => file.endsWith('.md')) : [];
  const misnamed = files.filter((file) => !/^\d+\.\d+\.md$/.test(file));
  if (misnamed.length) {
    console.error(`Release notes are named <major>.<minor>.md, patch releases get none: ${misnamed.join(', ')}`);
    process.exit(1);
  }
  const notes = files
    .map((file) => ({ version: file.slice(0, -3), html: toHtml(readFileSync(join(NOTES, file), 'utf8')) }))
    .sort((a, b) => b.version.localeCompare(a.version, undefined, { numeric: true }));
  const entries = notes.map((n) => `  { version: ${JSON.stringify(n.version)}, html: ${JSON.stringify(n.html)} },\n`).join('');
  return [
    '// Generated from docs/whats-new by scripts/whats-new.mts. Do not edit.\n',
    'export interface ReleaseNotes {\n  version: string;\n  html: string;\n}\n\n',
    `export const RELEASE_NOTES: readonly ReleaseNotes[] = [\n${entries}];\n`,
  ].join('');
}

const output = render();
if (process.argv.includes('--check')) {
  if (readFileSync(TARGET, 'utf8') !== output) {
    console.error('release-notes.generated.ts is out of date, run: pnpm whats-new');
    process.exit(1);
  }
} else {
  writeFileSync(TARGET, output);
}
