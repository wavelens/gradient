/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input } from '@angular/core';
import { CommonModule } from '@angular/common';

export type MessageBannerType = 'error' | 'success' | 'info' | 'warning';

const DEFAULT_ICONS: Record<MessageBannerType, string> = {
  error: 'error',
  success: 'check_circle',
  info: 'info',
  warning: 'warning',
};

@Component({
  selector: 'gr-message-banner',
  standalone: true,
  imports: [CommonModule],
  templateUrl: './message-banner.component.html',
  styleUrl: './message-banner.component.scss',
})
export class MessageBannerComponent {
  type = input<MessageBannerType>('info');
  icon = input<string>();

  resolvedIcon = computed(() => this.icon() ?? DEFAULT_ICONS[this.type()]);
}
