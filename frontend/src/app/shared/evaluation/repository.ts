/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const SCP_ADDRESS = /^[^@/\s]+@([^:/\s]+):(.+)$/;

function hostAndPath(repository: string): { host: string; path: string } | null {
  const scp = SCP_ADDRESS.exec(repository);
  if (scp) return { host: scp[1], path: scp[2] };

  try {
    const url = new URL(repository);
    if (!/^(https?|ssh|git\+ssh):$/.test(url.protocol) || !url.hostname) return null;
    return { host: url.protocol === 'http:' ? url.host : url.hostname, path: url.pathname };
  } catch {
    return null;
  }
}

export function repositoryWebUrl(repository: string): string | null {
  const parts = hostAndPath(repository);
  if (!parts) return null;
  const scheme = repository.startsWith('http://') ? 'http' : 'https';
  const path = parts.path.replace(/^\/+/, '').replace(/\/+$/, '').replace(/\.git$/, '');
  return `${scheme}://${parts.host}/${path}`;
}

export function commitWebUrl(repository: string, commit: string | null | undefined): string | null {
  const web = commit ? repositoryWebUrl(repository) : null;
  return web && `${web}/commit/${commit}`;
}
