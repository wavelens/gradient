/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const nixEvaluationMessageSourceTag = 'nix-eval';
const optionalNixEvaluationErrorPrefixRegex = /^(?:is_derivation: )?(?:Evaluation error: )?/;
const nixErrorClassTagRegex = /\[nix::\w+\]\s*/g;

export function removeNixErrorInternalsFromEvaluationMessage(message: string): string {
  return message.replace(optionalNixEvaluationErrorPrefixRegex, '').replace(nixErrorClassTagRegex, '');
}

export function getEvaluationMessageSourceLabel(source: string | null | undefined): string | null {
  if (!source || source === nixEvaluationMessageSourceTag) return null;
  return source.startsWith(`${nixEvaluationMessageSourceTag}:`) ? source.slice(nixEvaluationMessageSourceTag.length + 1) : source;
}
