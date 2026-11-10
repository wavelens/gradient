/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ansiToHtml } from './ansi';

const RED = 'color:var(--gr-status-danger)';
const GREEN = 'color:var(--gr-status-success)';

describe('ansiToHtml', () => {
  it('escapes markup in the log text', () => {
    expect(ansiToHtml('<b>&')).toBe('&lt;b&gt;&amp;');
  });

  it('closes a colour at its reset', () => {
    expect(ansiToHtml('\x1b[31mred\x1b[0m plain')).toBe(`<span style="${RED}">red</span> plain`);
  });

  it('closes colours a line leaves open', () => {
    expect(ansiToHtml('\x1b[1m\x1b[32mok')).toBe(`<span style="${GREEN};font-weight:bold">ok</span>`);
  });

  it("applies each code of a combined sequence such as gcc's bold red", () => {
    expect(ansiToHtml('\x1b[01;31merror\x1b[m')).toBe(`<span style="${RED};font-weight:bold">error</span>`);
    expect(ansiToHtml('\x1b[1;4;31mx')).toBe(`<span style="${RED};font-weight:bold;text-decoration:underline">x</span>`);
  });

  it('resets and sets a colour in one sequence without the old colour bleeding on', () => {
    expect(ansiToHtml('\x1b[1;31ma\x1b[0;32mb')).toBe(`<span style="${RED};font-weight:bold">a</span><span style="${GREEN}">b</span>`);
  });

  it('Render 256-color, truecolor and background codes', () => {
    expect(ansiToHtml('\x1b[38;5;10ma')).toBe(`<span style="${GREEN}">a</span>`);
    expect(ansiToHtml('\x1b[38;5;196ma')).toBe('<span style="color:rgb(255,0,0)">a</span>');
    expect(ansiToHtml('\x1b[38;2;1;2;3ma')).toBe('<span style="color:rgb(1,2,3)">a</span>');
    expect(ansiToHtml('\x1b[41ma\x1b[49mb')).toBe('<span style="background:color-mix(in srgb, var(--gr-status-danger) 30%, transparent)">a</span>b');
  });

  it('Strip terminal title and hyperlink sequences, keep link text.', () => {
    expect(ansiToHtml('\x1b]0;title\x07\x1b]8;;https://example.com\x1b\\docs\x1b]8;;\x1b\\ end')).toBe('docs end');
  });

  it('Keep private use characters as text, not markers.', () => {
    expect(ansiToHtml('icon')).toBe('icon');
  });

  it('marks each match of a search, ignoring case and colour codes', () => {
    const html = ansiToHtml('Err \x1b[31merr\x1b[0mor', 'err');
    expect(html.match(/<mark[^>]*>/g)).toHaveLength(2);
    expect(html.replace(/<mark[^>]*>/g, '[').replace(/<\/mark>/g, ']')).toBe(`[Err] <span style="${RED}">[err]</span>or`);
  });

  it('keeps a match across a colour change inside the spans', () => {
    const html = ansiToHtml('ab\x1b[31mcd', 'bc');
    expect(html.replace(/<mark[^>]*>/g, '[').replace(/<\/mark>/g, ']')).toBe(`a[b]<span style="${RED}">[c]d</span>`);
  });

  it('escapes markup next to a match', () => {
    expect(ansiToHtml('a<b', '<b').replace(/<mark[^>]*>/, '[')).toBe('a[&lt;b</mark>');
  });

  it('drops cursor movements', () => {
    expect(ansiToHtml('\x1b[2K\x1b[?25ldone')).toBe('done');
  });
});
