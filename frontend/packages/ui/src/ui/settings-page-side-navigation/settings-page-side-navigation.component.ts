/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, input, signal, viewChild } from '@angular/core';
import { RouterLink, RouterLinkActive, type UrlTree } from '@angular/router';
import { BadgeComponent } from '../badge/badge.component';
import { IconComponent } from '../icon/icon.component';
import { scrollActiveLinkToMiddleAfterRenderAndResize } from '../scroll-active-link-to-middle';

export interface VerticalNavigationLink {
  label: string;
  icon: string;
  link: unknown[] | UrlTree;
  badge?: string;
  showSeparatorLineBeforeLink?: boolean;
}

// Links to settings pages at the same level within a project, task, cache, or team. Lists with one link offer no choice and render nothing.
@Component({
  selector: 'gr-settings-page-side-navigation',
  standalone: true,
  imports: [RouterLink, RouterLinkActive, BadgeComponent, IconComponent],
  template: `
    @if (links().length > 1) {
      <nav #sideNavigation class="side-navigation gr-scroll-fade-row" [attr.aria-label]="ariaLabel()">
        @for (item of links(); track item.label) {
          @if (item.showSeparatorLineBeforeLink) {
            <hr class="side-navigation-separator" />
          }
          <a #link class="side-navigation-link" [routerLink]="item.link" routerLinkActive="is-active" ariaCurrentWhenActive="page"
            (isActiveChange)="$event && activeLinkElement.set(link)">
            <gr-icon [name]="item.icon" size="sm" />
            <span class="side-navigation-label">{{ item.label }}</span>
            @if (item.badge) {
              <gr-badge severity="warning">{{ item.badge }}</gr-badge>
            }
          </a>
        }
      </nav>
    }
  `,
  styleUrl: './settings-page-side-navigation.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class SettingsPageSideNavigationComponent {
  links = input.required<VerticalNavigationLink[]>();
  ariaLabel = input.required<string>();

  protected activeLinkElement = signal<HTMLElement | null>(null);
  private sideNavigationElement = viewChild<ElementRef<HTMLElement>>('sideNavigation');

  constructor() {
    scrollActiveLinkToMiddleAfterRenderAndResize(this.activeLinkElement, () => this.sideNavigationElement()?.nativeElement, this.links);
  }
}
