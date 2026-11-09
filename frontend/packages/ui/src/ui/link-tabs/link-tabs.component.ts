/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, afterRenderEffect, input, signal } from '@angular/core';
import { RouterLink, RouterLinkActive, type UrlTree } from '@angular/router';

export interface LinkTab {
  label: string;
  link: unknown[] | UrlTree;
}

// Sibling pages appear as tabs. Each tab is a link. Browser history and open in a new tab behave as usual.
@Component({
  selector: 'gr-link-tabs',
  standalone: true,
  imports: [RouterLink, RouterLinkActive],
  template: `
    <nav class="link-tabs gr-scroll-fade-row" [attr.aria-label]="ariaLabel()">
      @for (tab of tabs(); track tab.label) {
        <a #link class="link-tab" [routerLink]="tab.link" routerLinkActive="is-active" ariaCurrentWhenActive="page"
          (isActiveChange)="$event && activeLink.set(link)">{{ tab.label }}</a>
      }
    </nav>
  `,
  styleUrl: './link-tabs.component.scss',
  changeDetection: ChangeDetectionStrategy.Eager,
})
export class LinkTabsComponent {
  tabs = input.required<LinkTab[]>();
  ariaLabel = input.required<string>();

  protected activeLink = signal<HTMLElement | null>(null);

  constructor() {
    afterRenderEffect(() => {
      this.tabs();
      this.activeLink()?.scrollIntoView?.({ block: 'nearest', inline: 'center' });
    });
  }
}
