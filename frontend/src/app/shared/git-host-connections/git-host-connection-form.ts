/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { CreateGitHostConnectionRequest, GitHostConnectionResponse, GitHostType, PatchGitHostConnectionRequest } from '@core/models';
import { repositoryHost } from '@shared/git/git-host';
import { fieldChecks, isIpNetwork, splitLines } from '@shared/forms';
import { slugify } from '@shared/text';

export interface GitHostConnectionFormValue {
  git_host_type: GitHostType;
  base_url: string;
  access_token: string;
  name: string | null;
  display_name: string;
  receives_events: boolean;
  reports_status: boolean | null;
  secret: string;
  allowed_ips: string;
}

export interface GitHostConnectionFormContext {
  editing: GitHostConnectionResponse | null;
  existingNames: readonly string[];
}

export type GitHostConnectionFormErrors = Partial<
  Record<'base_url' | 'access_token' | 'name' | 'capabilities' | 'secret' | 'allowed_ips', string>
>;

const NAME_PATTERN = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/;

export function emptyGitHostConnectionForm(prefill: { git_host_type?: string | null; base_url?: string | null } = {}): GitHostConnectionFormValue {
  const base_url = prefill.base_url ? gitHostBaseUrl(prefill.base_url) : '';
  return {
    git_host_type: (prefill.git_host_type as GitHostType | null | undefined) ?? knownGitHostType(base_url) ?? 'forgejo',
    base_url,
    access_token: '',
    name: null,
    display_name: '',
    receives_events: true,
    reports_status: null,
    secret: '',
    allowed_ips: '',
  };
}

export function gitHostConnectionFormFromResponse(connection: GitHostConnectionResponse): GitHostConnectionFormValue {
  return {
    git_host_type: connection.git_host_type as GitHostType,
    base_url: connection.base_url ?? '',
    access_token: '',
    name: connection.name,
    display_name: connection.display_name === connection.name ? '' : connection.display_name,
    receives_events: connection.receives_events,
    reports_status: connection.reports_status,
    secret: '',
    allowed_ips: connection.allowed_ips.join('\n'),
  };
}

export function knownGitHostType(url: string): GitHostType | null {
  const host = repositoryHost(url) ?? '';
  if (host === 'gitlab.com' || host.includes('gitlab')) return 'gitlab';
  if (host === 'codeberg.org' || host.includes('forgejo')) return 'forgejo';
  if (host === 'gitea.com' || host.includes('gitea')) return 'gitea';
  return null;
}

export function gitHostBaseUrl(text: string): string {
  const value = text.trim();
  if (!value) return '';
  const host = repositoryHost(value);
  if (!host) return value;
  try {
    const url = new URL(value);
    const segments = url.pathname.split('/').filter(Boolean);
    if ((url.protocol === 'https:' || url.protocol === 'http:') && segments.length < 2) {
      return `${url.origin}${segments.length ? '/' + segments[0] : ''}`;
    }
    const port = url.protocol === 'https:' || url.protocol === 'http:' ? url.port : '';
    return `${url.protocol === 'http:' ? 'http' : 'https'}://${host}${port ? ':' + port : ''}`;
  } catch {
    return `https://${host}`;
  }
}

export function gitHostConnectionNameFromHost(baseUrl: string, existingNames: readonly string[]): string {
  const host = repositoryHost(baseUrl);
  if (!host) return '';
  const labels = host.split('.');
  const base = slugify((labels.length > 1 ? labels.slice(0, -1) : labels).join('-')) || 'git-host';
  let name = base;
  for (let suffix = 2; existingNames.includes(name); suffix++) name = `${base}-${suffix}`;
  return name;
}

export function effectiveConnectionName(form: GitHostConnectionFormValue, context: GitHostConnectionFormContext): string {
  return form.name ?? gitHostConnectionNameFromHost(form.base_url, context.existingNames);
}

