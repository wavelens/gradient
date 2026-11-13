/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, ElementRef, input, signal, viewChild } from '@angular/core';
import { RouterLink, RouterLinkActive, type UrlTree } from '@angular/router';
import { scrollActiveLinkToMiddleAfterRenderAndResize } from '../scroll-active-link-to-middle';

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
    <nav #tabLinksNav class="link-tabs gr-scroll-fade-row" [attr.aria-label]="ariaLabel()">
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
  private tabLinksNavElement = viewChild.required<ElementRef<HTMLElement>>('tabLinksNav');

  constructor() {
    scrollActiveLinkToMiddleAfterRenderAndResize(this.activeLink, () => this.tabLinksNavElement().nativeElement, this.tabs);
  }
}
