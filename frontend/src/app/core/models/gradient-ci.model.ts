/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ConnectionScope, ProjectWorkerEntry, WorkerConnection } from './api.generated';

export type GradientCiState = 'hidden' | 'connect' | 'connected';

export interface GradientCiEntry {
  state: GradientCiState;
  worker: ProjectWorkerEntry | null;
}

export const CONNECT_WAIT_MS = 30_000;

export function gradientCiEntry(offered: boolean, workers: ProjectWorkerEntry[]): GradientCiEntry {
  const connected = workers.find((w) => w.gradient_ci);
  if (connected) return { state: 'connected', worker: connected };
  return offered ? { state: 'connect', worker: null } : { state: 'hidden', worker: null };
}

export function listedWorkers(workers: ProjectWorkerEntry[], entry: GradientCiEntry): ProjectWorkerEntry[] {
  return workers.filter((w) => w !== entry.worker);
}

export function gradientCiConnectUrl(serviceUrl: string, scope: ConnectionScope, label: string): string {
  const url = new URL('/connect', serviceUrl);
  url.searchParams.set('scope', scope);
  url.searchParams.set('label', label);
  return url.toString();
}

export function gradientCiHost(serviceUrl: string): string {
  return new URL(serviceUrl).host;
}

export function gradientCiKeysUrl(serviceUrl: string): string {
  return new URL('/account/keys', serviceUrl).toString();
}

export function connectWaitState(
  status: WorkerConnection | undefined,
  elapsedMs: number,
): 'online' | 'waiting' | 'offline' {
  if (status?.connected) return 'online';
  return elapsedMs >= CONNECT_WAIT_MS ? 'offline' : 'waiting';
}
