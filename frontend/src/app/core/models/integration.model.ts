/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export type IntegrationKind = 'inbound' | 'outbound';
export type GitHostType = 'gitea' | 'forgejo' | 'gitlab' | 'github';
export type InboundGitHost = 'gitea' | 'forgejo' | 'gitlab';

export interface Integration {
  id: string;
  project: string;
  name: string;
  display_name: string;
  kind: IntegrationKind;
  git_host_type: GitHostType;
  endpoint_url: string | null;
  has_secret: boolean;
  has_access_token: boolean;
  allowed_ips: string[];
  created_by: string;
  created_at: string;
  installation_id?: number | null;
  account_login?: string | null;
}

/** Credential-free integration handle returned by the project-member summary
 *  endpoint and inlined into reporter trigger responses. */
export interface IntegrationSummary {
  id: string;
  name: string;
  display_name: string;
  kind: IntegrationKind;
  git_host_type: GitHostType;
}

export interface CreateIntegrationRequest {
  name: string;
  display_name?: string;
  kind: IntegrationKind;
  git_host_type: GitHostType;
  secret?: string;
  endpoint_url?: string;
  access_token?: string;
  allowed_ips?: string[];
  installation_id?: number;
}

export interface PatchIntegrationRequest {
  name?: string;
  display_name?: string;
  git_host_type?: GitHostType;
  secret?: string;
  endpoint_url?: string;
  access_token?: string;
  allowed_ips?: string[];
}
