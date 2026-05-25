/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface CachePermissionDescriptor {
  id: string;
  mutating: boolean;
}

export interface CacheRole {
  id: string;
  name: string;
  cache: string | null;
  builtin: boolean;
  managed: boolean;
  permissions: string[];
}

export interface CacheMemberItem {
  id: string;
  name: string;
}

export interface CacheRoleListResponse {
  roles: CacheRole[];
  available_permissions: CachePermissionDescriptor[];
}
