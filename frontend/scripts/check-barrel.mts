/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const UI = join(dirname(fileURLToPath(import.meta.url)), '../src/app/shared/ui');

function modules(): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(UI)) {
    const path = join(UI, entry);
    if (statSync(path).isDirectory()) {
      for (const file of readdirSync(path)) {
        if (file.endsWith('.ts') && !file.endsWith('.spec.ts')) out.push(`./${entry}/${file.slice(0, -3)}`);
      }
    } else if (entry.endsWith('.ts') && entry !== 'index.ts' && !entry.endsWith('.spec.ts')) {
      out.push(`./${entry.slice(0, -3)}`);
    }
  }
  return out.sort();
}

const barrel = readFileSync(join(UI, 'index.ts'), 'utf8');
const missing = modules().filter((m) => !barrel.includes(`from '${m}'`));

const specless: string[] = [];
for (const entry of readdirSync(UI)) {
  const path = join(UI, entry);
  if (!statSync(path).isDirectory()) continue;
  const files = readdirSync(path);
  const impl = files.filter((f) => /\.(component|directive|service)\.ts$/.test(f));
  for (const f of impl) {
    if (!files.includes(f.replace(/\.ts$/, '.spec.ts'))) specless.push(`${entry}/${f}`);
  }
}

// Rule 3: a primitive that is not demonstrated in the styleguide does not exist as far as
// consumers are concerned, so an undemoed selector is a conformance failure.
const SG = join(dirname(fileURLToPath(import.meta.url)), '../src/app/features/styleguide');
const APP = join(dirname(fileURLToPath(import.meta.url)), '../src/app');

function styleguideMarkup(): string {
  const parts: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir)) {
      const path = join(dir, entry);
      if (statSync(path).isDirectory()) walk(path);
      else if (entry.endsWith('.html') || entry.endsWith('.ts')) parts.push(readFileSync(path, 'utf8'));
    }
  };
  walk(SG);
  return parts.join('\n');
}

const SELECTOR = /selector:\s*'([^']+)'/;
const EXEMPT = new Set(['gr-tooltip-panel', 'gr-confirm-dialog', 'gr-form-dialog']);

const markup = styleguideMarkup();
const undemoed: string[] = [];
for (const entry of readdirSync(UI)) {
  const path = join(UI, entry);
  if (!statSync(path).isDirectory()) continue;
  for (const file of readdirSync(path)) {
    if (!file.endsWith('.component.ts') || file.endsWith('.spec.ts')) continue;
    const match = SELECTOR.exec(readFileSync(join(path, file), 'utf8'));
    const selector = match?.[1];
    if (!selector || !selector.startsWith('gr-') || EXEMPT.has(selector)) continue;
    if (!markup.includes(`<${selector}`)) undemoed.push(selector);
  }
}

// Overriding a primitive's host display from outside silently breaks its internal
// layout: gr-checkbox loses the gap between box and label, gr-icon loses its
// rotation centre. The component owns its own display.
function hostDisplayOverrides(): string[] {
  const hits: string[] = [];
  const scan = (dir: string) => {
    for (const entry of readdirSync(dir)) {
      const path = join(dir, entry);
      if (statSync(path).isDirectory()) {
        scan(path);
        continue;
      }
      if (!entry.endsWith('.scss') && !entry.endsWith('.ts')) continue;
      // A primitive declaring its own host display lives in its own directory.
      if (path.includes('/shared/ui/')) continue;
      const text = readFileSync(path, 'utf8');
      const rule = /(^|\n)\s*(gr-[a-z-]+)(?:\s*,\s*gr-[a-z-]+)*\s*\{([^}]*)\}/g;
      for (const [, , selector, body] of text.matchAll(rule)) {
        if (/\bdisplay\s*:/.test(body)) hits.push(`${path.split('/src/')[1]} sets display on ${selector}`);
      }
    }
  };
  scan(APP);
  return hits;
}

const overrides = hostDisplayOverrides();

// Rule 5: styles declared inline escape stylelint, so a whole feature can drift
// off the token system unnoticed. Every component points at a stylesheet.
function inlineStyles(): string[] {
  const hits: string[] = [];
  const scan = (dir: string) => {
    for (const entry of readdirSync(dir)) {
      const path = join(dir, entry);
      if (statSync(path).isDirectory()) {
        scan(path);
        continue;
      }
      if (!entry.endsWith('.ts') || entry.endsWith('.spec.ts')) continue;
      if (/\n\s*styles:\s*\[/.test(readFileSync(path, 'utf8'))) hits.push(path.split('/src/')[1]);
    }
  };
  scan(APP);
  return hits;
}

const inlined = inlineStyles();
if (inlined.length) {
  console.error(`Components with inline styles, invisible to stylelint (${inlined.length}):`);
  inlined.forEach((m) => console.error(`  ${m}`));
}

let failed = inlined.length > 0;
if (overrides.length) {
  console.error(`Host display overridden from outside the component (${overrides.length}):`);
  overrides.forEach((m) => console.error(`  ${m}`));
  failed = true;
}
if (undemoed.length) {
  console.error(`Primitives with no styleguide demo (${undemoed.length}):`);
  undemoed.forEach((m) => console.error(`  ${m}`));
  failed = true;
}
if (missing.length) {
  console.error(`Not exported from @shared/ui (${missing.length}):`);
  missing.forEach((m) => console.error(`  ${m}`));
  failed = true;
}
if (specless.length) {
  console.error(`Design-system modules without a spec (${specless.length}):`);
  specless.forEach((m) => console.error(`  ${m}`));
  failed = true;
}

if (failed) process.exit(1);
console.log('@shared/ui is complete: every module is exported, specced and demonstrated.');
