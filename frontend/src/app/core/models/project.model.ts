/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface Project {
  id: string;
  name: string;
  display_name: string;
  description: string;
  public_key?: string;
  public: boolean;
  hide_build_requests: boolean;
  managed: boolean;
  created_by?: string;
  created_at?: string;
  role?: 'Admin' | 'Write' | 'View';
  running_evaluations?: number;
  github_app_available?: boolean;
}

export interface ProjectMember {
  id: string;
  username: string;
  name: string;
  role: string;
}

export interface ProjectSSH {
  public_key: string;
}
