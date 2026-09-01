/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { CdkTrapFocus } from '@angular/cdk/a11y';
import { IconComponent } from '../icon/icon.component';
import { ESCAPE } from '@angular/cdk/keycodes';
import { Overlay, OverlayRef } from '@angular/cdk/overlay';
import { TemplatePortal } from '@angular/cdk/portal';
import { detachAnimated } from '../overlay/overlay-animation';
import {
  Component,
  OnDestroy,
  ViewContainerRef,
  booleanAttribute,
  effect,
  inject,
  input,
  model,
  output,
  viewChild,
  ChangeDetectionStrategy
} from '@angular/core';
import { TemplateRef } from '@angular/core';

let dialogSeq = 0;

/// Modal dialog on the CDK overlay. Escape closes it, the backdrop does not,
/// matching what every call site relied on before.
@Component({
  selector: 'gr-dialog',
  standalone: true,
  imports: [IconComponent, CdkTrapFocus],
  template: `
    <ng-template #panel>
      <div
        class="gr-dialog"
        role="dialog"
        aria-modal="true"
        [attr.aria-labelledby]="titleId"
        [style.width]="width()"
        cdkTrapFocus
        [cdkTrapFocusAutoCapture]="true"
      >
        <div class="gr-dialog__header">
          <h2 class="gr-dialog__title" [id]="titleId">{{ header() }}</h2>
          @if (closable()) {
            <button type="button" class="gr-dialog__close" aria-label="Close" (click)="close()">
              <gr-icon name="close" />
            </button>
          }
        </div>
        <div class="gr-dialog__content"><ng-content /></div>
        <div class="gr-dialog__footer"><ng-content select="[grDialogFooter]" /></div>
      </div>
    </ng-template>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './dialog.component.scss',
})
export class DialogComponent implements OnDestroy {
  visible = model(false);
  header = input('');
  width = input('420px');
  closable = input(true, { transform: booleanAttribute });
  hide = output<void>();

  private panel = viewChild.required<TemplateRef<unknown>>('panel');
  protected titleId = `gr-dialog-title-${dialogSeq++}`;
  private overlay = inject(Overlay);
  private vcr = inject(ViewContainerRef);
  private ref?: OverlayRef;

  constructor() {
    effect(() => (this.visible() ? this.open() : this.detach()));
  }

  ngOnDestroy(): void {
    this.ref?.dispose();
  }

  close(): void {
    this.visible.set(false);
  }

  private open(): void {
    if (this.ref?.hasAttached()) return;
    this.ref ??= this.overlay.create({
      hasBackdrop: true,
      backdropClass: 'gr-dialog-backdrop',
      panelClass: 'gr-dialog-panel',
      scrollStrategy: this.overlay.scrollStrategies.block(),
      positionStrategy: this.overlay.position().global().centerHorizontally().centerVertically(),
    });
    this.ref.keydownEvents().subscribe((e) => {
      if (e.keyCode === ESCAPE && this.closable()) this.close();
    });
    this.ref.attach(new TemplatePortal(this.panel(), this.vcr));
  }

  private detach(): void {
    detachAnimated(this.ref, () => this.hide.emit());
  }
}
