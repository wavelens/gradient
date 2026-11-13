/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { IconComponent } from '../icon/icon.component';
import { TooltipDirective } from '../tooltip/tooltip.directive';

let explanationTextSpanSeq = 0;

@Component({
  selector: 'gr-label-explanation-button',
  standalone: true,
  imports: [IconComponent, TooltipDirective],
  template: `
    @if (documentationPageUrl(); as url) {
      <a
        class="label-explanation-button"
        [href]="url"
        target="_blank"
        rel="noopener noreferrer"
        [grTooltip]="tooltipText()"
        [attr.aria-label]="iconButtonAriaLabel()"
        [attr.aria-describedby]="explanationText() ? explanationTextId : null"
      >
        <gr-icon name="help" size="sm" />
      </a>
    } @else {
      <button
        type="button"
        class="label-explanation-button"
        [grTooltip]="tooltipText()"
        [attr.aria-label]="iconButtonAriaLabel()"
        [attr.aria-describedby]="explanationText() ? explanationTextId : null"
      >
        <gr-icon name="help" size="sm" />
      </button>
    }
    @if (explanationText(); as text) {
      <span hidden [id]="explanationTextId">{{ text }}</span>
    }
  `,
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './label-explanation-button.component.scss',
})
export class LabelExplanationButtonComponent {
  iconButtonAriaLabel = input.required<string>();
  explanationText = input<string>();
  documentationPageUrl = input<string>();

  protected readonly explanationTextId = `gr-label-explanation-${explanationTextSpanSeq++}`;
  protected readonly tooltipText = computed(() => this.explanationText() || this.iconButtonAriaLabel());
}
