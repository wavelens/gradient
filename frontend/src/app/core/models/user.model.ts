/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface User {
  id: string;
  username: string;
  name: string;
  email: string;
  last_login_at?: string;
  created_at?: string;
  email_verified?: boolean;
  superuser?: boolean;
}

export interface UserSettings {
  username: string;
  name: string;
  email: string;
  is_oidc: boolean;
  managed: boolean;
}

export interface ApiKey {
  id: string;
  name: string;
  managed: boolean;
  permissions: string[];
  organization: string | null;
  cache: string | null;
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
  revoked_at: string | null;
  allowed_ips: string[];
}

export interface Session {
  id: string;
  user_agent: string | null;
  ip: string | null;
  created_at: string;
  last_used_at: string;
  expires_at: string;
  remember_me: boolean;
  current: boolean;
}

export interface AuditLogEntry {
  id: string;
  event: string;
  ip: string | null;
  user_agent: string | null;
  metadata: Record<string, unknown> | null;
  created_at: string;
}

export interface PaginatedResponse<T> {
  items: T;
  total: number;
  page: number;
  per_page: number;
}
