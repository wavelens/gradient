/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, DestroyRef, inject, signal, effect, ChangeDetectionStrategy } from '@angular/core';
import { SEMANTIC_ROLES } from '../../../styles/tokens';
import { ThemeService } from '@core/services/theme.service';
import { TableComponent } from '@shared/ui';

const SPACING = ['xs', 'sm', 'md', 'lg', 'xl', 'xxl'];
const RADIUS = ['sm', 'md'];
const SIZES = ['xs', 'sm', 'md', 'lg', 'xl', 'xxl'];

@Component({
  selector: 'app-sg-foundations',
  standalone: true,
  imports: [TableComponent],
  templateUrl: './foundations.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './foundations.component.scss',
})
export class FoundationsComponent {
  private theme = inject(ThemeService);

  roles = SEMANTIC_ROLES;
  spacing = SPACING;
  radius = RADIUS;
  sizes = SIZES;
  resolved = signal<Record<string, string>>({});

  constructor() {
    effect(() => {
      this.theme.resolved();
      this.read();
    });

    // The swatches follow the theme through CSS whatever changes it; the printed
    // values must too, or this table quietly contradicts its own claim.
    const observer = new MutationObserver(() => this.read());
    observer.observe(document.documentElement, { attributeFilter: ['data-theme'] });
    inject(DestroyRef).onDestroy(() => observer.disconnect());
  }

  private read(): void {
    const style = getComputedStyle(document.documentElement);
    const map: Record<string, string> = {};
    for (const role of SEMANTIC_ROLES) map[role.name] = style.getPropertyValue(role.name).trim();
    this.resolved.set(map);
  }
}
