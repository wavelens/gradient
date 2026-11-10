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

/// Tag requests that show their result in one part of the page, such as a page section or a single form field. These sections show their own errors. A busy or unreachable server doesn't replace the whole page with an error page or show a separate busy toast.
export const SECTION_REQUEST = new HttpContextToken(() => false);

export function asSectionRequest(): { context: HttpContext } {
  return { context: new HttpContext().set(SECTION_REQUEST, true) };
}
