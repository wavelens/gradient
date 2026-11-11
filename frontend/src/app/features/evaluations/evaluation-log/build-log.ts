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

export async function fetchBuildLogResponseWithCookiesOrThrow(
  path: string,
  { method = 'GET', signal }: Pick<RequestInit, 'method' | 'signal'> = {},
): Promise<Response> {
  let res: Response;
  try {
    res = await fetch(`${environment.apiUrl}/${path}`, { method, signal, credentials: 'include' });
  } catch {
    throw new ApiError('Failed to connect to the server. Check the connection and try again.', 0);
  }
  if (!res.ok) throw new ApiError((await serverReason(res)) ?? 'The server failed to send the log. Try again later.', res.status);
  return res;
}

export function buildBuildLogApiPath(
  buildId: string,
  attempt: number | null,
  endpoint = '',
  query: Record<string, string | number> = {},
): string {
  const params = new URLSearchParams(Object.entries(query).map(([name, value]) => [name, String(value)]));
  if (attempt !== null) params.set('attempt', String(attempt));
  const search = params.toString();
  return `builds/${buildId}/log${endpoint}${search ? `?${search}` : ''}`;
}

async function fetchLines(buildId: string, attempt: number | null, start: number, end: number): Promise<string[]> {
  const res = await fetchBuildLogResponseWithCookiesOrThrow(buildBuildLogApiPath(buildId, attempt, '/lines', { start, end }));
  return splitLogLines(await res.text());
}

async function searchLog(
  buildId: string,
  attempt: number | null,
  query: string,
  onHits: (hits: LogSearchHit[]) => void,
  isStale: () => boolean,
): Promise<number> {
  const res = await fetch(
    `${environment.apiUrl}/${buildBuildLogApiPath(buildId, attempt, '/search', { q: query })}`,
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

export class BuildLogStreamDecoder {
  private unfinishedJsonLine = '';
  private unfinishedLogLine = '';

  appendTextAndReturnCompletedLines(text: string): string[] {
    const jsonLines = (this.unfinishedJsonLine + text).split('\n');
    this.unfinishedJsonLine = jsonLines.pop() ?? '';
    const logText = this.unfinishedLogLine + jsonLines
      .filter((line) => line.trim() !== '')
      .map((line) => JSON.parse(line) as string)
      .join('');
    const lastLineEnd = logText.lastIndexOf('\n');
    this.unfinishedLogLine = logText.slice(lastLineEnd + 1);
    return splitLogLines(logText.slice(0, lastLineEnd + 1));
  }

  takeLastLogLineWithoutNewline(): string[] {
    const lines = this.appendTextAndReturnCompletedLines('\n');
    const last = this.unfinishedLogLine;
    this.unfinishedLogLine = '';
    return last ? [...lines, ...splitLogLines(last)] : lines;
  }
}

export function buildLog(buildId: string, total: number, attempt: number | null = null): ServerLog {
  return {
    total,
    lines: (start, end) => fetchLines(buildId, attempt, start, end),
    search: (query, onHits, isStale) => searchLog(buildId, attempt, query, onHits, isStale),
  };
}
