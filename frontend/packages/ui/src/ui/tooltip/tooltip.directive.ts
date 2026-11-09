/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  ConnectedPosition,
  Overlay,
  OverlayPositionBuilder,
  OverlayRef,
} from '@angular/cdk/overlay';
import { ComponentPortal } from '@angular/cdk/portal';
import { disposeAnimated } from '../overlay/overlay-animation';
import { DOCUMENT } from '@angular/common';
import {
  ChangeDetectionStrategy,
  Component,
  ComponentRef,
  computed,
  Directive,
  ElementRef,
  OnDestroy,
  effect,
  inject,
  input,
  signal,
} from '@angular/core';

export type TooltipPosition = 'top' | 'bottom' | 'left' | 'right';

const POSITIONS: Record<TooltipPosition, ConnectedPosition> = {
  top: { originX: 'center', originY: 'top', overlayX: 'center', overlayY: 'bottom', offsetY: -6 },
  bottom: { originX: 'center', originY: 'bottom', overlayX: 'center', overlayY: 'top', offsetY: 6 },
  left: { originX: 'start', originY: 'center', overlayX: 'end', overlayY: 'center', offsetX: -6 },
  right: { originX: 'end', originY: 'center', overlayX: 'start', overlayY: 'center', offsetX: 6 },
};

@Component({
  selector: 'gr-tooltip-panel',
  standalone: true,
  template: '<div class="gr-tooltip">{{ text }}</div>',
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './tooltip-panel.component.scss',
})
export class TooltipPanelComponent {
  text = '';
}

@Directive({
  selector: '[grTooltip]',
  standalone: true,
  host: {
    '(mouseenter)': 'show()',
    '(mouseleave)': 'hide()',
    '(focus)': 'show()',
    '(blur)': 'hide()',
  },
})
export class TooltipDirective implements OnDestroy {
  grTooltip = input('');
  tooltipPosition = input<TooltipPosition>('top');

  private disabledReason = signal('');
  private text = computed(() => this.disabledReason() || this.grTooltip());
  private tooltip = new TooltipOverlay();

  constructor() {
    effect(() => this.tooltip.update(this.text()));
  }

  ngOnDestroy(): void {
    this.hide();
  }

  show(): void {
    this.tooltip.show(this.text(), this.tooltipPosition());
  }

  hide(): void {
    this.tooltip.hide();
  }

  setTooltipTextFromButtonDisabledReason(reason: string): void {
    this.disabledReason.set(reason);
  }
}

export class TooltipOverlay {
  private overlay = inject(Overlay);
  private positions = inject(OverlayPositionBuilder);
  private host = inject<ElementRef<HTMLElement>>(ElementRef);
  private document = inject(DOCUMENT);
  private ref?: OverlayRef;
  private panel?: ComponentRef<TooltipPanelComponent>;
  private readonly hideWhenPointerLeavesHost = (event: PointerEvent) => {
    if (!(event.target instanceof Node) || !this.host.nativeElement.contains(event.target)) this.hide();
  };

  show(text: string, position: TooltipPosition = 'top'): void {
    if (!text || this.ref) return;
    this.ref = this.overlay.create({
      positionStrategy: this.positions
        .flexibleConnectedTo(this.host)
        .withPositions([POSITIONS[position], POSITIONS.bottom]),
      scrollStrategy: this.overlay.scrollStrategies.close(),
      panelClass: 'gr-tooltip-overlay',
    });
    this.panel = this.ref.attach(new ComponentPortal(TooltipPanelComponent));
    this.update(text);
    // A disabled host doesn't fire mouseleave. Listeners on the document watch the pointer target instead.
    this.document.addEventListener('pointermove', this.hideWhenPointerLeavesHost, { passive: true });
  }

  hide(): void {
    const ref = this.ref;
    this.ref = undefined;
    this.panel = undefined;
    this.document.removeEventListener('pointermove', this.hideWhenPointerLeavesHost);
    disposeAnimated(ref);
  }

  update(text: string): void {
    if (!text) return this.hide();
    if (!this.panel) return;
    this.panel.instance.text = text;
    this.panel.changeDetectorRef.detectChanges();
  }
}
