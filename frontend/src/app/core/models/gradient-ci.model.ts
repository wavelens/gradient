/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { ConnectionFailure, Worker } from './worker.model';

export type GradientCiScope = 'base' | 'project';
export type GradientCiState = 'hidden' | 'connect' | 'enable' | 'connected';

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
  project?: string;
  token: string;
}

export interface GradientCiConnectResponse {
  worker_id: string;
}

export interface BaseWorkerEntry extends ConnectionStatus {
  worker_id: string;
  display_name: string;
  url?: string | null;
  enabled: boolean;
  auto_enable: boolean;
  gradient_ci: boolean;
}

export const CONNECT_WAIT_MS = 30_000;

export function gradientCiEntry(offered: boolean, workers: Worker[]): GradientCiEntry {
  const connections = workers.filter((w) => w.gradient_ci);
  const connected = connections.find((w) => !w.is_base || w.active);
  if (connected) return { state: 'connected', worker: connected };
  if (!offered) return { state: 'hidden', worker: null };
  const base = connections.find((w) => w.is_base);
  return base ? { state: 'enable', worker: base } : { state: 'connect', worker: null };
}

export function listedWorkers(workers: Worker[], entry: GradientCiEntry): Worker[] {
  return workers.filter((w) => w !== entry.worker && !(w.gradient_ci && w.is_base && !w.active));
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
