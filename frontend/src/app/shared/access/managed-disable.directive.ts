/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive, ElementRef, Renderer2, booleanAttribute, computed, effect, inject, input } from '@angular/core';
import { takeUntilDestroyed } from '@angular/core/rxjs-interop';
import { NgControl } from '@angular/forms';
import { AccessState } from '@core/models/access.model';
import { AccessService } from './access.service';

/// This directive can also manage the `[disabled]` binding on the element, so the busy flag and lack of access don't overwrite each other.
@Directive({
  selector: '[appManagedDisable]',
  standalone: true,
})
export class ManagedDisableDirective {
  private el = inject(ElementRef<HTMLElement>);
  private renderer = inject(Renderer2);
  private access = inject(AccessService);
  private ngControl = inject(NgControl, { optional: true, self: true });

  appManagedDisable = input.required<AccessState | null | undefined>();
  disabled = input(false, { transform: booleanAttribute });

  private blockedReason = computed(() => {
    const state = this.appManagedDisable();
    return state && this.access.shouldDisableInput(state) ? accessDisabledReason(state) : null;
  });
  private shouldDisable = computed(() => this.disabled() || this.blockedReason() !== null);

  constructor() {
    effect(() => this.apply(this.shouldDisable(), this.blockedReason()));
    this.ngControl?.statusChanges
      ?.pipe(takeUntilDestroyed())
      .subscribe(() => queueMicrotask(() => this.syncControl(this.shouldDisable())));
  }

  private apply(disable: boolean, reason: string | null): void {
    const node = this.el.nativeElement;
    this.renderer.setProperty(node, 'disabled', disable);
    if (reason) {
      this.renderer.setAttribute(node, 'aria-disabled', 'true');
      this.renderer.setAttribute(node, 'title', reason);
    } else {
      this.renderer.removeAttribute(node, 'aria-disabled');
      this.renderer.removeAttribute(node, 'title');
    }
    this.syncControl(disable);
  }

  private syncControl(disable: boolean): void {
    const control = this.ngControl?.control;
    if (!control || control.disabled === disable) return;
    if (disable) control.disable({ emitEvent: false });
    else control.enable({ emitEvent: false });
  }
}

export function accessDisabledReason(state: AccessState): string {
  if (state.managed) return 'This item is managed by the server configuration and cannot be changed here.';
  return 'You have read-only access';
}
