/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  afterRenderEffect,
  computed,
  inject,
  input,
  linkedSignal,
  viewChild,
} from '@angular/core';
import { injectOnScreen } from '@gradient/ui/ui';
import type { StatusPhase } from '@shared/evaluation';

export type StatusIconSize = 'sm' | 'md';

const SPIN_RATE: Partial<Record<StatusPhase, number>> = { queued: 0.25, running: 1 };
const SPIN_KEYFRAMES: Keyframe[] = [{ transform: 'rotate(0deg)' }, { transform: 'rotate(360deg)' }];
const SPIN_LAP_MS = 1000;
const PRIORITIZED_RUN_BOOST = 2.5;

function motionAllowed(): boolean {
  return typeof Element.prototype.animate === 'function'
    && !window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
}

/// Morphs between phases; the first render is static so loading a page never replays history.
/// Whatever moves endlessly is an HTML element: the browser animates those off the main thread,
/// while a moving part inside an SVG repaints the whole page on every frame.
@Component({
  selector: 'gr-status-icon',
  standalone: true,
  host: {
    '[class]': "'gr-status-icon--' + size()",
    '[attr.data-phase]': 'phase()',
    '[attr.data-animate]': "animate() ? '' : null",
    '[attr.data-offscreen]': "onScreen() ? null : ''",
    '[attr.role]': "label() ? 'img' : null",
    '[attr.aria-label]': 'label() ?? null',
    '[attr.aria-hidden]': "label() ? null : 'true'",
  },
  template: `
    <span class="body">
      <span #spinner class="spin">
        <svg viewBox="0 0 24 24">
          <circle class="ring" cx="12" cy="12" r="9" pathLength="100" transform="rotate(-90 12 12)" />
        </svg>
      </span>
      <span class="glyphs">
        <svg viewBox="0 0 24 24">
          <path class="glyph check" pathLength="1" d="M8 12.4l2.8 2.8 5.2-5.6" />
          <path class="glyph cross" pathLength="1" d="M9.2 9.2l5.6 5.6" />
          <path class="glyph cross second" pathLength="1" d="M14.8 9.2l-5.6 5.6" />
          <path class="glyph slash" pathLength="1" d="M8.6 15.4l6.8-6.8" />
          <path class="glyph bar" pathLength="1" d="M10.2 9.2v5.6" />
          <path class="glyph bar second" pathLength="1" d="M13.8 9.2v5.6" />
        </svg>
      </span>
    </span>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './status-icon.component.scss',
})
export class StatusIconComponent {
  phase = input.required<StatusPhase>();
  size = input<StatusIconSize>('md');
  label = input<string>();
  prioritized = input(false);

  private readonly spinner = viewChild.required<ElementRef<HTMLElement>>('spinner');
  private readonly motion = motionAllowed();
  protected readonly onScreen = injectOnScreen();
  private spin?: Animation;

  private readonly changed = linkedSignal<StatusPhase, boolean>({
    source: this.phase,
    computation: (_, previous) => previous !== undefined,
  });

  protected readonly animate = computed(() => this.motion && this.changed());

  constructor() {
    afterRenderEffect(() => this.syncSpin(this.phase(), this.prioritized(), this.onScreen()));
    inject(DestroyRef).onDestroy(() => this.spin?.cancel());
  }

  private syncSpin(phase: StatusPhase, prioritized: boolean, onScreen: boolean): void {
    if (!this.motion) return;
    const rate = SPIN_RATE[phase];
    if (rate === undefined) this.finishLap();
    else this.spinning().updatePlaybackRate(phase === 'running' && prioritized ? rate * PRIORITIZED_RUN_BOOST : rate);
    if (onScreen && this.spin?.playState === 'paused') this.spin.play();
    if (!onScreen && this.spin?.playState === 'running') this.spin.pause();
  }

  private spinning(): Animation {
    if (this.spin && this.spin.playState !== 'finished') {
      this.spin.effect?.updateTiming({ iterations: Infinity });
      return this.spin;
    }
    this.spin = this.spinner().nativeElement.animate(SPIN_KEYFRAMES, { duration: SPIN_LAP_MS, iterations: Infinity });
    return this.spin;
  }

  private finishLap(): void {
    const effect = this.spin?.effect;
    const lap = effect?.getComputedTiming().currentIteration;
    if (effect && lap != null) effect.updateTiming({ iterations: lap + 1 });
  }
}
