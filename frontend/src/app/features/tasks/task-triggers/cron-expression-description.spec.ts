/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { cronExpressionToPlainEnglish } from './cron-expression-description';

describe('cronExpressionToPlainEnglish', () => {
  it.each([
    ['0 0 2 * * *', 'Every day at 02:00 UTC'],
    ['0 30 14 * * ? *', 'Every day at 14:30 UTC'],
    ['0 0 9 * * Mon-Fri', 'Every week on Monday to Friday at 09:00 UTC'],
    ['0 15 6 * * mon,WED,friday', 'Every week on Monday, Wednesday, Friday at 06:15 UTC'],
    ['0 30 4 * * 1', 'Every week on Sunday at 04:30 UTC'],
    ['0 0 9 * * 2-6', 'Every week on Monday to Friday at 09:00 UTC'],
    ['0 0 3 1 * *', 'Every month on day 1 at 03:00 UTC'],
    ['0 5 * * * *', 'Every hour at minute 5'],
    ['0 */15 * * * *', 'Every 15 minutes'],
    ['0 0 */6 * * *', 'Every 6 hours'],
  ])('describes %s', (expression, description) => {
    expect(cronExpressionToPlainEnglish(expression)).toBe(description);
  });

  it.each([
    '',
    '0 2 * * *',
    '30 0 2 * * *',
    '0 0 2 * 1 *',
    '0 0 2 * * 0',
    '0 0 2 * * 8',
    '0 0 2 1 * Mon',
    '0 0 2 * * * 2027',
    '0 0 25 * * *',
    '0 */1 * * * *',
    '0 0-30 2 * * *',
  ])('leaves %s to the raw expression', (expression) => {
    expect(cronExpressionToPlainEnglish(expression)).toBeNull();
  });
});
