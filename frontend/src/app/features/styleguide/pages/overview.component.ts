/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ChangeDetectionStrategy } from '@angular/core';

@Component({
  selector: 'app-sg-overview',
  standalone: true,
  changeDetection: ChangeDetectionStrategy.Eager,
  template: `
    <h1>Gradient Design System</h1>
    <p>
      Every page composes primitives from <code>&#64;shared/ui</code>. This guide is the only place
      those primitives are demonstrated, and nothing here declares markup a primitive already covers.
    </p>
    <h2>Rules</h2>
    <ol>
      <li>Any store path, hash, key, ID or URL is a <code>gr-copy-field</code>, never a bare code span.</li>
      <li>Colour comes from semantic roles. No hex outside the palette, and no component reads a palette token directly.</li>
      <li>New shared classes go in the design system, never in a component stylesheet.</li>
      <li>Content shapes are <code>gr-row-list</code> or <code>gr-card-grid</code>, never named per entity.</li>
      <li>Every element stays legible in both themes. Nothing hard-codes black or white.</li>
      <li>Text sits at most one step from body: 16px interactive, 14px secondary, 12px badges only.</li>
    </ol>
  `,
})
export class OverviewComponent {}
