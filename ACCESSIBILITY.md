<!--
SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
SPDX-License-Identifier: AGPL-3.0-only
-->

# Accessibility

Every page of the Gradient frontend should work with a keyboard and a screen reader, without relying on color or motion.

## In Place

- Light, dark and system theme.
- No animation of status icons, progress bars and the command palette under `prefers-reduced-motion`.
- Command palette on `/`, navigable with the arrow keys or `Ctrl+J` / `Ctrl+K`.
- Status icons carry a text label for screen readers.

## Reporting a Barrier

Barriers go into a [Bug report (frontend)](https://github.com/wavelens/gradient/issues/new?template=bug-report-frontend.yml), together with the assistive technology in use.

Accessibility issues are bugs and get the same priority as functional ones.
