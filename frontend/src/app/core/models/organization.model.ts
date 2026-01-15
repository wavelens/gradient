/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface Organization {
  id: string;
  name: string;
  display_name: string;
  description: string;
  public_key?: string;
  use_nix_store: boolean;
  created_by?: string;
  created_at?: string;
  role?: 'owner' | 'member';
}

export interface OrganizationMember {
  id: string;
  username: string;
  name: string;
  role: 'owner' | 'member';
}

export interface OrganizationSSH {
  public_key: string;
}
