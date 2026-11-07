/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { fetchFailureHint } from './fetch-failure-hint';

describe('fetchFailureHint', () => {
  it.each([
    ['Git command failed: git@host: Permission denied (publickey).', 'deploy key'],
    ['SSH to forgejo.example.com: failed to lookup address information', 'resolve the host name'],
    ['remote: Repository not found.', 'Repository not found'],
  ])('maps %s to a fix', (error, expected) => {
    expect(fetchFailureHint(error)).toContain(expected);
  });

  it('gives no hint for unknown or missing errors', () => {
    expect(fetchFailureHint('something else broke')).toBeNull();
    expect(fetchFailureHint(null)).toBeNull();
  });
});
