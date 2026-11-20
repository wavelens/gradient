/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import type { GitHostConnectionResponse } from '@core/models';
import {
  GitHostConnectionFormContext,
  createGitHostConnectionRequest,
  emptyGitHostConnectionForm,
  gitHostBaseUrl,
  gitHostConnectionFormErrors,
  gitHostConnectionFormFromResponse,
  gitHostConnectionNameFromHost,
  hasNonDefaultAdvancedSettings,
  patchGitHostConnectionRequest,
} from './git-host-connection-form';

const creating: GitHostConnectionFormContext = { editing: null, existingNames: [] };

const existing: GitHostConnectionResponse = {
  id: 'c1',
  owner: { type: 'project', name: 'acme' },
  name: 'git-example',
  display_name: 'git-example',
  git_host_type: 'forgejo',
  base_url: 'https://git.example.com',
  receives_events: true,
  reports_status: true,
  can_manage: true,
  access_token_refreshes_automatically: false,
  refused_signature_count: 0,
  task_count: 2,
  managed: false,
  has_secret: true,
  has_access_token: true,
  allowed_ips: [],
  created_by: 'u1',
  created_at: '2026-01-01T00:00:00Z',
};

describe('form to connect to a git host', () => {
  it('shortens repository URL to host URL', () => {
    expect(gitHostBaseUrl('https://git.example.com/acme/app.git')).toBe('https://git.example.com');
    expect(gitHostBaseUrl('git@git.example.com:acme/app.git')).toBe('https://git.example.com');
    expect(gitHostBaseUrl('https://example.com/forgejo')).toBe('https://example.com/forgejo');
  });

  it('prefills type and host from the repository URL', () => {
    const form = emptyGitHostConnectionForm({ base_url: 'https://gitlab.com/acme/app' });
    expect(form.base_url).toBe('https://gitlab.com');
    expect(form.git_host_type).toBe('gitlab');
  });

  it('derives an unused name from the host', () => {
    expect(gitHostConnectionNameFromHost('https://git.example.com', [])).toBe('git-example');
    expect(gitHostConnectionNameFromHost('https://git.example.com', ['git-example'])).toBe('git-example-2');
  });

  it('requires a host URL and rejects a name already used by another connection', () => {
    const form = { ...emptyGitHostConnectionForm(), name: 'taken' };
    expect(gitHostConnectionFormErrors(form, { editing: null, existingNames: ['taken'] })).toEqual({
      base_url: 'Enter the host URL.',
      name: 'This owner already has a connection with this name.',
    });
  });

  it('needs a token when status reporting is enabled and denies connections that lack capabilities', () => {
    const form = { ...emptyGitHostConnectionForm({ base_url: 'https://git.example.com' }), reports_status: true };
    expect(gitHostConnectionFormErrors(form, creating)).toEqual({ access_token: 'Enter an access token to report status.' });
    expect(gitHostConnectionFormErrors({ ...form, reports_status: false, receives_events: false }, creating).capabilities).toBeDefined();
  });

  it('creates with the derived name and reports status exactly when the request has an access token', () => {
    const form = { ...emptyGitHostConnectionForm({ base_url: 'https://git.example.com/acme/app' }), access_token: ' tok ' };
    expect(createGitHostConnectionRequest(form, creating)).toEqual({
      name: 'git-example',
      git_host_type: 'forgejo',
      base_url: 'https://git.example.com',
      receives_events: true,
      reports_status: true,
      access_token: 'tok',
      allowed_ips: [],
    });
    expect(createGitHostConnectionRequest({ ...form, access_token: '' }, creating).reports_status).toBe(false);
  });

  it('patches only changed fields', () => {
    const form = { ...gitHostConnectionFormFromResponse(existing), display_name: 'Example', reports_status: false };
    const context = { editing: existing, existingNames: ['git-example'] };
    expect(gitHostConnectionFormErrors(form, context)).toEqual({});
    expect(patchGitHostConnectionRequest(form, existing, context)).toEqual({ display_name: 'Example', reports_status: false });
  });

  it('detects advanced settings only when one setting is not the default', () => {
    const context = { editing: existing, existingNames: [] };
    const saved = gitHostConnectionFormFromResponse(existing);
    expect(hasNonDefaultAdvancedSettings(emptyGitHostConnectionForm(), creating)).toBe(false);
    expect(hasNonDefaultAdvancedSettings(saved, context)).toBe(false);
    expect(hasNonDefaultAdvancedSettings({ ...saved, reports_status: false }, context)).toBe(true);
    expect(hasNonDefaultAdvancedSettings({ ...saved, display_name: 'Example' }, context)).toBe(true);
    expect(hasNonDefaultAdvancedSettings({ ...saved, receives_events: false }, context)).toBe(true);
    expect(hasNonDefaultAdvancedSettings({ ...saved, secret: 'whsec' }, context)).toBe(true);
    expect(hasNonDefaultAdvancedSettings({ ...saved, allowed_ips: '10.0.0.0/8' }, context)).toBe(true);
  });
});
