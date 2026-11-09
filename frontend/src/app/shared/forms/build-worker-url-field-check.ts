/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { FieldCheck } from './validators';

const WORKER_URL_PROTOCOLS = new Set(['wss:', 'ws:', 'https:', 'http:']);
const WORKER_URL_MESSAGE = 'Enter a complete URL with the wss://, ws://, https://, or http:// prefix.';

export const checkWorkerUrlWithWebSocketOrHttpScheme: FieldCheck = (value) => {
  const raw = value === null || value === undefined ? '' : String(value).trim();
  if (raw === '') return null;
  try {
    return WORKER_URL_PROTOCOLS.has(new URL(raw).protocol) ? null : WORKER_URL_MESSAGE;
  } catch {
    return WORKER_URL_MESSAGE;
  }
};
