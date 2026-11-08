/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { formatDate } from '@angular/common';
import { LOCALE_ID, Pipe, PipeTransform, inject } from '@angular/core';
import { serverTime } from './clock-time';

/// This pipe can match the Angular `date` pipe. The pipe can read zone-less API timestamps as UTC, so all pages display the same local time.
@Pipe({ name: 'serverDate' })
export class ServerTimestampToLocalDateTimePipe implements PipeTransform {
  private readonly locale = inject(LOCALE_ID);

  transform(at: string | number | null | undefined, format = 'medium'): string {
    return at == null ? '' : formatDate(serverTime(at), format, this.locale);
  }
}
