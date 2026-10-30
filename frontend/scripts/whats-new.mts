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

interface Page {
  title: string;
  html: string;
  icon?: string;
  image?: { src: string; alt: string };
}

function fail(message: string): never {
  console.error(message);
  process.exit(1);
}

function image(file: string, alt: string, path: string): Page['image'] {
  if (path.includes('..') || path.startsWith('/') || !existsSync(join(NOTES, path))) {
    fail(`${file}: image ${path} is not a file next to the notes in docs/whats-new`);
  }
  return { src: `whats-new/${path}`, alt };
}

function toPages(file: string, markdown: string): Page[] {
  const pages: Page[] = [];
  let page: Page | null = null;
  let html: string[] = [];
  let list: string[] = [];
  let paragraph: string[] = [];
  const flush = (): void => {
    if (list.length) html.push(`<ul>${list.map((item) => `<li>${item}</li>`).join('')}</ul>`);
    if (paragraph.length) html.push(`<p>${inline(paragraph.join(' '))}</p>`);
    list = [];
    paragraph = [];
  };
  const close = (): void => {
    flush();
    if (page) pages.push({ ...page, html: html.join('') });
    html = [];
  };
  for (const line of markdown.split('\n').map((l) => l.trim())) {
    const picture = /^!\[([^\]]*)\]\(([^)\s]+)\)$/.exec(line);
    if (line.startsWith('<!--')) continue;
    if (/^#{1,6} /.test(line)) {
      close();
      page = { title: line.replace(/^#+ /, ''), html: '' };
    } else if (!page) {
      if (line !== '') fail(`${file}: every page starts with a "## Title" line`);
    } else if (picture) {
      page.image = image(file, picture[1], picture[2]);
    } else if (/^icon: [a-z0-9_]+$/.test(line)) {
      page.icon = line.slice(6);
    } else if (line === '') {
      flush();
    } else if (line.startsWith('- ')) {
      if (paragraph.length) flush();
      list.push(inline(line.slice(2)));
    } else if (list.length) {
      list[list.length - 1] += ` ${inline(line)}`;
    } else {
      paragraph.push(line);
    }
  }
  close();
  return pages;
}

function render(): string {
  const files = existsSync(NOTES) ? readdirSync(NOTES).filter((file) => file.endsWith('.md') && file !== 'README.md') : [];
  const misnamed = files.filter((file) => !/^\d+\.\d+\.md$/.test(file));
  if (misnamed.length) fail(`Release notes are named <major>.<minor>.md, patch releases get none: ${misnamed.join(', ')}`);
  const notes = files
    .map((file) => ({ version: file.slice(0, -3), pages: toPages(file, readFileSync(join(NOTES, file), 'utf8')) }))
    .sort((a, b) => b.version.localeCompare(a.version, undefined, { numeric: true }));
  const entries = notes.map((n) => `  { version: ${JSON.stringify(n.version)}, pages: ${JSON.stringify(n.pages)} },\n`).join('');
  return [
    '// Generated from docs/whats-new by scripts/whats-new.mts. Do not edit.\n',
    'export interface WhatsNewPage {\n  title: string;\n  html: string;\n  icon?: string;\n  image?: { src: string; alt: string };\n}\n\n',
    'export interface ReleaseNotes {\n  version: string;\n  pages: readonly WhatsNewPage[];\n}\n\n',
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
