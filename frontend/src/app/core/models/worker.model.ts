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
  sign: boolean;
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

export interface Worker {
  worker_id: string;
  /** Human-readable display name. */
  name: string;
  managed: boolean;
  active: boolean;
  registered_at?: string;
  /** WebSocket URL where the worker accepts incoming server connections. */
  url?: string;
  /** Present when the worker is currently connected via proto. */
  live?: WorkerLiveInfo;
}

export interface WorkerRegistration {
  peer_id: string;
  /** Absent when the token was pre-supplied in the registration request. */
  token?: string;
}
