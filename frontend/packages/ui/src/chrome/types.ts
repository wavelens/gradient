/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

export interface ChromeLink {
  label: string;
  href: string;
}

export interface Brand extends ChromeLink {
  tagline?: string;
}

export interface NavLink extends ChromeLink {
  exact?: boolean;
}

export interface FooterLink extends ChromeLink {
  icon?: string;
  prefix?: string;
}
