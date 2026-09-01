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
import { Component, Directive, ElementRef, OnDestroy, inject, input, ChangeDetectionStrategy } from '@angular/core';

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
  styles: [
    `
      .gr-tooltip {
        max-width: 18rem;
        padding: 0.35rem 0.55rem;
        background: #050708;
        border: 1px solid #2d333b;
        border-radius: 5px;
        color: #fff;
        font-size: 0.75rem;
        line-height: 1.3;
      }
    `,
  ],
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

  private overlay = inject(Overlay);
  private positions = inject(OverlayPositionBuilder);
  private host = inject(ElementRef<HTMLElement>);
  private ref?: OverlayRef;

  ngOnDestroy(): void {
    this.hide();
  }

  show(): void {
    if (!this.grTooltip() || this.ref) return;
    this.ref = this.overlay.create({
      positionStrategy: this.positions
        .flexibleConnectedTo(this.host)
        .withPositions([POSITIONS[this.tooltipPosition()], POSITIONS.bottom]),
      scrollStrategy: this.overlay.scrollStrategies.close(),
      panelClass: 'gr-tooltip-overlay',
    });
    const panel = this.ref.attach(new ComponentPortal(TooltipPanelComponent));
    panel.instance.text = this.grTooltip();
    panel.changeDetectorRef.detectChanges();
  }

  hide(): void {
    const ref = this.ref;
    this.ref = undefined;
    disposeAnimated(ref);
  }
}
