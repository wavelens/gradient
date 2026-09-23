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
import type { StatusPhase } from '@shared/evaluation';

export type StatusIconSize = 'sm' | 'md';

const SPIN_RATE: Partial<Record<StatusPhase, number>> = { queued: 0.25, running: 1 };
const SPIN_KEYFRAMES: Keyframe[] = [{ transform: 'rotate(0deg)' }, { transform: 'rotate(360deg)' }];
const SPIN_LAP_MS = 1000;

function motionAllowed(): boolean {
  return typeof Element.prototype.animate === 'function'
    && !window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
}

/// Morphs between phases; the first render is static so loading a page never replays history.
@Component({
  selector: 'gr-status-icon',
  standalone: true,
  host: {
    '[class]': "'gr-status-icon--' + size()",
    '[attr.data-phase]': 'phase()',
    '[attr.data-animate]': "animate() ? '' : null",
    '[attr.role]': "label() ? 'img' : null",
    '[attr.aria-label]': 'label() ?? null',
    '[attr.aria-hidden]': "label() ? null : 'true'",
  },
  template: `
    <svg viewBox="0 0 24 24">
      <g class="body">
        <g #spinner class="spin">
          <circle class="ring" cx="12" cy="12" r="9" pathLength="100" transform="rotate(-90 12 12)" />
        </g>
        <path class="glyph check" pathLength="1" d="M8 12.4l2.8 2.8 5.2-5.6" />
        <path class="glyph cross" pathLength="1" d="M9.2 9.2l5.6 5.6" />
        <path class="glyph cross second" pathLength="1" d="M14.8 9.2l-5.6 5.6" />
        <path class="glyph slash" pathLength="1" d="M8.6 15.4l6.8-6.8" />
        <g class="pause">
          <path class="glyph bar" pathLength="1" d="M10.2 9.2v5.6" />
          <path class="glyph bar second" pathLength="1" d="M13.8 9.2v5.6" />
        </g>
      </g>
    </svg>
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './status-icon.component.scss',
})
export class StatusIconComponent {
  phase = input.required<StatusPhase>();
  size = input<StatusIconSize>('md');
  label = input<string>();

  private readonly spinner = viewChild.required<ElementRef<SVGGElement>>('spinner');
  private readonly motion = motionAllowed();
  private spin?: Animation;

  private readonly changed = linkedSignal<StatusPhase, boolean>({
    source: this.phase,
    computation: (_, previous) => previous !== undefined,
  });

  protected readonly animate = computed(() => this.motion && this.changed());

  constructor() {
    afterRenderEffect(() => this.syncSpin(this.phase()));
    inject(DestroyRef).onDestroy(() => this.spin?.cancel());
  }

  private syncSpin(phase: StatusPhase): void {
    if (!this.motion) return;
    const rate = SPIN_RATE[phase];
    if (rate === undefined) this.finishLap();
    else this.spinning().updatePlaybackRate(rate);
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
