/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { splitPhaseMarkers } from './phase-markers';

describe('splitPhaseMarkers', () => {
  it('splits the log at each phase marker line', () => {
    expect(splitPhaseMarkers('a\n::gradient-phase:: epoch-1\nb\n')).toEqual([
      { kind: 'text', text: 'a\n' },
      { kind: 'phase', name: 'epoch-1' },
      { kind: 'text', text: 'b\n' },
    ]);
  });

  it('keeps a line that only mentions the marker later in the line as text', () => {
    const log = 'echo ::gradient-phase:: fake\nnext\n';
    expect(splitPhaseMarkers(log)).toEqual([{ kind: 'text', text: log }]);
  });

  it('joins consecutive markers without empty text between them', () => {
    expect(splitPhaseMarkers('::gradient-phase:: a\n::gradient-phase:: b\n')).toEqual([
      { kind: 'phase', name: 'a' },
      { kind: 'phase', name: 'b' },
    ]);
  });
});
