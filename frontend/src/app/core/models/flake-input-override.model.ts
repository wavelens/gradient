/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface FlakeInputOverride {
  id: string;
  project: string;
  input_name: string;
  url: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreateFlakeInputOverrideBody {
  input_name: string;
  url: string | null;
}

export interface UpdateFlakeInputOverrideBody {
  input_name?: string;
  url?: string | null;
}
