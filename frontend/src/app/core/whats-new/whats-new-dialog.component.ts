/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, effect, inject, signal } from '@angular/core';
import { ButtonComponent, DialogComponent, IconComponent } from '@gradient/ui/ui';
import { WhatsNewService } from './whats-new.service';

const SWIPE_PX = 50;

@Component({
  selector: 'app-whats-new-dialog',
  standalone: true,
  imports: [ButtonComponent, DialogComponent, IconComponent],
  template: `
    <gr-dialog header="What's New!" [visible]="whatsNew.open()" (visibleChange)="!$event && whatsNew.close()" width="min(760px, calc(100vw - 2rem))">
      @if (page(); as page) {
        <div class="slides" tabindex="-1" (pointerdown)="swipeStart($event)" (pointerup)="swipeEnd($event)"
             (keydown.arrowleft)="go(index() - 1)" (keydown.arrowright)="go(index() + 1)">
          @for (shown of [page]; track index()) {
            <article class="slide" [class.back]="back()">
              <div class="visual">
                @if (shown.image; as image) {
                  <img [src]="image.src" [alt]="image.alt" draggable="false" />
                } @else {
                  <gr-icon size="xl" [name]="shown.icon ?? 'auto_awesome'" />
                }
              </div>
              <span class="version">Gradient {{ shown.version }}</span>
              <h3>{{ shown.title }}</h3>
              <div class="notes" [innerHTML]="shown.html"></div>
            </article>
          }
        </div>
      }
      <div grDialogFooter class="footer">
        <div class="dots">
          @for (p of pages(); track $index) {
            <button type="button" class="dot" [class.active]="$index === index()"
                    [attr.aria-current]="$index === index() ? 'step' : null" [attr.aria-label]="'Page ' + ($index + 1)"
                    (click)="go($index)"></button>
          }
        </div>
        @if (index() > 0) {
          <button grButton label="Back" severity="secondary" (click)="go(index() - 1)"></button>
        }
        @if (last()) {
          <button grButton label="Got it" (click)="whatsNew.close()"></button>
        } @else {
          <button grButton label="Next" icon="arrow_forward" iconPos="right" (click)="go(index() + 1)"></button>
        }
      </div>
    </gr-dialog>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './whats-new-dialog.component.scss',
})
export class WhatsNewDialogComponent {
  protected whatsNew = inject(WhatsNewService);
  protected index = signal(0);
  protected back = signal(false);
  protected pages = computed(() => this.whatsNew.notes().flatMap((r) => r.pages.map((p) => ({ ...p, version: r.version }))));
  protected page = computed(() => this.pages()[this.index()] ?? null);
  protected last = computed(() => this.index() >= this.pages().length - 1);
  private swipeX: number | null = null;

  constructor() {
    effect(() => {
      if (this.whatsNew.open()) this.index.set(0);
    });
  }

  protected go(index: number): void {
    if (index < 0 || index >= this.pages().length) return;
    this.back.set(index < this.index());
    this.index.set(index);
  }

  protected swipeStart(event: PointerEvent): void {
    this.swipeX = event.clientX;
  }

  protected swipeEnd(event: PointerEvent): void {
    if (this.swipeX === null) return;
    const distance = event.clientX - this.swipeX;
    this.swipeX = null;
    if (Math.abs(distance) >= SWIPE_PX) this.go(this.index() + (distance < 0 ? 1 : -1));
  }
}
