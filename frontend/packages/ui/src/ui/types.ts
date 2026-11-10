/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { UrlTree } from '@angular/router';

export interface SelectOption {
  [key: string]: unknown;
}

export interface MenuItem {
  label?: string;
  icon?: string;
  disabled?: boolean;
  disabledReason?: string | null;
  danger?: boolean;
  separator?: boolean;
  command?: () => void;
  routerLink?: string | unknown[] | UrlTree;
  queryParams?: Record<string, unknown>;
  submenuItems?: readonly MenuItem[];
}

export type MessageSeverity = 'success' | 'info' | 'warn' | 'error';

export interface Message {
  severity?: MessageSeverity;
  summary?: string;
  detail?: string;
  life?: number;
  key?: string;
}

export type BadgeSeverity = 'neutral' | 'success' | 'danger' | 'warning' | 'info';
