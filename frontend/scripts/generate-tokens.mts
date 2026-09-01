/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { PALETTE, DARK_ROLES, LIGHT_ROLES } from '../src/app/styles/tokens.ts';

const TARGET = join(dirname(fileURLToPath(import.meta.url)), '../src/app/styles/_themes.scss');

const block = (entries: Record<string, string>, wrap: (v: string) => string): string =>
  Object.entries(entries)
    .map(([name, value]) => `  ${name}: ${wrap(value)};`)
    .join('\n');

function render(): string {
  return `/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// GENERATED FROM tokens.ts BY scripts/generate-tokens.ts. Do not edit by hand.

@mixin palette {
${block(PALETTE, (v) => v)}
}

@mixin dark-roles {
  color-scheme: dark;
${block(DARK_ROLES, (v) => `var(${v})`)}
}

@mixin light-roles {
  color-scheme: light;
${block(LIGHT_ROLES, (v) => `var(${v})`)}
}

:root {
  @include palette;
  @include dark-roles;
}

:root[data-theme='light'] {
  @include light-roles;
}

@media (prefers-color-scheme: light) {
  :root:not([data-theme='dark']) {
    @include light-roles;
  }
}
`;
}

const expected = render();

if (process.argv.includes('--check')) {
  const actual = readFileSync(TARGET, 'utf8');
  if (actual !== expected) {
    console.error('_themes.scss is out of sync with tokens.ts. Run: pnpm -C frontend tokens:generate');
    process.exit(1);
  }
  console.log('_themes.scss is in sync with tokens.ts');
} else {
  writeFileSync(TARGET, expected);
  console.log(`wrote ${TARGET}`);
}