export function effectiveReportsStatus(form: GitHostConnectionFormValue, context: GitHostConnectionFormContext): boolean {
  return form.reports_status ?? (!!form.access_token.trim() || !!context.editing?.has_access_token);
}

export function gitHostConnectionFormErrors(form: GitHostConnectionFormValue, context: GitHostConnectionFormContext): GitHostConnectionFormErrors {
  const errors: GitHostConnectionFormErrors = {};
  const baseUrl = form.base_url.trim();
  if (!baseUrl) errors.base_url = 'Enter the host URL.';
  else {
    const urlError = fieldChecks.httpUrl(gitHostBaseUrl(baseUrl));
    if (urlError) errors.base_url = urlError;
  }

  const reportsStatus = effectiveReportsStatus(form, context);
  if (reportsStatus && !form.access_token.trim() && !context.editing?.has_access_token) {
    errors.access_token = 'Enter an access token to report status.';
  }

  const name = effectiveConnectionName(form, context).trim();
  if (!name) errors.name = 'Enter a name.';
  else if (!NAME_PATTERN.test(name)) errors.name = "Use lowercase letters, digits and dashes. Don't start or end with a dash.";
  else if (context.existingNames.some((existing) => existing === name && existing !== context.editing?.name)) {
    errors.name = 'This owner already has a connection with this name.';
  }

  if (!form.receives_events && !reportsStatus) errors.capabilities = 'Turn on receiving events or reporting status.';

  const editing = context.editing;
  if (editing && form.receives_events && !editing.has_secret && !form.secret.trim()) {
    errors.secret = 'Enter or generate a webhook secret to receive events.';
  }

  const invalidIps = splitLines(form.allowed_ips).filter((entry) => !isIpNetwork(entry));
  if (invalidIps.length > 0) errors.allowed_ips = 'Not an IP address or CIDR range: ' + invalidIps.join(', ');
  return errors;
}

export function createGitHostConnectionRequest(form: GitHostConnectionFormValue, context: GitHostConnectionFormContext): CreateGitHostConnectionRequest {
  const body: CreateGitHostConnectionRequest = {
    name: effectiveConnectionName(form, context).trim(),
    git_host_type: form.git_host_type,
    base_url: gitHostBaseUrl(form.base_url),
    receives_events: form.receives_events,
    reports_status: effectiveReportsStatus(form, context),
  };
  if (form.display_name.trim()) body.display_name = form.display_name.trim();
  if (form.access_token.trim()) body.access_token = form.access_token.trim();
  if (form.receives_events && form.secret.trim()) body.secret = form.secret.trim();
  if (form.receives_events) body.allowed_ips = splitLines(form.allowed_ips);
  return body;
}

export function patchGitHostConnectionRequest(
  form: GitHostConnectionFormValue,
  target: GitHostConnectionResponse,
  context: GitHostConnectionFormContext,
): PatchGitHostConnectionRequest {
  const body: PatchGitHostConnectionRequest = {};
  const name = effectiveConnectionName(form, context).trim();
  if (name !== target.name) body.name = name;
  const displayName = form.display_name.trim() || name;
  if (displayName !== target.display_name) body.display_name = displayName;
  if (form.git_host_type !== target.git_host_type) body.git_host_type = form.git_host_type;
  const baseUrl = gitHostBaseUrl(form.base_url);
  if (baseUrl !== (target.base_url ?? '')) body.base_url = baseUrl;
  if (form.access_token.trim()) body.access_token = form.access_token.trim();
  if (form.secret.trim()) body.secret = form.secret.trim();
  const allowed = splitLines(form.allowed_ips);
  if (allowed.join('\n') !== target.allowed_ips.join('\n')) body.allowed_ips = allowed;
  if (form.receives_events !== target.receives_events) body.receives_events = form.receives_events;
  const reportsStatus = effectiveReportsStatus(form, context);
  if (reportsStatus !== target.reports_status) body.reports_status = reportsStatus;
  return body;
}
