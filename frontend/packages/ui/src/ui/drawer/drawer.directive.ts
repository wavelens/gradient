/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive, ElementRef, afterRenderEffect, inject, input, output } from '@angular/core';

const keyboardTabFocusableElementSelector =
  'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/// Side panels slide over the page on narrow screens. Escape can close it anywhere, focus can move inside, and focus can return to the opener.
@Directive({
  selector: '[grDrawer]',
  standalone: true,
  host: {
    '(document:keydown.escape)': 'onEscape()',
    '(keydown)': 'wrapFocusInsideDrawerOnTab($event)',
  },
})
export class DrawerDirective {
  readonly open = input.required<boolean>({ alias: 'grDrawer' });
  readonly closed = output<void>({ alias: 'grDrawerClose' });

  private readonly host = inject<ElementRef<HTMLElement>>(ElementRef).nativeElement;
  private opener: HTMLElement | null = null;
  private wasOpen = false;

  constructor() {
    afterRenderEffect(() => {
      const open = this.open();
      if (open === this.wasOpen) return;
      this.wasOpen = open;
      if (open) {
        this.opener = document.activeElement as HTMLElement | null;
        this.getFocusableElementsInDrawer()[0]?.focus();
      } else {
        this.returnFocusToOpener();
      }
    });
  }

  protected onEscape(): void {
    if (this.open()) this.closed.emit();
  }

  protected wrapFocusInsideDrawerOnTab(event: KeyboardEvent): void {
    if (!this.open() || event.key !== 'Tab') return;
    const focusable = this.getFocusableElementsInDrawer();
    if (!focusable.length) return;
    const edge = event.shiftKey ? focusable[0] : focusable[focusable.length - 1];
    if (document.activeElement !== edge) return;
    event.preventDefault();
    (event.shiftKey ? focusable[focusable.length - 1] : focusable[0]).focus();
  }

  private getFocusableElementsInDrawer(): HTMLElement[] {
    return Array.from(this.host.querySelectorAll<HTMLElement>(keyboardTabFocusableElementSelector)).filter(
      (element) => !element.closest('[hidden], [inert]'),
    );
  }

  private returnFocusToOpener(): void {
    const focused = document.activeElement;
    if (!focused || focused === document.body || this.host.contains(focused)) this.opener?.focus();
    this.opener = null;
  }
}
