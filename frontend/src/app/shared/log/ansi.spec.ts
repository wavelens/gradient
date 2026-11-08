/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ansiToHtml } from './ansi';

describe('ansiToHtml', () => {
  it('escapes markup in the log text', () => {
    expect(ansiToHtml('<b>&')).toBe('&lt;b&gt;&amp;');
  });

  it('closes a colour at its reset', () => {
    expect(ansiToHtml('\x1b[31mred\x1b[0m plain')).toBe('<span style="color:var(--gr-status-danger)">red</span> plain');
  });

  it('closes colours a line leaves open', () => {
    expect(ansiToHtml('\x1b[1m\x1b[32mok')).toBe('<span style="font-weight:bold"><span style="color:var(--gr-status-success)">ok</span></span>');
  });

  it('marks each match of a search, ignoring case and colour codes', () => {
    const html = ansiToHtml('Err \x1b[31merr\x1b[0mor', 'err');
    expect(html.match(/<mark[^>]*>/g)).toHaveLength(2);
    expect(html.replace(/<mark[^>]*>/g, '[').replace(/<\/mark>/g, ']')).toBe('[Err] <span style="color:var(--gr-status-danger)">[err</span>]or');
  });

  it('escapes markup next to a match', () => {
    expect(ansiToHtml('a<b', '<b').replace(/<mark[^>]*>/, '[')).toBe('a[&lt;b</mark>');
  });

  it('drops cursor movements and unknown styles', () => {
    expect(ansiToHtml('\x1b[2K\x1b[38;5;10mdone')).toBe('done');
  });
});
