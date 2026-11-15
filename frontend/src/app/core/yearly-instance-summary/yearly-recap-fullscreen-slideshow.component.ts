/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, effect, ElementRef, inject, signal, viewChild } from '@angular/core';
import { CdkTrapFocus } from '@angular/cdk/a11y';
import { IconComponent } from '@gradient/ui/ui';
import { NotifyService } from '@core/services/notify.service';
import { saveFile } from '@shared/download';
import { CountUpTextDirective } from './count-up-text.directive';
import { drawYearlyRecapSlideToPngBlob } from './draw-yearly-recap-slide-to-png-blob';
import { recapSlideColorSchemeColors } from './recap-slide-color-scheme-colors';
import { YearlyRecapAutoOpenService } from './yearly-recap-auto-open.service';
import { buildNonEmptySlidesFromRecapStatistics, yearlyRecapCreditLine } from './yearly-recap-slides';

const minimumHorizontalPointerMovementForSwipeInPixels = 50;
const previousSlideTapWidthShare = 1 / 3;
const longestTapDurationMilliseconds = 250;

@Component({
  selector: 'app-yearly-recap-fullscreen-slideshow',
  standalone: true,
  imports: [IconComponent, CdkTrapFocus, CountUpTextDirective],
  template: `
    @if (yearlyRecapAutoOpenService.open() && yearlyRecapAutoOpenService.recap(); as recap) {
      <div class="backdrop" [style]="currentSlideColorCustomProperties()" role="dialog" aria-modal="true" [attr.aria-label]="'Gradient Wrapped ' + recap.year"
           cdkTrapFocus [cdkTrapFocusAutoCapture]="true" (click)="onBackdropClickCloseSlideshowIfLastSlideShown($event)">
        @if (currentSlide(); as slide) {
          <button type="button" class="side-arrow previous" aria-label="Previous slide" [attr.aria-disabled]="currentSlideIndex() === 0"
                  (click)="moveToSlideAtIndexIfInRange(currentSlideIndex() - 1)">
            <gr-icon name="chevron_left" />
          </button>
          <div class="story" [attr.data-color-scheme]="slide.colorScheme"
               [class.paused]="isAutoAdvancePaused() || isPointerHoldPauseActive()">
            <span class="faint-background-logo" aria-hidden="true"></span>
            <header class="top">
              <div class="progress" aria-hidden="true">
                @for (s of slidesFromLoadedRecap(); track $index) {
                  <span class="segment" [class.done]="$index < currentSlideIndex()">
                    @if ($index === currentSlideIndex()) {
                      <span class="fill" [class.timed]="!isLastSlide()" (animationend)="advanceToNextSlideOnProgressBarFillEnd()"></span>
                    }
                  </span>
                }
              </div>
              <div class="controls">
                <span class="brand">Gradient Wrapped {{ recap.year }}</span>
                <button #autoAdvanceToggleButton type="button" class="icon-button" [style.visibility]="isLastSlide() ? 'hidden' : null"
                        [attr.aria-label]="isAutoAdvancePaused() ? 'Resume slideshow' : 'Pause slideshow'"
                        (click)="isAutoAdvancePaused.set(!isAutoAdvancePaused())">
                  <gr-icon [name]="isAutoAdvancePaused() ? 'play_arrow' : 'pause'" />
                </button>
                <button type="button" class="icon-button" aria-label="Download image" (click)="downloadCurrentYearlyRecapSlideAsPng()">
                  <gr-icon name="download" />
                </button>
                <button #closeSlideshowButton type="button" class="icon-button" aria-label="Close Gradient Wrapped" (click)="closeYearlyRecapSlideshow()">
                  <gr-icon name="close" />
                </button>
              </div>
            </header>
            <div class="bands" aria-hidden="true">
              @for (band of [currentSlideIndex()]; track currentSlideIndex()) {
                <span class="band band-1"></span>
                <span class="band band-2"></span>
                <span class="band band-3"></span>
              }
            </div>
            <p class="sr-only" aria-live="polite">Slide {{ currentSlideIndex() + 1 }}/{{ slidesFromLoadedRecap().length }}</p>
            <div class="stage" (pointerdown)="rememberPointerPressOnSlide($event)" (pointerup)="onSlidePointerRelease($event)"
                 (pointercancel)="isPointerHoldPauseActive.set(false)" (pointerleave)="isPointerHoldPauseActive.set(false)">
              @for (shown of [slide]; track currentSlideIndex()) {
                <article class="slide" [class.back]="previousSlideChangeWasBackward()" [attr.data-color-scheme]="shown.colorScheme">
                  @switch (shown.colorScheme) {
                    @case ('intro') {
                      <p class="intro-label">{{ shown.mainStatistic.label }}</p>
                      <h2 class="intro-year" [attr.aria-label]="shown.mainStatistic.value">
                        @for (digit of shown.mainStatistic.value.split(''); track $index) {
                          <span class="digit" aria-hidden="true" [style.--digit-index]="$index">{{ digit }}</span>
                        }
                      </h2>
                      <p class="intro-text">{{ shown.headingText }}</p>
                    }
                    @case ('summary') {
                      <h2 class="heading">{{ shown.headingText }}</h2>
                      <dl class="summary-grid">
                        @for (item of [shown.mainStatistic].concat(shown.secondaryStatistics); track $index) {
                          <div class="summary-item" [style.--item-index]="$index">
                            <dt>{{ item.label }}</dt>
                            <dd [class.text-value]="item.countUpTarget === null" [appCountUpText]="item"></dd>
                          </div>
                        }
                      </dl>
                      <button type="button" class="download-pill" (pointerdown)="$event.stopPropagation()" (pointerup)="$event.stopPropagation()"
                              (click)="downloadCurrentYearlyRecapSlideAsPng()">
                        <gr-icon name="download" />
                        Download image
                      </button>
                    }
                    @default {
                      <h2 class="heading">{{ shown.headingText }}</h2>
                      <div class="main">
                        <p class="main-value" [class.text-value]="shown.mainStatistic.countUpTarget === null" [appCountUpText]="shown.mainStatistic"></p>
                        <p class="main-label">{{ shown.mainStatistic.label }}</p>
                        @if (shown.mainStatistic.detail) {
                          <p class="main-detail">{{ shown.mainStatistic.detail }}</p>
                        }
                      </div>
                      @if (shown.secondaryStatistics.length) {
                        <dl class="secondary">
                          @for (item of shown.secondaryStatistics; track $index) {
                            <div class="secondary-item" [style.--item-index]="$index">
                              <dt>{{ item.label }}</dt>
                              <dd class="secondary-value" [appCountUpText]="item"></dd>
                              @if (item.detail) {
                                <dd class="secondary-detail">{{ item.detail }}</dd>
                              }
                            </div>
                          }
                        </dl>
                      }
                    }
                  }
                  @if (shown.footnoteText) {
                    <p class="footnote">{{ shown.footnoteText }}</p>
                  }
                </article>
              }
            </div>
            <p class="slide-credit">{{ creditLine }}</p>
          </div>
          <button type="button" class="side-arrow next" [attr.aria-label]="isLastSlide() ? 'Close Gradient Wrapped' : 'Next slide'"
                  (click)="isLastSlide() ? closeYearlyRecapSlideshow() : moveToSlideAtIndexIfInRange(currentSlideIndex() + 1)">
            <gr-icon [name]="isLastSlide() ? 'check' : 'chevron_right'" />
          </button>
        }
      </div>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './yearly-recap-fullscreen-slideshow.component.scss',
  host: { '(document:keydown)': 'onDocumentKeydownCloseYearlyRecapSlideshowOrMoveToAdjacentSlide($event)' },
})
export class YearlyRecapFullscreenSlideshowComponent {
  protected yearlyRecapAutoOpenService = inject(YearlyRecapAutoOpenService);
  private notify = inject(NotifyService);
  protected readonly creditLine = yearlyRecapCreditLine;
  protected currentSlideIndex = signal(0);
  protected previousSlideChangeWasBackward = signal(false);
  protected isAutoAdvancePaused = signal(false);
  protected isPointerHoldPauseActive = signal(false);
  protected slidesFromLoadedRecap = computed(() => {
    const recap = this.yearlyRecapAutoOpenService.recap();
    return recap ? buildNonEmptySlidesFromRecapStatistics(recap) : [];
  });
  protected currentSlide = computed(() => this.slidesFromLoadedRecap()[this.currentSlideIndex()] ?? null);
  protected isLastSlide = computed(() => this.currentSlideIndex() >= this.slidesFromLoadedRecap().length - 1);
  protected currentSlideColorCustomProperties = computed(() => {
    const slide = this.currentSlide();
    if (!slide) return {};
    const colors = recapSlideColorSchemeColors[slide.colorScheme];
    return {
      '--recap-background': colors.background,
      '--recap-text': colors.text,
      '--recap-accent': colors.accent,
      '--recap-band-1': colors.bands[0],
      '--recap-band-2': colors.bands[1],
      '--recap-band-3': colors.bands[2],
      '--recap-backdrop': recapSlideColorSchemeColors.intro.background,
      '--recap-backdrop-text': recapSlideColorSchemeColors.intro.text,
    };
  });
  private autoAdvanceToggleButton = viewChild<ElementRef<HTMLButtonElement>>('autoAdvanceToggleButton');
  private closeSlideshowButton = viewChild<ElementRef<HTMLButtonElement>>('closeSlideshowButton');
  private pointerPressStartX: number | null = null;
  private pointerDownTimestamp = 0;

  constructor() {
    effect(() => {
      if (!this.yearlyRecapAutoOpenService.open()) return;
      this.currentSlideIndex.set(0);
      this.isAutoAdvancePaused.set(false);
    });
  }

  protected closeYearlyRecapSlideshow(): void {
    this.yearlyRecapAutoOpenService.closeRecapSlideShowAndMarkOpenedOnServer();
  }

  protected onBackdropClickCloseSlideshowIfLastSlideShown(event: MouseEvent): void {
    if (event.target === event.currentTarget && this.isLastSlide()) this.closeYearlyRecapSlideshow();
  }

  protected onDocumentKeydownCloseYearlyRecapSlideshowOrMoveToAdjacentSlide(event: KeyboardEvent): void {
    if (!this.yearlyRecapAutoOpenService.open() || !this.currentSlide()) return;
    if (event.key === 'Escape') this.closeYearlyRecapSlideshow();
    else if (event.key === 'ArrowLeft') this.moveToSlideAtIndexIfInRange(this.currentSlideIndex() - 1);
    else if (event.key === 'ArrowRight') this.moveToSlideAtIndexIfInRange(this.currentSlideIndex() + 1);
  }

  protected moveToSlideAtIndexIfInRange(index: number): void {
    const slideCount = this.slidesFromLoadedRecap().length;
    if (index < 0 || index >= slideCount) return;
    if (index === slideCount - 1 && document.activeElement === this.autoAdvanceToggleButton()?.nativeElement) {
      this.closeSlideshowButton()?.nativeElement.focus();
    }
    this.previousSlideChangeWasBackward.set(index < this.currentSlideIndex());
    this.currentSlideIndex.set(index);
  }

  protected advanceToNextSlideOnProgressBarFillEnd(): void {
    if (!this.isLastSlide()) this.moveToSlideAtIndexIfInRange(this.currentSlideIndex() + 1);
  }

  protected rememberPointerPressOnSlide(event: PointerEvent): void {
    this.pointerPressStartX = event.clientX;
    this.pointerDownTimestamp = event.timeStamp;
    this.isPointerHoldPauseActive.set(true);
  }

  protected onSlidePointerRelease(event: PointerEvent): void {
    this.isPointerHoldPauseActive.set(false);
    if (this.pointerPressStartX === null) return;
    const horizontalPointerMovementPixels = event.clientX - this.pointerPressStartX;
    this.pointerPressStartX = null;
    if (Math.abs(horizontalPointerMovementPixels) >= minimumHorizontalPointerMovementForSwipeInPixels) {
      this.moveToSlideAtIndexIfInRange(this.currentSlideIndex() + (horizontalPointerMovementPixels < 0 ? 1 : -1));
      return;
    }
    if (event.timeStamp - this.pointerDownTimestamp > longestTapDurationMilliseconds) return;
    const tappedSlideAreaBoundingRectangle = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const tappedLeftThird = event.clientX - tappedSlideAreaBoundingRectangle.left < tappedSlideAreaBoundingRectangle.width * previousSlideTapWidthShare;
    this.moveToSlideAtIndexIfInRange(this.currentSlideIndex() + (tappedLeftThird ? -1 : 1));
  }

  protected downloadCurrentYearlyRecapSlideAsPng(): void {
    const recap = this.yearlyRecapAutoOpenService.recap();
    const slide = this.currentSlide();
    if (!recap || !slide) return;
    drawYearlyRecapSlideToPngBlob(slide, recap.year).then(
      (blob) => saveFile(blob, `gradient-wrapped-${recap.year}-${this.currentSlideIndex() + 1}.png`),
      (error: unknown) => this.notify.failure('Failed to create slide image', error),
    );
  }
}
