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

export type BuildStatus =
  | 'Created'
  | 'Queued'
  | 'Building'
  | 'Completed'
  | 'Failed'
  | 'Aborted'
  | 'DependencyFailed';

export type Architecture = string;

export interface BuildDownload {
  filename: string;
  size: number;
  url: string;
}
