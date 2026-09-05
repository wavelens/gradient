/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface Invite {
  kind: 'project' | 'cache';
  token: string;
  scope: string;
  scope_display_name: string;
  role: string;
  invited_by: string;
  created_at: string;
  expires_at: string;
}

export interface PendingInvitation {
  user: string;
  name: string;
  role: string;
  created_at: string;
  expires_at: string;
}

export interface SubscriptionRequest {
  project: string;
  project_display_name: string;
  mode: number;
  requested_by: string;
  created_at: string;
}

export interface CacheSubscription {
  id: string;
  name: string;
  mode: number;
  status: 'active' | 'pending';
}
