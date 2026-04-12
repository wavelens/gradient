/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface Worker {
  worker_id: string;
  peer_id: string;
  managed: boolean;
  created_at?: string;
  /** Present when the worker is currently connected via proto. */
  connected?: boolean;
  architectures?: string[];
  system_features?: string[];
  max_concurrent_builds?: number;
  draining?: boolean;
}

export interface WorkerRegistration {
  peer_id: string;
  token: string;
}
