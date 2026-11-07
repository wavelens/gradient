/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';
import ts from 'typescript';
import { parseTemplate, TmplAstBoundText, TmplAstElement, TmplAstText, type TmplAstNode } from '@angular/compiler';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const SOURCES = [join(ROOT, 'src/app'), join(ROOT, 'packages/ui/src')];
const NAME_ATTRIBUTES = new Set(['label', 'aria-label', 'aria-labelledby', 'title', 'aria-hidden']);
const ICON_ELEMENTS = new Set(['gr-icon', 'gr-status-icon', 'gr-loading-spinner']);

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) return files(path);
    return /\.(html|ts)$/.test(entry) && !entry.endsWith('.spec.ts') ? [path] : [];
  });
}

function inlineTemplates(path: string, source: string): string[] {
  const file = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true);
  const out: string[] = [];
  const visit = (node: ts.Node) => {
    if (
      ts.isPropertyAssignment(node) &&
      node.name.getText(file) === 'template' &&
      (ts.isNoSubstitutionTemplateLiteral(node.initializer) || ts.isStringLiteral(node.initializer))
    ) {
      out.push(node.initializer.text);
    }
    ts.forEachChild(node, visit);
  };
  visit(file);
  return out;
}

function bindingNames(element: TmplAstElement): string[] {
  return [
    ...element.attributes.map((a) => (a.value.trim() ? a.name : '')),
    ...element.inputs.map((i) => i.name),
  ];
}

function isIconOnly(element: TmplAstElement): boolean {
  const names = bindingNames(element);
  if (names.includes('grButton') || element.attributes.some((a) => a.name === 'grButton')) {
    if (!names.includes('icon')) return false;
  } else if (element.name !== 'button' && element.name !== 'a') {
    return false;
  }
  return !hasText(element.children);
}

function hasText(nodes: TmplAstNode[]): boolean {
  return nodes.some((node) => {
    if (node instanceof TmplAstText) return node.value.trim() !== '';
    if (node instanceof TmplAstBoundText) return true;
    if (node instanceof TmplAstElement) {
      if (ICON_ELEMENTS.has(node.name) || node.attributes.some((a) => a.name === 'aria-hidden')) return false;
      if (node.attributes.some((a) => a.name === 'class' && a.value.includes('material-symbols'))) return false;
      return true;
    }
    return 'children' in node && Array.isArray(node.children) ? hasText(node.children as TmplAstNode[]) : true;
  });
}

function unnamed(nodes: TmplAstNode[], out: TmplAstElement[]): TmplAstElement[] {
  for (const node of nodes) {
    if (node instanceof TmplAstElement && isIconOnly(node) && !bindingNames(node).some((n) => NAME_ATTRIBUTES.has(n.replace(/^attr\./, '')))) {
      out.push(node);
    }
    if ('children' in node && Array.isArray(node.children)) unnamed(node.children as TmplAstNode[], out);
    for (const branch of ['branches', 'cases'] as const) {
      if (branch in node) for (const b of (node as unknown as Record<string, { children: TmplAstNode[] }[]>)[branch]) unnamed(b.children, out);
    }
    for (const block of ['empty', 'placeholder', 'loading', 'error'] as const) {
      const child = (node as unknown as Record<string, { children: TmplAstNode[] } | null>)[block];
      if (child && typeof child === 'object' && 'children' in child) unnamed(child.children, out);
    }
  }
  return out;
}

const problems: string[] = [];
for (const path of SOURCES.flatMap(files)) {
  const source = readFileSync(path, 'utf8');
  const templates = path.endsWith('.html') ? [source] : inlineTemplates(path, source);
  for (const template of templates) {
    const { nodes } = parseTemplate(template, path, { preserveWhitespaces: false });
    for (const element of unnamed(nodes, [])) {
      problems.push(`${relative(ROOT, path)}: <${element.name}> at line ${element.sourceSpan.start.line + 1}: Icon only, no aria-label`);
    }
  }
}

if (problems.length) {
  console.error(problems.join('\n'));
  process.exit(1);
}
console.log('All icon-only buttons have accessible names.');
