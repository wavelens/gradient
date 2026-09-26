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

/** Bytes a running Substitute or Download has fetched; `total` is null when the source announced no size. */
export interface DownloadProgress {
  downloaded: number;
  total: number | null;
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
