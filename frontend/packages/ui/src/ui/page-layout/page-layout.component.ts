/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { RouterLink } from '@angular/router';
import { Component, input, ChangeDetectionStrategy } from '@angular/core';
import { CommonModule } from '@angular/common';
import { SkeletonComponent } from '../skeleton/skeleton.component';

// A `null` label is a name still loading; it shows as a placeholder.
export interface Crumb {
  label: string | null;
  link?: unknown[];
}

@Component({
  selector: 'gr-page-layout',
  standalone: true,
  imports: [RouterLink, CommonModule, SkeletonComponent],
  templateUrl: './page-layout.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './page-layout.component.scss',
})
export class PageLayoutComponent {
  breadcrumb = input<Crumb[]>([]);
  title = input.required<string>();
  subtitle = input<string>();
  maxWidth = input<string>('1200px');
}
