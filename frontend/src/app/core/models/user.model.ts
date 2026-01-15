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
}

export interface UserSettings {
  id: string;
  username: string;
  name: string;
  email: string;
}

export interface ApiKey {
  id: string;
  name: string;
  key?: string;  // Only present when first created
  created_at: string;
}
