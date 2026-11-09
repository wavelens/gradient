/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { getCiTriggerTypeLabel, getEvaluationTriggerLabel } from './evaluation-trigger';

describe('getEvaluationTriggerLabel', () => {
  it('names the trigger and the user who started the evaluation', () => {
    expect(getEvaluationTriggerLabel({ trigger: null, triggered_by: 'dave' })).toBe('Manual · dave');
    expect(getEvaluationTriggerLabel({ trigger: null })).toBe('Manual');
    expect(getEvaluationTriggerLabel({ trigger: { type: 'reporter_push' } })).toBe('Push');
    expect(getEvaluationTriggerLabel({ trigger: { type: 'reporter_pull_request' }, pr_number: 12 })).toBe('PR #12');
    expect(getEvaluationTriggerLabel({ trigger: { type: 'reporter_pull_request' } })).toBe('PR');
    expect(getEvaluationTriggerLabel({ trigger: { type: 'time' } })).toBe('Schedule (cron)');
  });

  it('names a schedule trigger the same way in the trigger list and the evaluation', () => {
    expect(getCiTriggerTypeLabel('time')).toBe(getEvaluationTriggerLabel({ trigger: { type: 'time' } }));
  });
});
