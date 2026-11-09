/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { CdkTrapFocus, InteractivityChecker } from '@angular/cdk/a11y';
import { IconComponent } from '../icon/icon.component';
import { ButtonComponent } from '../button/button.component';
import { MessageBannerComponent } from '../message-banner/message-banner.component';
import { ESCAPE } from '@angular/cdk/keycodes';
import { Overlay, OverlayRef } from '@angular/cdk/overlay';
import { TemplatePortal } from '@angular/cdk/portal';
import { detachAnimated } from '../overlay/overlay-animation';
import { HtmlOverflowHiddenScrollStrategy } from '../overlay/scroll-strategy';
import { FormErrors } from '../form-field/form-errors';
import {
  Component,
  DestroyRef,
  Injector,
  OnDestroy,
  ViewContainerRef,
  afterNextRender,
  booleanAttribute,
  effect,
  inject,
  input,
  model,
  output,
  signal,
  viewChild,
  ChangeDetectionStrategy
} from '@angular/core';
import { TemplateRef } from '@angular/core';

let dialogSeq = 0;

/// CDK modal overlay. Esc/backdrop/close-button dismiss. Close confirmation: `busy` set during request, `closable` off for one-time secret, `confirmDiscard` on unsaved-edit form. Ctrl+Enter doesn't resubmit while busy.
@Component({
  selector: 'gr-dialog',
  standalone: true,
  imports: [IconComponent, ButtonComponent, CdkTrapFocus, MessageBannerComponent],
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
        @if (isDiscardChangesPromptVisible()) {
          <div class="gr-dialog__discard">
            <p class="gr-dialog__discard-question" role="alert">Discard changes?</p>
            <button grButton type="button" severity="secondary" label="Continue editing" (click)="hideDiscardPromptAndReturnToForm()"></button>
            <button grButton type="button" severity="danger" label="Discard" (click)="discardEditsAndClose()"></button>
          </div>
        }
        <div class="gr-dialog__footer" [hidden]="isDiscardChangesPromptVisible()"><ng-content select="[grDialogFooter]" /></div>
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
  confirmDiscard = input(false, { transform: booleanAttribute });
  hide = output<void>();

  private panel = viewChild.required<TemplateRef<unknown>>('panel');
  protected titleId = `gr-dialog-title-${dialogSeq++}`;
  private overlay = inject(Overlay);
  private vcr = inject(ViewContainerRef);
  private injector = inject(Injector);
  private checker = inject(InteractivityChecker);
  private destroyRef = inject(DestroyRef);
  private ref?: OverlayRef;
  private leaving = false;
  private elementFocusedBeforeDialogOpened: HTMLElement | null = null;
  protected isDiscardChangesPromptVisible = signal(false);

  constructor() {
    effect(() => (this.visible() ? this.open() : this.detach()));
  }

  ngOnDestroy(): void {
    this.ref?.dispose();
  }

  close(): void {
    if (!this.closable() || this.busy()) return;
    if (!this.confirmDiscard()) {
      this.visible.set(false);
      return;
    }
    this.isDiscardChangesPromptVisible.set(true);
    afterNextRender(() => this.ref?.overlayElement.querySelector<HTMLElement>('.gr-dialog__discard button')?.focus(), {
      injector: this.injector,
    });
  }

  protected hideDiscardPromptAndReturnToForm(): void {
    this.isDiscardChangesPromptVisible.set(false);
    afterNextRender(() => this.focusInitialElementThenFirstFormFieldThenFooterButton(), { injector: this.injector });
  }

  protected discardEditsAndClose(): void {
    this.isDiscardChangesPromptVisible.set(false);
    this.visible.set(false);
  }

  private open(): void {
    if (this.ref?.hasAttached()) return;
    this.isDiscardChangesPromptVisible.set(false);
    this.elementFocusedBeforeDialogOpened = document.activeElement as HTMLElement | null;
    this.ref ??= this.createOverlay();
    this.ref.attach(new TemplatePortal(this.panel(), this.vcr));
    afterNextRender(() => this.focusInitialElementThenFirstFormFieldThenFooterButton(), { injector: this.injector });
  }

  private focusInitialElementThenFirstFormFieldThenFooterButton(): void {
    const panel = this.ref?.overlayElement;
    if (!panel) return;
    const firstTabbable = (selector: string) =>
      Array.from(panel.querySelectorAll<HTMLElement>(selector)).find((el) => this.checker.isFocusable(el) && this.checker.isTabbable(el));
    const target =
      panel.querySelector<HTMLElement>('[cdkFocusInitial]') ??
      firstTabbable('.gr-dialog__content :is(input, select, textarea)') ??
      firstTabbable('.gr-dialog__footer :is(button, a[href])') ??
      firstTabbable('button, a[href], input, select, textarea, [tabindex]');
    target?.focus();
  }

  private createOverlay(): OverlayRef {
    const ref = this.overlay.create({
      hasBackdrop: true,
      backdropClass: 'gr-dialog-backdrop',
      panelClass: 'gr-dialog-panel',
      scrollStrategy: new HtmlOverflowHiddenScrollStrategy(),
      positionStrategy: this.overlay.position().global().centerHorizontally().centerVertically(),
    });
    ref.keydownEvents().subscribe((e) => {
      if (e.keyCode === ESCAPE) {
        if (this.isDiscardChangesPromptVisible()) this.hideDiscardPromptAndReturnToForm();
        else this.close();
      }
      if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) this.submit(ref, e);
    });
    ref.backdropClick().subscribe(() => this.close());
    return ref;
  }

  private restoreFocusToOpenerOrMainContent(): void {
    const opener = this.elementFocusedBeforeDialogOpened;
    this.elementFocusedBeforeDialogOpened = null;
    const target = opener?.isConnected ? opener : document.getElementById('main-content');
    target?.focus({ preventScroll: true });
  }

  private submit(ref: OverlayRef, event: KeyboardEvent): void {
    if (this.busy() || this.isDiscardChangesPromptVisible()) {
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
      if (this.destroyRef.destroyed) return;
      this.restoreFocusToOpenerOrMainContent();
      this.hide.emit();
    });
  }
}
