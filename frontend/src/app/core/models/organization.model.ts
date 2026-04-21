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
  public: boolean;
  managed: boolean;
  created_by?: string;
  created_at?: string;
  role?: 'Admin' | 'Write' | 'View';
  running_evaluations?: number;
  github_installation_id?: number | null;
  github_app_enabled?: boolean;
  github_app_available?: boolean;
}

export interface OrganizationMember {
  id: string;
  username: string;
  name: string;
  role: string;
}

export interface OrganizationSSH {
  public_key: string;
}
