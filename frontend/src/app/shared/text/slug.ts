/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import slugifyText from '@sindresorhus/slugify';

export function slugify(text: string): string {
  return slugifyText(text.normalize('NFC'), { decamelize: false });
}
