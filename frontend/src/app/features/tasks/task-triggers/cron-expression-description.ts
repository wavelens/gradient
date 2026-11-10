/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const WEEKDAY_NAMES = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

const twoDigits = (value: number) => String(value).padStart(2, '0');

function integerUpTo(field: string, max: number): number | null {
  return /^\d{1,2}$/.test(field) && Number(field) <= max ? Number(field) : null;
}

function repeatStep(field: string): number | null {
  const match = /^\*\/(\d+)$/.exec(field);
  return match && Number(match[1]) > 1 ? Number(match[1]) : null;
}

function weekdayName(field: string): string | null {
  if (/^[1-7]$/.test(field)) return WEEKDAY_NAMES[Number(field) - 1];
  const name = field.toLowerCase();
  return WEEKDAY_NAMES.find((day) => day.toLowerCase() === name || day.slice(0, 3).toLowerCase() === name) ?? null;
}

function weekdayList(field: string): string | null {
  const parts = field.split(',').map((part) => {
    const [first, last, ...rest] = part.split('-').map(weekdayName);
    if (rest.length || !first || last === null) return null;
    return last === undefined ? first : `${first} to ${last}`;
  });
  return parts.every((part) => part !== null) ? parts.join(', ') : null;
}

// Handle common formats only. Other expressions return null, so the UI can show only the original expression as text.
export function cronExpressionToPlainEnglish(expression: string): string | null {
  const fields = expression.trim().split(/\s+/).map((field) => (field === '?' ? '*' : field));
  if (fields.length === 5) fields.unshift('0');
  if (fields.length === 7 && fields[6] === '*') fields.pop();
  if (fields.length !== 6 || fields[0] !== '0' || fields[4] !== '*') return null;
  const [, minuteField, hourField, dayOfMonthField, , dayOfWeekField] = fields;
  const minute = integerUpTo(minuteField, 59);
  const hour = integerUpTo(hourField, 23);
  const everyDay = dayOfMonthField === '*' && dayOfWeekField === '*';

  if (minute !== null && hour !== null) {
    const time = `${twoDigits(hour)}:${twoDigits(minute)}`;
    if (everyDay) return `Every day at ${time} UTC`;
    const day = integerUpTo(dayOfMonthField, 31);
    if (day && dayOfWeekField === '*') return `Every month on day ${day} at ${time} UTC`;
    const days = dayOfMonthField === '*' ? weekdayList(dayOfWeekField) : null;
    return days ? `Every week on ${days} at ${time} UTC` : null;
  }
  if (!everyDay) return null;
  if (minute !== null && hourField === '*') return `Every hour at minute ${minute}`;
  const minuteStep = repeatStep(minuteField);
  if (minuteStep && hourField === '*') return `Every ${minuteStep} minutes`;
  const hourStep = repeatStep(hourField);
  if (hourStep && minute === 0) return `Every ${hourStep} hours`;
  return null;
}
