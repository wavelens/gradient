/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { HttpContext, HttpContextToken } from '@angular/common/http';

export const SESSION_PROBE = new HttpContextToken(() => false);

export function asSessionProbe(): { context: HttpContext } {
  return { context: new HttpContext().set(SESSION_PROBE, true) };
}

/// Marks a read that fills one section of a page. The section shows its own error, so a busy or unreachable server doesn't replace the whole page with the error page.
export const SECTION_REQUEST = new HttpContextToken(() => false);

export function asSectionRequest(): { context: HttpContext } {
  return { context: new HttpContext().set(SECTION_REQUEST, true) };
}
