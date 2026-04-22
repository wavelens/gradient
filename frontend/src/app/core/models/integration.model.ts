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
  created_by: string;
  created_at: string;
}

export interface CreateIntegrationRequest {
  name: string;
  display_name?: string;
  kind: IntegrationKind;
  forge_type: ForgeType;
  secret?: string;
  endpoint_url?: string;
  access_token?: string;
}

export interface PatchIntegrationRequest {
  name?: string;
  display_name?: string;
  forge_type?: ForgeType;
  secret?: string;
  endpoint_url?: string;
  access_token?: string;
}

export interface ProjectIntegrationLink {
  project: string;
  inbound_integration: string | null;
  outbound_integration: string | null;
}

export interface SetProjectIntegrationRequest {
  inbound_integration?: string | null;
  outbound_integration?: string | null;
}
