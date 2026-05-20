/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive, ElementRef, Input, OnInit, Renderer2, inject } from '@angular/core';
import { NgControl } from '@angular/forms';
import { AccessState } from '@core/models/access.model';
import { AccessService } from './access.service';

@Directive({
  selector: '[appManagedDisable]',
  standalone: true,
})
export class ManagedDisableDirective implements OnInit {
  private el = inject(ElementRef<HTMLElement>);
  private renderer = inject(Renderer2);
  private access = inject(AccessService);
  private ngControl = inject(NgControl, { optional: true, self: true });
  private state: AccessState | null | undefined;
  private initialized = false;

  @Input({ required: true }) set appManagedDisable(state: AccessState | null | undefined) {
    this.state = state;
    if (this.initialized) this.apply();
  }

  ngOnInit(): void {
    this.initialized = true;
    this.apply();
  }

  private apply(): void {
    const node = this.el.nativeElement;
    const disable = !!this.state && this.access.shouldDisableInput(this.state);
    this.renderer.setProperty(node, 'disabled', disable);
    if (disable) {
      this.renderer.setAttribute(node, 'aria-disabled', 'true');
      this.renderer.setAttribute(node, 'title', this.tooltip(this.state!));
    } else {
      this.renderer.removeAttribute(node, 'aria-disabled');
      this.renderer.removeAttribute(node, 'title');
    }
    this.ngControl?.valueAccessor?.setDisabledState?.(disable);
  }

  private tooltip(state: AccessState): string {
    if (state.managed) return 'Managed by Nix — edit via declarative config';
    return 'You have read-only access';
  }
}
