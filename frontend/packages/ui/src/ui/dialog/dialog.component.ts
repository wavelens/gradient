/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { CdkTrapFocus } from '@angular/cdk/a11y';
import { IconComponent } from '../icon/icon.component';
import { MessageBannerComponent } from '../message-banner/message-banner.component';
import { ESCAPE } from '@angular/cdk/keycodes';
import { Overlay, OverlayRef } from '@angular/cdk/overlay';
import { TemplatePortal } from '@angular/cdk/portal';
import { detachAnimated } from '../overlay/overlay-animation';
import { FormErrors } from '../form-field/form-errors';
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

/// CDK modal overlay. Escape, backdrop, and close button dismiss it. Exception: `busy` set during request, `closable` off for one-time secret. Ctrl+Enter doesn't resubmit while busy.
@Component({
  selector: 'gr-dialog',
  standalone: true,
  imports: [IconComponent, CdkTrapFocus, MessageBannerComponent],
  providers: [FormErrors],
  template: `
    <ng-template #panel>
      <div
        class="gr-dialog"
        role="dialog"
        aria-modal="true"
        [attr.aria-labelledby]="titleId"
        [attr.aria-busy]="busy() || null"
        [style.width]="width()"
        cdkTrapFocus
        [cdkTrapFocusAutoCapture]="true"
      >
        <div class="gr-dialog__header">
          <h2 class="gr-dialog__title" [id]="titleId">{{ header() }}</h2>
          @if (closable()) {
            <button type="button" class="gr-dialog__close" aria-label="Close" [disabled]="busy()" (click)="close()">
              <gr-icon name="close" />
            </button>
          }
        </div>
        <div class="gr-dialog__content"><ng-content /></div>
        @if (error(); as message) {
          <div class="gr-dialog__error"><gr-message-banner type="error">{{ message }}</gr-message-banner></div>
        }
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
  error = input<string | null>(null);
  busy = input(false, { transform: booleanAttribute });
  hide = output<void>();

  private panel = viewChild.required<TemplateRef<unknown>>('panel');
  protected titleId = `gr-dialog-title-${dialogSeq++}`;
  private overlay = inject(Overlay);
  private vcr = inject(ViewContainerRef);
  private ref?: OverlayRef;
  private leaving = false;

  constructor() {
    effect(() => (this.visible() ? this.open() : this.detach()));
  }

  ngOnDestroy(): void {
    this.ref?.dispose();
  }

  close(): void {
    if (this.closable() && !this.busy()) this.visible.set(false);
  }

  private open(): void {
    if (this.ref?.hasAttached()) return;
    this.ref ??= this.createOverlay();
    this.ref.attach(new TemplatePortal(this.panel(), this.vcr));
  }

  private createOverlay(): OverlayRef {
    const ref = this.overlay.create({
      hasBackdrop: true,
      backdropClass: 'gr-dialog-backdrop',
      panelClass: 'gr-dialog-panel',
      scrollStrategy: this.overlay.scrollStrategies.block(),
      positionStrategy: this.overlay.position().global().centerHorizontally().centerVertically(),
    });
    ref.keydownEvents().subscribe((e) => {
      if (e.keyCode === ESCAPE) this.close();
      if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) this.submit(ref, e);
    });
    ref.backdropClick().subscribe(() => this.close());
    return ref;
  }

  private submit(ref: OverlayRef, event: KeyboardEvent): void {
    if (this.busy()) {
      event.preventDefault();
      return;
    }
    const actions = ref.overlayElement.querySelectorAll<HTMLButtonElement>(
      '.gr-dialog__footer button:not(.gr-button--secondary):not(.gr-button--loading):not(:disabled)',
    );
    const primary = actions[actions.length - 1];
    if (!primary) return;
    event.preventDefault();
    primary.click();
  }

  private detach(): void {
    if (this.leaving || !this.ref?.hasAttached()) return;
    this.leaving = true;
    detachAnimated(this.ref, () => {
      this.leaving = false;
      this.hide.emit();
    });
  }
}
