/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export type IntegrationKind = 'inbound' | 'outbound';
export type ForgeType = 'gitea' | 'forgejo' | 'gitlab' | 'github';
export type InboundForge = 'gitea' | 'forgejo' | 'gitlab';

export interface Integration {
  id: string;
  organization: string;
  name: string;
  display_name: string;
  kind: IntegrationKind;
  forge_type: ForgeType;
  endpoint_url: string | null;
  has_secret: boolean;
  has_access_token: boolean;
  allowed_ips: string[];
  created_by: string;
  created_at: string;
}

/** Credential-free integration handle returned by the org-member summary
 *  endpoint and inlined into reporter trigger responses. */
export interface IntegrationSummary {
  id: string;
  name: string;
  display_name: string;
  kind: IntegrationKind;
  forge_type: ForgeType;
}

export interface CreateIntegrationRequest {
  name: string;
  display_name?: string;
  kind: IntegrationKind;
  forge_type: ForgeType;
  secret?: string;
  endpoint_url?: string;
  access_token?: string;
  allowed_ips?: string[];
}

export interface PatchIntegrationRequest {
  name?: string;
  display_name?: string;
  forge_type?: ForgeType;
  secret?: string;
  endpoint_url?: string;
  access_token?: string;
  allowed_ips?: string[];
}
