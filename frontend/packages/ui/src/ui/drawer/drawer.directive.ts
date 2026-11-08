/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { FocusTrapFactory } from '@angular/cdk/a11y';
import { DestroyRef, Directive, ElementRef, afterRenderEffect, inject, input, output } from '@angular/core';

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
  private readonly focusTrap = inject(FocusTrapFactory).create(this.host, true);
  private opener: HTMLElement | null = null;
  private wasOpen = false;

  constructor() {
    this.focusTrap.enabled = false;
    inject(DestroyRef).onDestroy(() => this.focusTrap.destroy());
    afterRenderEffect(() => {
      const open = this.open();
      if (open === this.wasOpen) return;
      this.wasOpen = open;
      this.focusTrap.enabled = open;
      if (open) {
        this.opener = document.activeElement as HTMLElement | null;
        this.focusTrap.attachAnchors();
        this.focusTrap.focusFirstTabbableElement();
      } else {
        this.returnFocusToOpener();
      }
    });
  }

  protected onEscape(): void {
    if (this.open()) this.closed.emit();
  }

  private returnFocusToOpener(): void {
    const focused = document.activeElement;
    if (!focused || focused === document.body || this.host.contains(focused)) this.opener?.focus();
    this.opener = null;
  }
}
