/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface GradientCapabilities {
  core: boolean;
  federate: boolean;
  fetch: boolean;
  eval: boolean;
  build: boolean;
  cache: boolean;
}

export interface WorkerLiveInfo {
  capabilities: GradientCapabilities;
  architectures: string[];
  system_features: string[];
  max_concurrent_builds: number;
  assigned_job_count: number;
  draining: boolean;
}

export interface ConnectionFailure {
  reason: string;
  at: string;
  direction: 'outbound' | 'inbound';
  before_auth: boolean;
}

export interface AllowedCapabilities {
  enable_fetch: boolean;
  enable_eval: boolean;
  enable_build: boolean;
}

export interface WorkerPatch extends Partial<AllowedCapabilities> {
  active?: boolean;
  display_name?: string;
}

export function changedCapabilities(
  before: AllowedCapabilities,
  after: AllowedCapabilities,
): Partial<AllowedCapabilities> {
  const keys: (keyof AllowedCapabilities)[] = ['enable_fetch', 'enable_eval', 'enable_build'];
  return Object.fromEntries(keys.filter((k) => before[k] !== after[k]).map((k) => [k, after[k]]));
}

export interface Worker extends AllowedCapabilities {
  worker_id: string;
  /** Human-readable display name. */
  display_name: string;
  managed: boolean;
  active: boolean;
  team?: string;
  gradient_ci: boolean;
  connected: boolean;
  last_error?: ConnectionFailure;
  registered_at?: string;
  /** WebSocket URL where the worker accepts incoming server connections. */
  url?: string;
  /** User who registered this worker. Null for legacy or declarative rows. */
  created_by?: string | null;
  /** Present when the worker is currently connected via proto. */
  live?: WorkerLiveInfo;
}

export interface WorkerRegistration {
  peer_id: string;
  /** Absent when the token was pre-supplied in the registration request. */
  token?: string;
}

export interface WorkerTestResponse {
  ok: boolean;
  connected: boolean;
  authorized_for_project: boolean;
  message: string;
}
