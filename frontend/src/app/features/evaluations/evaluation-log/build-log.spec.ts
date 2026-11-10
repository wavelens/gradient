/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { BuildLogStreamDecoder } from './build-log';

describe('BuildLogStreamDecoder', () => {
  it('joins a JSON line split across two reads', () => {
    const decoder = new BuildLogStreamDecoder();
    const line = JSON.stringify('\u001b[1mbuilding\u001b[0m "foo"\n');
    expect(decoder.appendTextAndReturnCompletedLines(line.slice(0, 9))).toEqual([]);
    expect(decoder.appendTextAndReturnCompletedLines(`${line.slice(9)}\n`)).toEqual(['\u001b[1mbuilding\u001b[0m "foo"']);
  });

  it('joins a log line split across two JSON chunks', () => {
    const decoder = new BuildLogStreamDecoder();
    const read = `${JSON.stringify('one\ntw')}\n${JSON.stringify('o\nthree\n')}\n`;
    expect(decoder.appendTextAndReturnCompletedLines(read)).toEqual(['one', 'two', 'three']);
  });

  it('ignores keepalives and returns the last line without a newline at the end', () => {
    const decoder = new BuildLogStreamDecoder();
    expect(decoder.appendTextAndReturnCompletedLines(`""\n${JSON.stringify('done')}\n""\n`)).toEqual([]);
    expect(decoder.takeLastLogLineWithoutNewline()).toEqual(['done']);
    expect(decoder.takeLastLogLineWithoutNewline()).toEqual([]);
  });
});
