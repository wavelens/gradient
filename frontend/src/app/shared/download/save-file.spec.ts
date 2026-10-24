/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { filenameFromDisposition } from './save-file';

describe('filenameFromDisposition', () => {
  it('prefers the server filename, which names the evaluation and date', () => {
    expect(
      filenameFromDisposition(
        'attachment; filename="gradient-report-01a05a38-2026-09-01.db"',
        'fallback.db',
      ),
    ).toBe('gradient-report-01a05a38-2026-09-01.db');
  });

  it('falls back when the header is absent or unparseable', () => {
    expect(filenameFromDisposition(null, 'fallback.db')).toBe('fallback.db');
    expect(filenameFromDisposition('attachment', 'fallback.db')).toBe('fallback.db');
  });
});
