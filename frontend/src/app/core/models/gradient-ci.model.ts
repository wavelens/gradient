/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ConnectionFailure, Worker } from './worker.model';

export type GradientCiScope = 'team' | 'project';
export type GradientCiState = 'hidden' | 'connect' | 'connected';

export interface GradientCiEntry {
  state: GradientCiState;
  worker: Worker | null;
}

export interface ConnectionStatus {
  connected: boolean;
  last_error?: ConnectionFailure;
}

export interface GradientCiConnectRequest {
  scope: GradientCiScope;
  team?: string;
  project?: string;
  token: string;
}

export interface GradientCiConnectResponse {
  worker_id: string;
}

export const CONNECT_WAIT_MS = 30_000;

export function gradientCiEntry(offered: boolean, workers: Worker[]): GradientCiEntry {
  const connected = workers.find((w) => w.gradient_ci);
  if (connected) return { state: 'connected', worker: connected };
  return offered ? { state: 'connect', worker: null } : { state: 'hidden', worker: null };
}

export function listedWorkers(workers: Worker[], entry: GradientCiEntry): Worker[] {
  return workers.filter((w) => w !== entry.worker);
}

export function gradientCiConnectUrl(serviceUrl: string, scope: GradientCiScope, label: string): string {
  const url = new URL('/connect', serviceUrl);
  url.searchParams.set('scope', scope);
  url.searchParams.set('label', label);
  return url.toString();
}

export function gradientCiKeysUrl(serviceUrl: string): string {
  return new URL('/account/keys', serviceUrl).toString();
}

export function connectWaitState(
  status: ConnectionStatus | undefined,
  elapsedMs: number,
): 'online' | 'waiting' | 'offline' {
  if (status?.connected) return 'online';
  return elapsedMs >= CONNECT_WAIT_MS ? 'offline' : 'waiting';
}
