/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive, ElementRef, afterRenderEffect, inject, input, output } from '@angular/core';

const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), select, textarea, [tabindex]:not([tabindex="-1"])';

/// Side panels slide over the page on narrow screens. Escape can close it anywhere, focus can move inside, and focus can return to the opener.
@Directive({
  selector: '[grDrawer]',
  standalone: true,
  host: { '(document:keydown.escape)': 'onEscape()' },
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
      if (open && !this.wasOpen) {
        this.opener = document.activeElement as HTMLElement | null;
        this.host.querySelector<HTMLElement>(FOCUSABLE)?.focus();
      }
      this.wasOpen = open;
    });
  }

  protected onEscape(): void {
    if (!this.open()) return;
    this.closed.emit();
    this.opener?.focus();
  }
}
