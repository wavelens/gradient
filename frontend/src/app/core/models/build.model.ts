/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface Build {
  id: string;
  evaluation: string;
  status: BuildStatus;
  derivation_path: string;
  architecture: Architecture;
  server?: string;
  created_at: string;
  updated_at: string;
}

export type BuildProgressPhase = 'prefetch' | 'download' | 'upload';

/** Transfers of a running build; a total is null when the worker knows no size or count. */
export interface BuildProgress {
  phase: BuildProgressPhase;
  bytes_done: number;
  bytes_total: number | null;
  paths_done: number;
  paths_total: number | null;
}

export type BuildStatus =
  | 'Created'
  | 'Queued'
  | 'Building'
  | 'Completed'
  | 'Substituted'
  | 'FailedPermanent'
  | 'FailedTransient'
  | 'FailedTimeout'
  | 'Aborted'
  | 'DependencyFailed'
  | 'Skipped';

export type Architecture = string;

export interface BuildDownload {
  filename: string;
  size: number;
  url: string;
}
