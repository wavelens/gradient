/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import { Component, computed, input, ChangeDetectionStrategy } from '@angular/core';
import { DocLink, docsUrl } from '@core/docs';
import { LabelExplanationButtonComponent } from '@gradient/ui/ui';

@Component({
  selector: 'gr-label-help',
  standalone: true,
  imports: [LabelExplanationButtonComponent],
  template: `
    <gr-label-explanation-button
      [iconButtonAriaLabel]="title()"
      [explanationText]="explanationText()"
      [documentationPageUrl]="href()"
    />
  `,
  host: { '[attr.title]': 'null' },
  changeDetection: ChangeDetectionStrategy.Eager,
  styleUrl: './label-help.component.scss',
})
export class LabelHelpComponent {
  doc = input<DocLink>();
  title = input<string>('Learn more');
  explanationText = input<string>();

  protected readonly href = computed(() => {
    const doc = this.doc();
    return doc ? docsUrl(doc) : undefined;
  });
}
