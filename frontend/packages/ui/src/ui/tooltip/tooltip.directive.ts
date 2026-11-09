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
  Directive,
  ElementRef,
  OnDestroy,
  effect,
  inject,
  input,
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

  private overlay = inject(Overlay);
  private positions = inject(OverlayPositionBuilder);
  private host = inject(ElementRef<HTMLElement>);
  private document = inject(DOCUMENT);
  private ref?: OverlayRef;
  private panel?: ComponentRef<TooltipPanelComponent>;
  private readonly hideWhenPointerLeavesHost = (event: PointerEvent) => {
    if (!(event.target instanceof Node) || !this.host.nativeElement.contains(event.target)) this.hide();
  };

  constructor() {
    effect(() => {
      const text = this.grTooltip();
      if (!text) this.hide();
      else this.render(text);
    });
  }

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
    this.panel = this.ref.attach(new ComponentPortal(TooltipPanelComponent));
    this.render(this.grTooltip());
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

  private render(text: string): void {
    if (!this.panel) return;
    this.panel.instance.text = text;
    this.panel.changeDetectorRef.detectChanges();
  }
}
