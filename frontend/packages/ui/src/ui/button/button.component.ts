/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, ElementRef, OnDestroy, booleanAttribute, computed, effect, inject, input, ChangeDetectionStrategy } from '@angular/core';
import { TooltipDirective, TooltipOverlay } from '../tooltip/tooltip.directive';

export type ButtonSeverity = 'primary' | 'secondary' | 'danger';

let disabledReasonSpanSeq = 0;

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
    @if (disabledReason()) {
      <span hidden [id]="disabledReasonDescriptionId">{{ disabledReason() }}</span>
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
    '[attr.aria-disabled]': 'blocked() ? "true" : null',
    '[attr.aria-describedby]': 'disabledReason() ? disabledReasonDescriptionId : null',
    '[attr.tabindex]': 'loading() ? "-1" : null',
    '(mouseenter)': 'showDisabledReason()',
    '(focus)': 'showDisabledReason()',
    '(mouseleave)': 'reasonTooltip.hide()',
    '(blur)': 'reasonTooltip.hide()',
  },
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './button.component.scss',
})
export class ButtonComponent implements OnDestroy {
  label = input('');
  icon = input('');
  iconPos = input<'left' | 'right'>('left');
  severity = input<ButtonSeverity>('primary');
  size = input<'small' | 'normal'>('normal');
  loading = input(false, { transform: booleanAttribute });
  text = input(false, { transform: booleanAttribute });
  disabledReason = input<string | null | undefined>(null);

  protected disabledReasonDescriptionId = `gr-button-disabled-reason-${disabledReasonSpanSeq++}`;
  protected blocked = computed(() => this.loading() || !!this.disabledReason());
  private hostTooltip = inject(TooltipDirective, { self: true, optional: true });
  protected reasonTooltip = new TooltipOverlay();

  constructor() {
    inject<ElementRef<HTMLElement>>(ElementRef).nativeElement.addEventListener(
      'click',
      (event) => {
        if (!this.blocked()) return;
        event.preventDefault();
        event.stopImmediatePropagation();
      },
      { capture: true },
    );
    effect(() => {
      const reason = this.disabledReason() ?? '';
      if (this.hostTooltip) this.hostTooltip.setTooltipTextFromButtonDisabledReason(reason);
      else this.reasonTooltip.update(reason);
    });
  }

  ngOnDestroy(): void {
    this.reasonTooltip.hide();
  }

  protected showDisabledReason(): void {
    if (!this.hostTooltip) this.reasonTooltip.show(this.disabledReason() ?? '');
  }

  protected severityClass = computed(() => `gr-button--${this.severity() || 'primary'}`);
}
