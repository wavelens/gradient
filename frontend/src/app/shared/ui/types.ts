/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface SelectOption {
  [key: string]: unknown;
}

export interface MenuItem {
  label?: string;
  icon?: string;
  disabled?: boolean;
  separator?: boolean;
  command?: () => void;
  routerLink?: string | unknown[];
  queryParams?: Record<string, unknown>;
}

export type MessageSeverity = 'success' | 'info' | 'warn' | 'error';

export interface Message {
  severity?: MessageSeverity;
  summary?: string;
  detail?: string;
  life?: number;
}
