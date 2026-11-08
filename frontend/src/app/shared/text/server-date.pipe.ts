/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Pipe, PipeTransform } from '@angular/core';
import { dateTimeText } from './clock-time';

@Pipe({ name: 'serverDate' })
export class ServerTimestampToLocalDateTimePipe implements PipeTransform {
  transform(at: string | number | null | undefined): string {
    return at == null ? '' : dateTimeText(at);
  }
}
