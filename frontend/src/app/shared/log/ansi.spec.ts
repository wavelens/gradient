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
    expect(ansiToHtml('\x1b[31mred\x1b[0m plain')).toBe('<span style="color:#ef4444">red</span> plain');
  });

  it('closes colours a line leaves open', () => {
    expect(ansiToHtml('\x1b[1m\x1b[32mok')).toBe('<span style="font-weight:bold"><span style="color:#22c55e">ok</span></span>');
  });

  it('drops cursor movements and unknown styles', () => {
    expect(ansiToHtml('\x1b[2K\x1b[38;5;10mdone')).toBe('done');
  });
});
