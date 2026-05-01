# Frontend Style Guide

Gradient ships a developer-facing style guide page that demonstrates every
shared primitive, color, and layout used in the frontend. It is intended as
the first stop when building a new feature: prefer reusing a primitive over
reinventing one in a feature component.

## Accessing the page

The page is mounted at `/styleguide`, lazy-loaded, and intentionally not
linked from any navigation. Open it directly in the browser while developing.

There is no auth guard — anyone with the URL can view it. It is read-only and
purely a reference; no production data is shown.

## What it covers

The page is structured into anchored sections (sticky table of contents on the
left):

1. **Colors** — palette swatches grouped as Base, Theme, UI, Semantic,
   Graph/Badge, and Text. Click a swatch to copy its hex.
2. **Typography** — font families, all `$font-size-*` tokens, headings, links,
   inline code.
3. **Spacing & Radius** — visual chips for every `$spacing-*` and
   `$border-radius-*` token.
4. **Icons** — Material Symbols and PrimeIcons sample grids.
5. **Buttons** — PrimeNG `pButton` severities, variants (icon, text, outlined,
   rounded), and states (default, disabled, loading).
6. **Form Primitives** — live demo of `<gr-form-field>`,
   `<gr-password-input>`, `<gr-message-banner>`, and the `FormFieldsBuilder`
   service.
7. **Popups & Overlays** — `<gr-form-dialog>`, PrimeNG confirm dialog,
   toast notifications, and tooltips.
8. **Feedback** — `<app-loading-spinner>`, `<app-empty-state>`,
   `<app-stat-card>`, status chips, and badges.
9. **Tables & Lists** — table styles, list rows using the
   `.gr-grid-rows` utility, breadcrumb.
10. **Grids** — `.gr-grid-stats`, `.gr-grid-form`, `.gr-grid-cards`,
    `.gr-grid-rows` utility classes from `app/styles/_grids.scss`.
11. **Layouts** — `<gr-page-layout>` + `<gr-settings-section>`, the
    primitives that replace the per-feature page-header / settings-card
    SCSS scattered across settings pages.

## Form primitives

Located at `frontend/src/app/shared/components/form/`.

| Component / Service | Purpose |
| --- | --- |
| `<gr-form-field>` | Wraps `label` + projected control + error + hint. Pass `[control]` to wire validation display. |
| `<gr-form-error>` | Renders a validation message for a control once touched/dirty. Built-in messages for `required`, `email`, `minlength`, `maxlength`, `min`, `max`, `pattern`, `passwordStrength`, `passwordMatch`, `usernameTaken`. Overridable per-instance via `[messages]`. |
| `<gr-password-input>` | Password input with a built-in show/hide toggle. Bind to a `FormControl` via `[control]`. |
| `<gr-message-banner>` | Page-level message with `error / success / warning / info` types. Default icon per type, overridable. |
| `<gr-form-dialog>` | PrimeNG `p-dialog` wrapper with standardized Cancel / Submit footer, loading state, and disabled state. |
| `FormFieldsBuilder` | Typed convenience wrapper around `FormBuilder`: `text`, `email`, `password`, `confirm`, `number`, `checkbox`. Inject and call instead of repeating `[Validators.required, ...]` everywhere. Also exports `passwordStrengthValidator` and `passwordMatchValidator` as standalone helpers. |

Import from the barrel:

```ts
import {
  FormFieldComponent,
  FormErrorComponent,
  PasswordInputComponent,
  MessageBannerComponent,
  FormDialogComponent,
  FormFieldsBuilder,
} from '@shared/components/form';
```

## Layout primitives

Located at `frontend/src/app/shared/components/layout/`.

| Component | Purpose |
| --- | --- |
| `<gr-page-layout>` | Page shell with title/subtitle header, optional `[slot=actions]` button row, optional `[slot=banner]` for top-of-page banners, and a content area. |
| `<gr-settings-section>` | Titled section with optional description; renders content inside a card by default (toggle with `[card]="false"`). |

## Grid utilities

Located at `frontend/src/app/styles/_grids.scss` and globally registered in
`src/styles.scss`.

| Class | Purpose |
| --- | --- |
| `.gr-grid-stats` | Auto-fit grid for stat-cards (220px min). |
| `.gr-grid-form` | Two-column form grid; collapses to one column below `$breakpoint-md`. Use `.gr-grid-form__full` on a child to span both columns. |
| `.gr-grid-cards` | Auto-fill grid for feature cards (280px min). |
| `.gr-grid-rows` | Three-column "label / value / actions" row grid. |
| `.gr-form-actions` | Flex row for form action buttons (`--end` modifier for right alignment). |

## Migrating existing forms

The current login, register, profile, organization-settings,
project-settings, cache-settings, integrations, api-keys, and admin/github-app
components each rebuild the same `.form-group` + label + error markup by hand.
Migrating them onto the primitives is intentionally out of scope for the
style-guide PR; do migrations incrementally per feature so each diff stays
small and reviewable.
