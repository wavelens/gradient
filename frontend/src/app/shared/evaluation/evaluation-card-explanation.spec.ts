/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { clockTime } from '@shared/text';
import { evaluationRetryOrRerunLabel, getAbortedEvaluationExplanationWithReplacementId } from './evaluation-card-explanation';

const aborted = {
  status: 'Aborted' as const,
  finished_at: '2026-01-01T16:31:00',
  updated_at: '2026-01-01T16:32:00',
};

describe('getAbortedEvaluationExplanationWithReplacementId', () => {
  it('names the user and the finish time of a user abort', () => {
    const explanation = getAbortedEvaluationExplanationWithReplacementId({ ...aborted, abort_reason: 'user', aborted_by: 'dave' });
    expect(explanation).toEqual({ text: `Aborted by dave at ${clockTime(aborted.finished_at)}`, replacementId: null });
  });

  it('links the newer evaluation that replaced it, with its short commit', () => {
    const explanation = getAbortedEvaluationExplanationWithReplacementId(
      { ...aborted, abort_reason: 'superseded', superseded_by: 'eval-new-0000' },
      '5c20f0c3deadbeef',
    );
    expect(explanation).toEqual({ text: 'New push: 5c20f0c3 replaced this evaluation', replacementId: 'eval-new-0000' });
  });

  it('falls back to the short evaluation id when the replacing commit is unknown', () => {
    const explanation = getAbortedEvaluationExplanationWithReplacementId({ ...aborted, abort_reason: 'superseded', superseded_by: 'abcdef0123456' });
    expect(explanation?.text).toBe('New push: abcdef01 replaced this evaluation');
  });

  it('explains an abort because no connected worker can build the systems', () => {
    const explanation = getAbortedEvaluationExplanationWithReplacementId({ ...aborted, abort_reason: 'no_connected_worker_can_build_required_systems' });
    expect(explanation?.text).toBe("Aborted: connected workers can't build the required system");
  });

  it('reads plain Aborted for old rows without a reason', () => {
    expect(getAbortedEvaluationExplanationWithReplacementId({ ...aborted, abort_reason: null })).toEqual({ text: 'Aborted', replacementId: null });
  });

  it('returns nothing for an evaluation that did not abort', () => {
    expect(getAbortedEvaluationExplanationWithReplacementId({ ...aborted, status: 'Failed', abort_reason: 'user', aborted_by: 'dave' })).toBeNull();
  });
});

describe('evaluationRetryOrRerunLabel', () => {
  const commit = 'e525f3b4aaaaaaaa';

  it('labels a retry of an earlier evaluation', () => {
    expect(evaluationRetryOrRerunLabel({ commit, trigger: null, retry_of: 'old' }, [])).toBe('Retry of e525f3b4');
  });

  it('labels a manual run of a commit an older evaluation already ran', () => {
    expect(evaluationRetryOrRerunLabel({ commit, trigger: null }, [{ commit }])).toBe('Re-run of e525f3b4');
  });

  it('leaves first manual runs and triggered runs unlabelled', () => {
    expect(evaluationRetryOrRerunLabel({ commit, trigger: null }, [{ commit: 'other' }])).toBeNull();
    expect(evaluationRetryOrRerunLabel({ commit, trigger: { id: 't', type: 'polling' } as never }, [{ commit }])).toBeNull();
  });
});
