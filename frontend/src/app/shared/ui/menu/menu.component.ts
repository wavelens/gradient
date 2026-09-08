/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ESCAPE } from '@angular/cdk/keycodes';
import {
  ConnectedPosition,
  FlexibleConnectedPositionStrategyOrigin,
  Overlay,
  OverlayRef,
} from '@angular/cdk/overlay';
import { TemplatePortal } from '@angular/cdk/portal';
import { disposeAnimated } from '../overlay/overlay-animation';
import {
  Component,
  OnDestroy,
  TemplateRef,
  ViewContainerRef,
  inject,
  input,
  viewChild,
  ChangeDetectionStrategy
} from '@angular/core';
import { RouterLink } from '@angular/router';
import { MenuItem } from '../types';

const DROPDOWN_POSITIONS: ConnectedPosition[] = [
  { originX: 'end', originY: 'bottom', overlayX: 'end', overlayY: 'top', offsetY: 4 },
  { originX: 'end', originY: 'top', overlayX: 'end', overlayY: 'bottom', offsetY: -4 },
];

// A point origin collapses every corner onto the cursor, so the four entries
// only differ in which way the panel unfolds when it would leave the viewport.
const CONTEXT_POSITIONS: ConnectedPosition[] = [
  { originX: 'start', originY: 'bottom', overlayX: 'start', overlayY: 'top' },
  { originX: 'end', originY: 'bottom', overlayX: 'end', overlayY: 'top' },
  { originX: 'start', originY: 'top', overlayX: 'start', overlayY: 'bottom' },
  { originX: 'end', originY: 'top', overlayX: 'end', overlayY: 'bottom' },
];

@Component({
  selector: 'gr-menu',
  standalone: true,
  imports: [RouterLink],
  template: `
    <ng-template #panel>
      <ul class="gr-menu" role="menu">
        @for (item of model(); track $index) {
          @if (item.separator) {
            <li class="gr-menu__separator" role="separator"></li>
          } @else {
            <li role="none">
              @if (item.routerLink && !item.disabled) {
                <a
                  role="menuitem"
                  class="gr-menu__item"
                  [routerLink]="item.routerLink"
                  [queryParams]="item.queryParams ?? null"
                  (click)="run(item)"
                >
                  @if (item.icon) {
                    <span class="material-symbols-outlined gr-menu__icon" aria-hidden="true">{{ item.icon }}</span>
                  }
                  <span>{{ item.label }}</span>
                </a>
              } @else {
                <button
                  type="button"
                  role="menuitem"
                  class="gr-menu__item"
                  [disabled]="item.disabled"
                  (click)="run(item)"
                >
                  @if (item.icon) {
                    <span class="material-symbols-outlined gr-menu__icon" aria-hidden="true">{{ item.icon }}</span>
                  }
                  <span>{{ item.label }}</span>
                </button>
              }
            </li>
          }
        }
      </ul>
    </ng-template>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './menu.component.scss',
})
export class MenuComponent implements OnDestroy {
  model = input<readonly MenuItem[]>([]);

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
    this.attach(origin, DROPDOWN_POSITIONS);
  }

  /** Opens the menu at the cursor, replacing the browser's own context menu. */
  openAt(event: MouseEvent): void {
    event.preventDefault();
    // The Menu key raises `contextmenu` without pointer coordinates; anchor to
    // the element it fired on so keyboard users get the panel on their row.
    const keyboard = event.clientX === 0 && event.clientY === 0;
    const origin = keyboard
      ? ((event.currentTarget ?? event.target) as HTMLElement)
      : { x: event.clientX, y: event.clientY };
    this.attach(origin, CONTEXT_POSITIONS);
  }

  private attach(
    origin: FlexibleConnectedPositionStrategyOrigin,
    positions: ConnectedPosition[],
  ): void {
    this.hide();
    this.ref = this.overlay.create({
      hasBackdrop: true,
      backdropClass: 'cdk-overlay-transparent-backdrop',
      panelClass: 'gr-menu-panel',
      scrollStrategy: this.overlay.scrollStrategies.reposition(),
      positionStrategy: this.overlay.position().flexibleConnectedTo(origin).withPositions(positions),
    });
    this.ref.backdropClick().subscribe(() => this.hide());
    this.ref.keydownEvents().subscribe((e) => e.keyCode === ESCAPE && this.hide());
    this.ref.attach(new TemplatePortal(this.panel(), this.vcr));
  }

  hide(): void {
    const ref = this.ref;
    this.ref = undefined;
    disposeAnimated(ref);
  }

  protected run(item: MenuItem): void {
    this.hide();
    item.command?.();
  }
}
