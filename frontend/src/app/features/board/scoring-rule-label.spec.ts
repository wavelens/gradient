/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { scoringRuleClassNameToReadableLabel } from './scoring-rule-label';

describe('scoringRuleClassNameToReadableLabel', () => {
  it('reads scheduler rule type names as plain words', () => {
    expect(scoringRuleClassNameToReadableLabel('EstimatedTimeRule')).toBe('Estimated time');
    expect(scoringRuleClassNameToReadableLabel('ReserveFetchWorkersRule')).toBe('Reserve fetch workers');
    expect(scoringRuleClassNameToReadableLabel('QosRule')).toBe('Service quality');
  });

  it('keeps a name it cannot split', () => {
    expect(scoringRuleClassNameToReadableLabel('wait')).toBe('wait');
  });
});
