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

let failed = false;
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
console.log(`@shared/ui barrel is complete; every module has a spec.`);
