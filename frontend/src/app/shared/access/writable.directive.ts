/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  Directive,
  Input,
  TemplateRef,
  ViewContainerRef,
  inject,
} from '@angular/core';
import { AccessState } from '@core/models/access.model';

@Directive({
  selector: '[appWritable]',
  standalone: true,
})
export class WritableDirective {
  private templateRef = inject(TemplateRef<unknown>);
  private vcr = inject(ViewContainerRef);
  private rendered = false;

  @Input({ required: true }) set appWritable(state: AccessState | null | undefined) {
    const shouldRender = !!state && state.canEdit;
    if (shouldRender && !this.rendered) {
      this.vcr.createEmbeddedView(this.templateRef);
      this.rendered = true;
    } else if (!shouldRender && this.rendered) {
      this.vcr.clear();
      this.rendered = false;
    }
  }
}
