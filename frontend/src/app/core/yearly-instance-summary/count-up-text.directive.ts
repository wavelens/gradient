/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { DestroyRef, Directive, ElementRef, effect, inject, input } from '@angular/core';
import type { RecapSlideStatistic } from './yearly-recap-slides';

const countUpDurationMilliseconds = 1400;
const countUpStartDelayMilliseconds = 350;

function easeOutCubic(progress: number): number {
  return 1 - (1 - progress) ** 3;
}

@Directive({ selector: '[appCountUpText]', standalone: true })
export class CountUpTextDirective {
  readonly appCountUpText = input.required<RecapSlideStatistic>();
  private element = inject<ElementRef<HTMLElement>>(ElementRef).nativeElement;
  private animationFrame: number | null = null;

  constructor() {
    inject(DestroyRef).onDestroy(() => this.cancelAnimation());
    effect(() => this.animate(this.appCountUpText()));
  }

  private animate(statistic: RecapSlideStatistic): void {
    this.cancelAnimation();
    const target = statistic.countUpTarget;
    const reducedMotion = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
    if (target === null || reducedMotion || typeof requestAnimationFrame !== 'function') {
      this.element.textContent = statistic.value;
      return;
    }
    this.element.textContent = statistic.formatCountUpValue(0);
    const startedAt = performance.now() + countUpStartDelayMilliseconds;
    const step = (now: number) => {
      const progress = Math.min(1, Math.max(0, (now - startedAt) / countUpDurationMilliseconds));
      this.element.textContent = progress >= 1 ? statistic.value : statistic.formatCountUpValue(target * easeOutCubic(progress));
      this.animationFrame = progress >= 1 ? null : requestAnimationFrame(step);
    };
    this.animationFrame = requestAnimationFrame(step);
  }

  private cancelAnimation(): void {
    if (this.animationFrame !== null) cancelAnimationFrame(this.animationFrame);
    this.animationFrame = null;
  }
}
