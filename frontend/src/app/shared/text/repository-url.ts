/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const ACCEPTED_FORMS = 'ssh://git@host/path, git@host:path or https://host/path';
const INVALID = `Invalid repository URL; use ${ACCEPTED_FORMS}`;

// Mirrors `RepositoryUrl::from_str` in gradient-types so the form shows the server's message before saving.
export function repositoryUrlError(url: string): string | null {
  if (!url) return null;
  if (url.includes('file://') || url.startsWith('file')) return 'URLs pointing to local files are not allowed';

  const schemeEnd = url.indexOf('://');
  if (schemeEnd >= 0) return authorityError(url.slice(0, schemeEnd), url.slice(schemeEnd + 3));
  return url.includes('@') || url.includes(':') ? null : INVALID;
}

function authorityError(scheme: string, rest: string): string | null {
  const authority = rest.split('/')[0];
  const hostPort = authority.slice(authority.lastIndexOf('@') + 1);
  const split = splitHostPort(hostPort);
  if (!split?.host) return INVALID;
  if (split.port !== undefined && !isPortNumber(split.port)) {
    return `\`${scheme}://\` expects a port number after the host, not \`${split.port}\`; drop \`${scheme}://\` for the \`host:path\` form or use ${ACCEPTED_FORMS}`;
  }
  return null;
}

function splitHostPort(hostPort: string): { host: string; port?: string } | null {
  if (hostPort.startsWith('[')) {
    const close = hostPort.indexOf(']');
    if (close < 0) return null;
    const after = hostPort.slice(close + 1);
    return { host: hostPort.slice(1, close), port: after.startsWith(':') ? after.slice(1) : undefined };
  }
  const colon = hostPort.indexOf(':');
  return colon < 0 ? { host: hostPort } : { host: hostPort.slice(0, colon), port: hostPort.slice(colon + 1) };
}

function isPortNumber(port: string): boolean {
  return /^\d{1,5}$/.test(port) && Number(port) <= 65535;
}
