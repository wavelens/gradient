/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export type FieldCheck = (value: unknown) => string | null;

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const BASE64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;
const LOCAL_REPOSITORY = /^(?:file:|\/|\.{1,2}\/|~)/i;
const REMOTE_FLAKE_URL_PREFIXES = [
  'github:', 'gitlab:', 'sourcehut:', 'git+ssh://', 'git+https://', 'git+http://', 'git://', 'https://', 'http://', 'flake:', 'path:/nix/store/',
];

const text = (value: unknown): string => (value === null || value === undefined ? '' : String(value).trim());

/// Each check can return a message to show under the field. A null return means the value has no problem. Empty values pass through, so `required` can stay a separate decision.
export const fieldChecks = {
  positiveInteger: ((value) => {
    const raw = text(value);
    return raw === '' || (/^\d+$/.test(raw) && Number(raw) >= 1) ? null : 'Enter an integer of 1 or higher.';
  }) as FieldCheck,
  uuid: ((value) => {
    const raw = text(value);
    return raw === '' || UUID.test(raw) ? null : 'Enter a UUID such as 123e4567-e89b-12d3-a456-426614174000.';
  }) as FieldCheck,
  base64: ((value) => {
    const raw = text(value);
    return raw === '' || BASE64.test(raw) ? null : 'Enter a base64 value (letters, digits, +, /, and an optional trailing =).';
  }) as FieldCheck,
  httpUrl: ((value) => {
    const raw = text(value);
    if (raw === '') return null;
    try {
      const url = new URL(raw);
      return url.protocol === 'https:' || url.protocol === 'http:' ? null : 'Enter a complete URL with the https:// or http:// prefix.';
    } catch {
      return 'Enter a complete URL with the https:// or http:// prefix.';
    }
  }) as FieldCheck,
  remoteRepository: ((value) => {
    const raw = text(value);
    return raw === '' || !LOCAL_REPOSITORY.test(raw)
      ? null
      : "Enter the URL of a remote repository. The server can't fetch local paths and file:// URLs.";
  }) as FieldCheck,
  remoteFetchableFlakeUrl: ((value) => {
    const raw = text(value);
    return raw === '' || (!/\s/.test(raw) && REMOTE_FLAKE_URL_PREFIXES.some((prefix) => raw.startsWith(prefix)))
      ? null
      : `Start the flake URL with one of ${REMOTE_FLAKE_URL_PREFIXES.join(', ')}. Don't include spaces.`;
  }) as FieldCheck,
};

export function firstFieldError(value: unknown, ...checks: FieldCheck[]): string | null {
  for (const check of checks) {
    const message = check(value);
    if (message) return message;
  }
  return null;
}
