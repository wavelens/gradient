/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { getEvaluationMessageSourceLabel, removeNixErrorInternalsFromEvaluationMessage } from './evaluation-message-and-error-display';

describe('removeNixErrorInternalsFromEvaluationMessage', () => {
  it('keeps the thrown message and drops the Nix prefixes and error class', () => {
    expect(removeNixErrorInternalsFromEvaluationMessage('is_derivation: Evaluation error: [nix::ThrownError] broken on purpose'))
      .toBe('broken on purpose');
    expect(removeNixErrorInternalsFromEvaluationMessage("failed to evaluate 'packages': [nix::SQLiteError] database is locked"))
      .toBe("failed to evaluate 'packages': database is locked");
  });
});

describe('getEvaluationMessageSourceLabel', () => {
  it('names the attribute of a nix-eval source and hides the bare internal tag', () => {
    expect(getEvaluationMessageSourceLabel('nix-eval:packages.x86_64-linux.broken')).toBe('packages.x86_64-linux.broken');
    expect(getEvaluationMessageSourceLabel('nix-eval')).toBeNull();
    expect(getEvaluationMessageSourceLabel(null)).toBeNull();
    expect(getEvaluationMessageSourceLabel('flake-prefetch')).toBe('flake-prefetch');
  });
});
