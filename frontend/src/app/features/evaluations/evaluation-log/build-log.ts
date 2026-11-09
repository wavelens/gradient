/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { LogSearchHit } from '@core/models';
import { ApiError } from '@core/services/api.service';
import { type ServerLog, splitLogLines } from '@shared/log';
import { environment } from '@environments/environment';

async function serverReason(res: Response): Promise<string | null> {
  try {
    const message = (await res.json())?.message;
    return typeof message === 'string' && message ? message : null;
  } catch {
    return null;
  }
}

export async function fetchBuildLogResponseWithCookiesOrThrow(path: string): Promise<Response> {
  let res: Response;
  try {
    res = await fetch(`${environment.apiUrl}/${path}`, { method: 'GET', credentials: 'include' });
  } catch {
    throw new ApiError('Failed to connect to the server. Check the connection and try again.', 0);
  }
  if (!res.ok) throw new ApiError((await serverReason(res)) ?? 'The server failed to send the log. Try again later.', res.status);
  return res;
}

async function fetchLines(buildId: string, start: number, end: number): Promise<string[]> {
  const res = await fetchBuildLogResponseWithCookiesOrThrow(`builds/${buildId}/log/lines?start=${start}&end=${end}`);
  return splitLogLines(await res.text());
}

async function searchLog(
  buildId: string,
  query: string,
  onHits: (hits: LogSearchHit[]) => void,
  isStale: () => boolean,
): Promise<number> {
  const res = await fetch(
    `${environment.apiUrl}/builds/${buildId}/log/search?q=${encodeURIComponent(query)}`,
    { method: 'GET', credentials: 'include' },
  );
  if (!res.ok || !res.body) return 0;
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  const hits: LogSearchHit[] = [];
  let buffered = '';
  let total = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    if (isStale()) {
      await reader.cancel().catch(() => undefined);
      return total;
    }
    buffered += decoder.decode(value, { stream: true });
    const parts = buffered.split('\n');
    buffered = parts.pop() ?? '';
    for (const part of parts) {
      if (!part.trim()) continue;
      const obj = JSON.parse(part);
      if (obj.done === true) {
        total = obj.total_matches ?? hits.length;
      } else {
        hits.push(obj as LogSearchHit);
        onHits([...hits]);
      }
    }
  }
  return total;
}

export function buildLog(buildId: string, total: number): ServerLog {
  return {
    total,
    lines: (start, end) => fetchLines(buildId, start, end),
    search: (query, onHits, isStale) => searchLog(buildId, query, onHits, isStale),
  };
}
