/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, input, ChangeDetectionStrategy } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { RouterLink } from '@angular/router';
import { IconComponent } from '../../ui/icon/icon.component';
import { FooterLink } from '../types';

@Component({
  selector: 'gr-footer',
  standalone: true,
  imports: [IconComponent, NgTemplateOutlet, RouterLink],
  templateUrl: './footer.component.html',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './footer.component.scss',
})
export class FooterComponent {
  links = input<FooterLink[]>([]);
  note = input<string | null>(null);

  protected external(link: FooterLink): boolean {
    return /^https?:\/\//.test(link.href);
  }
}
