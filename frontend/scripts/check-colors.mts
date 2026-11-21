/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SOURCES = [join(ROOT, 'src/app'), join(ROOT, 'packages/ui/src')];
const TOKEN_FILES = new Set(['packages/ui/src/styles/tokens.ts', 'packages/ui/src/styles/_themes.scss']);
// The terminal can render its own screen. The logo can keep brand colors. Palette colors work for both.
const PALETTE_ALLOWED = new Set([
  'src/app/shared/terminal/terminal.component.scss',
  'packages/ui/src/ui/logo/logo.component.scss',
]);
// PNG exports draw the yearly recap slides onto a canvas. That canvas can't read CSS variables, so the slides keep fixed colors in every theme.
const FIXED_COLOR_FILES = new Set(['src/app/core/yearly-instance-summary/recap-slide-color-scheme-colors.ts']);
// The viewer for build logs can turn the 256-colour and 24-bit ANSI codes in logs into rgb() colours at runtime. Only the opening part of an rgb() colour computed this way is exempt in that file.
const ANSI_LOG_COMPUTED_RGB_COLOR_FILES = new Set(['src/app/shared/log/ansi.ts']);
const rgbCallWithTemplateInterpolationStartRegex = /\brgb\(\$\{/g;

const LITERAL = /(?<![\w&/])#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{3,4})\b|\b(?:rgba?|hsla?)\((?!var\()/;
const NAMED = /\b(?:color|background(?:-color)?|fill|stroke|border(?:-\w+)?)\s*:[^;]*\b(?:white|black|red|green|blue|yellow|orange|gray|grey|purple|pink)\b/;
const PALETTE = /var\(--gr-(?:white|black|(?:gray|ink|slate|green|red|amber|blue|cyan|sage|emerald|orange|purple|pink|teal)-\d+)\)/;
const COMMENT = /^\s*(?:\/\/|\/?\*|<!--)/;

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) return files(path);
    return /\.(html|ts|scss)$/.test(entry) && !entry.endsWith('.spec.ts') ? [path] : [];
  });
}

const problems: string[] = [];
for (const path of SOURCES.flatMap(files)) {
  const file = relative(ROOT, path);
  if (TOKEN_FILES.has(file) || FIXED_COLOR_FILES.has(file)) continue;
  readFileSync(path, 'utf8').split('\n').forEach((line, index) => {
    if (COMMENT.test(line)) return;
    const where = `${file}:${index + 1}`;
    const stripped = line.replace(/var\(--gr-[\w-]+\)/g, '');
    const lineWithoutRuntimeRgbOpenings = ANSI_LOG_COMPUTED_RGB_COLOR_FILES.has(file) ? line.replace(rgbCallWithTemplateInterpolationStartRegex, '') : line;
    if (LITERAL.test(lineWithoutRuntimeRgbOpenings) || (path.endsWith('.scss') && NAMED.test(stripped))) {
      problems.push(`${where}: Color literal. Use a role from tokens.ts`);
    } else if (PALETTE.test(line) && !PALETTE_ALLOWED.has(file)) {
      problems.push(`${where}: Palette color. Use a role from tokens.ts`);
    }
  });
}

if (problems.length) {
  console.error(problems.join('\n'));
  process.exit(1);
}
console.log('All colors come from theme roles.');
