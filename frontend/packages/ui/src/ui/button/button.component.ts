/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, booleanAttribute, computed, inject, input, ChangeDetectionStrategy } from '@angular/core';

export type ButtonSeverity = 'primary' | 'secondary' | 'danger';

/// Attribute component on a native <button>/<a>, so callers keep native
/// `disabled`, `type` and `routerLink` bindings and nothing here fights them.
@Component({
  selector: 'button[grButton], a[grButton]',
  standalone: true,
  template: `
    @if (loading()) {
      <span class="gr-button__spinner" aria-hidden="true"></span>
    } @else if (icon() && iconPos() === 'left') {
      <span class="gr-button__icon material-symbols-outlined" aria-hidden="true">{{ icon() }}</span>
    }
    @if (label()) {
      <span class="gr-button__label">{{ label() }}</span>
    }
    <ng-content />
    @if (!loading() && icon() && iconPos() === 'right') {
      <span class="gr-button__icon material-symbols-outlined" aria-hidden="true">{{ icon() }}</span>
    }
  `,
  host: {
    class: 'gr-button',
    '[class]': 'severityClass()',
    '[class.gr-button--small]': 'size() === "small"',
    '[class.gr-button--text]': 'text()',
    '[class.gr-button--icon-only]': '!label() && !!icon()',
    '[class.gr-button--loading]': 'loading()',
    '[attr.aria-busy]': 'loading() ? "true" : null',
    '[attr.aria-disabled]': 'loading() ? "true" : null',
    '[attr.tabindex]': 'loading() ? "-1" : null',
  },
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './button.component.scss',
})
export class ButtonComponent {
  label = input('');
  icon = input('');
  iconPos = input<'left' | 'right'>('left');
  severity = input<ButtonSeverity>('primary');
  size = input<'small' | 'normal'>('normal');
  loading = input(false, { transform: booleanAttribute });
  text = input(false, { transform: booleanAttribute });

  constructor() {
    inject<ElementRef<HTMLElement>>(ElementRef).nativeElement.addEventListener(
      'click',
      (event) => {
        if (!this.loading()) return;
        event.preventDefault();
        event.stopImmediatePropagation();
      },
      { capture: true },
    );
  }

  protected severityClass = computed(() => `gr-button--${this.severity() || 'primary'}`);
}
