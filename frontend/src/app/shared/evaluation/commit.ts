/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export const UNKNOWN_COMMIT_LABEL = '[unknown]';

/** Short commit hash for display, or a placeholder when the commit is not yet
 * known (an input_update evaluation before its generated flake.lock is pushed). */
export function commitLabel(commit: string | null | undefined): string {
  return commit ? commit.slice(0, 8) : UNKNOWN_COMMIT_LABEL;
}

/** Evaluation title: the commit message when present, else the short hash or
 * the unknown placeholder. */
export function evaluationTitle(
  evaluation: { commit: string | null | undefined; commit_message: string | null | undefined },
): string {
  return evaluation.commit_message || commitLabel(evaluation.commit);
}
