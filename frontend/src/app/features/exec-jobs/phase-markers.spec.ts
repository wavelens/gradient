/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { phaseMarker } from './phase-markers';

describe('phaseMarker', () => {
  it('names the phase a marker line starts', () => {
    expect(phaseMarker('::gradient-phase:: epoch-1')).toBe('epoch-1');
  });

  it('ignores the carriage return of a CRLF line', () => {
    expect(phaseMarker('::gradient-phase:: epoch-1\r')).toBe('epoch-1');
  });

  it('keeps a line that only mentions the marker later in the line as text', () => {
    expect(phaseMarker('echo ::gradient-phase:: fake')).toBeNull();
  });
});
