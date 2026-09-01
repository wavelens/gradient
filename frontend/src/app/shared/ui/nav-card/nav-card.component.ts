/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, booleanAttribute, input, ChangeDetectionStrategy } from '@angular/core';
import { RouterLink } from '@angular/router';
import { IconComponent } from '../icon/icon.component';

/// An index-page card that navigates: icon, title, description and a meta line.
/// The whole card is the link, so nothing inside it may be one.
@Component({
  selector: 'gr-nav-card',
  standalone: true,
  imports: [RouterLink, IconComponent],
  templateUrl: './nav-card.component.html',
  styleUrl: './nav-card.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class NavCardComponent {
  icon = input.required<string>();
  title = input.required<string>();
  description = input<string>();
  link = input<unknown[]>();
  muted = input(false, { transform: booleanAttribute });
}
