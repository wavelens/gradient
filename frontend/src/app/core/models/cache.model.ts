/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface Cache {
  id: string;
  name: string;
  display_name: string;
  description: string;
  active: boolean;
  priority: number;
  created_by?: string;
  created_at?: string;
}

export interface CacheKey {
  public_key: string;
}
