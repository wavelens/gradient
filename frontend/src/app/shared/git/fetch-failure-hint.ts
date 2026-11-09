/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

const HINTS: { pattern: RegExp; hint: string }[] = [
  {
    pattern: /publickey|permission denied|authentication failed|could not read username/i,
    hint: "The host can't accept Gradient's SSH key. Add the project SSH key as a deploy key on the repository.",
  },
  {
    pattern: /lookup address|could not resolve|name or service not known|no such host/i,
    hint: "Can't resolve the host name. Check the repository URL.",
  },
  {
    pattern: /failed to fill whole buffer|handshake/i,
    hint: 'The host closed the connection before Git started. Check the hostname and port in the repository URL.',
  },
  {
    pattern: /not found|does not exist|repository .* not exist/i,
    hint: 'Repository not found. Check the URL. Confirm the deploy key can access the repository.',
  },
];

export function fetchFailureHint(error: string | null | undefined): string | null {
  if (!error) return null;
  return HINTS.find(({ pattern }) => pattern.test(error))?.hint ?? null;
}
