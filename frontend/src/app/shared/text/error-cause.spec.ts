/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { getLastErrorCause } from './error-cause';

describe('getLastErrorCause', () => {
  it('Keep the last cause in an error chain', () => {
    expect(getLastErrorCause('send HTTP request: client error (Connect): dns error: Name or service not known')).toBe('Name or service not known');
    expect(getLastErrorCause('timed out')).toBe('Timed out');
  });
});
