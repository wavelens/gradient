/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Pipe, PipeTransform, inject } from '@angular/core';
import { CurrentTimeSignal } from './clock';
import { relativeTime } from './relative-time';

@Pipe({ name: 'relativeTime', pure: false })
export class RelativeTimePipe implements PipeTransform {
  private readonly clock = inject(CurrentTimeSignal);

  transform(at: string | null | undefined): string {
    return relativeTime(at, this.clock.currentTimeMilliseconds());
  }
}
