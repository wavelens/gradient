/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DownloadProgress } from '@core/models';
import { formatBytes } from '@shared/text';

export function downloadRatio(p: DownloadProgress): number | null {
  if (!p.total) return null;
  return Math.min(1, p.downloaded / p.total);
}

export function downloadLabel(p: DownloadProgress): string {
  const done = formatBytes(p.downloaded);
  return p.total ? `${done} / ${formatBytes(p.total)}` : done;
}
