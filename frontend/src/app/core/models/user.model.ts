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
}
