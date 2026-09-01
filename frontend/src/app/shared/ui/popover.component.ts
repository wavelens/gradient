/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ESCAPE } from '@angular/cdk/keycodes';
import { Overlay, OverlayRef } from '@angular/cdk/overlay';
import { TemplatePortal } from '@angular/cdk/portal';
import {
  Component,
  OnDestroy,
  TemplateRef,
  ViewContainerRef,
  inject,
  viewChild,
  ChangeDetectionStrategy
} from '@angular/core';

/// Click-anchored overlay. Callers keep a template reference and call
/// `toggle($event)` from the element the panel should hang off.
@Component({
  selector: 'gr-popover',
  standalone: true,
  template: `
    <ng-template #panel>
      <div class="gr-popover" role="dialog"><ng-content /></div>
    </ng-template>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './popover.component.scss',
})
export class PopoverComponent implements OnDestroy {
  private panel = viewChild.required<TemplateRef<unknown>>('panel');
  private overlay = inject(Overlay);
  private vcr = inject(ViewContainerRef);
  private ref?: OverlayRef;

  ngOnDestroy(): void {
    this.ref?.dispose();
  }

  toggle(event: Event): void {
    if (this.ref?.hasAttached()) return this.hide();
    this.show((event.currentTarget ?? event.target) as HTMLElement);
  }

  show(origin: HTMLElement): void {
    this.hide();
    this.ref = this.overlay.create({
      hasBackdrop: true,
      backdropClass: 'cdk-overlay-transparent-backdrop',
      panelClass: 'gr-popover-panel',
      scrollStrategy: this.overlay.scrollStrategies.reposition(),
      positionStrategy: this.overlay
        .position()
        .flexibleConnectedTo(origin)
        .withPositions([
          { originX: 'start', originY: 'bottom', overlayX: 'start', overlayY: 'top', offsetY: 4 },
          { originX: 'start', originY: 'top', overlayX: 'start', overlayY: 'bottom', offsetY: -4 },
        ]),
    });
    this.ref.backdropClick().subscribe(() => this.hide());
    this.ref.keydownEvents().subscribe((e) => e.keyCode === ESCAPE && this.hide());
    this.ref.attach(new TemplatePortal(this.panel(), this.vcr));
  }

  hide(): void {
    this.ref?.dispose();
    this.ref = undefined;
  }
}
