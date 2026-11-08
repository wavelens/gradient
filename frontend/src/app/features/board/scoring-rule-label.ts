/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const scoringRuleLabelByClassName: Record<string, string> = {
  QosRule: 'Service quality',
};

export function scoringRuleClassNameToReadableLabel(rule: string): string {
  const known = scoringRuleLabelByClassName[rule];
  if (known) return known;
  const words = rule.replace(/Rule$/, '').split(/(?=[A-Z])/).filter(Boolean);
  if (words.length === 0) return rule;
  return [words[0], ...words.slice(1).map((w) => w.toLowerCase())].join(' ');
}
