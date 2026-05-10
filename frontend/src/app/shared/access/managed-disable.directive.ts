/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Directive, ElementRef, Input, Renderer2, inject } from '@angular/core';
import { AccessState } from '@core/models/access.model';
import { AccessService } from './access.service';

@Directive({
  selector: '[appManagedDisable]',
  standalone: true,
})
export class ManagedDisableDirective {
  private el = inject(ElementRef<HTMLElement>);
  private renderer = inject(Renderer2);
  private access = inject(AccessService);

  @Input({ required: true }) set appManagedDisable(state: AccessState | null | undefined) {
    this.apply(state);
  }

  private apply(state: AccessState | null | undefined): void {
    const node = this.el.nativeElement;
    if (state && this.access.shouldDisableInput(state)) {
      this.renderer.setProperty(node, 'disabled', true);
      this.renderer.setAttribute(node, 'aria-disabled', 'true');
      this.renderer.setAttribute(node, 'title', this.tooltip(state));
    } else {
      this.renderer.setProperty(node, 'disabled', false);
      this.renderer.removeAttribute(node, 'aria-disabled');
      this.renderer.removeAttribute(node, 'title');
    }
  }

  private tooltip(state: AccessState): string {
    if (state.managed) return 'Managed by Nix — edit via declarative config';
    return 'You have read-only access';
  }
}
