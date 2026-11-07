/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
  Injector,
  afterNextRender,
  inject,
  input,
  signal,
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
                  [class.gr-menu__item--danger]="item.danger"
                  [routerLink]="item.routerLink"
                  [queryParams]="item.queryParams ?? null"
                  (click)="run(item)"
                  (auxclick)="hide()"
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
                  [class.gr-menu__item--danger]="item.danger"
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
  private injector = inject(Injector);
  private ref?: OverlayRef;
  private returnFocus?: HTMLElement;
  private readonly openState = signal(false);
  readonly isOpen = this.openState.asReadonly();

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
    this.returnFocus = origin instanceof HTMLElement ? origin : undefined;
    this.ref = this.overlay.create({
      hasBackdrop: true,
      backdropClass: 'cdk-overlay-transparent-backdrop',
      panelClass: 'gr-menu-panel',
      scrollStrategy: this.overlay.scrollStrategies.reposition(),
      positionStrategy: this.overlay.position().flexibleConnectedTo(origin).withPositions(positions),
    });
    this.ref.backdropClick().subscribe(() => this.hide());
    this.ref.keydownEvents().subscribe((e) => this.onKeydown(e));
    this.ref.attach(new TemplatePortal(this.panel(), this.vcr));
    this.openState.set(true);
    const panel = this.ref.overlayElement;
    afterNextRender(() => this.items(panel)[0]?.focus(), { injector: this.injector });
  }

  hide(): void {
    const ref = this.ref;
    this.ref = undefined;
    this.openState.set(false);
    disposeAnimated(ref);
  }

  private close(): void {
    const target = this.returnFocus;
    this.hide();
    target?.focus();
  }

  private items(panel: HTMLElement): HTMLElement[] {
    return Array.from(panel.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])'));
  }

  private onKeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape' || event.key === 'Tab') {
      if (event.key === 'Escape') event.preventDefault();
      return this.close();
    }
    if (!this.ref) return;
    const items = this.items(this.ref.overlayElement);
    const current = items.indexOf(document.activeElement as HTMLElement);
    const next: Record<string, number> = {
      ArrowDown: (current + 1) % items.length,
      ArrowUp: current <= 0 ? items.length - 1 : current - 1,
      Home: 0,
      End: items.length - 1,
    };
    if (!(event.key in next) || !items.length) return;
    event.preventDefault();
    items[next[event.key]].focus();
  }

  protected run(item: MenuItem): void {
    this.close();
    item.command?.();
  }
}
