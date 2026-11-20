/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { GitHostType } from '@core/models';
import { repositoryWebUrl, splitRepository } from '@shared/evaluation';

const SCP_HOST = /^[^@/\s]+@([^:/\s]+):/;

const GIT_HOST_LABELS: Record<GitHostType, string> = {
  forgejo: 'Forgejo',
  gitea: 'Gitea',
  gitlab: 'GitLab',
  github: 'GitHub',
};

export const SELF_HOSTED_GIT_HOSTS: readonly { label: string; value: GitHostType }[] = (['forgejo', 'gitea', 'gitlab'] as const)
  .map((value) => ({ label: GIT_HOST_LABELS[value], value }));

export function gitHostLabel(gitHost: string): string {
  return GIT_HOST_LABELS[gitHost as GitHostType] ?? gitHost;
}

export function repositoryHost(repository: string): string | null {
  const { cloneUrl } = splitRepository(repository.trim());
  const scp = SCP_HOST.exec(cloneUrl);
  if (scp) return scp[1].toLowerCase();
  try {
    const host = new URL(cloneUrl.replace(/^git\+/, '')).hostname;
    return host ? host.toLowerCase() : null;
  } catch {
    return null;
  }
}

export function repositoryName(url: string): string {
  const path = url.trim().replace(/\?.*$/, '').replace(/\/+$/, '').replace(/\.git$/, '');
  return path.split(/[/:]/).pop() ?? '';
}

export function knownGitHostType(url: string): GitHostType | null {
  const host = repositoryHost(url) ?? '';
  if (host === 'gitlab.com' || host.includes('gitlab')) return 'gitlab';
  if (host === 'codeberg.org' || host.includes('forgejo')) return 'forgejo';
  if (host === 'gitea.com' || host.includes('gitea')) return 'gitea';
  return null;
}

export function webhookEvents(gitHost: string): string {
  return gitHost === 'gitlab'
    ? 'Push, Tag push, Merge request, Comments (note) and Releases events'
    : 'Push, Pull Request, Issue Comment, Pull Request Comment, Pull Request Review and Release';
}

export interface GitHostSettingsLinks {
  deployKeys: string;
  webhooks: string;
}

export function gitHostSettingsLinks(repository: string, gitHost: string | null): GitHostSettingsLinks | null {
  const web = repositoryWebUrl(repository);
  if (!web) return null;
  if (gitHost === 'gitlab') {
    return { deployKeys: `${web}/-/settings/repository`, webhooks: `${web}/-/hooks` };
  }
  return { deployKeys: `${web}/settings/keys`, webhooks: `${web}/settings/hooks` };
}
