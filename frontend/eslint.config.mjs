/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

import js from '@eslint/js';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['dist/**', 'node_modules/**', '.angular/**'] },
  {
    files: ['src/**/*.ts'],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    rules: {
      // The design system has one entry point; reaching past it re-creates the tangle
      // the single barrel exists to prevent.
      'no-restricted-imports': [
        'error',
        {
          patterns: [
            {
              group: ['@shared/ui/*', '**/shared/ui/*/*'],
              message: 'Import from @shared/ui instead of reaching into a component directory.',
            },
          ],
        },
      ],
      '@typescript-eslint/no-explicit-any': 'off',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_' }],
    },
  },
  {
    files: ['src/app/shared/ui/**/*.ts', 'src/app/features/styleguide/**/*.ts'],
    rules: { '@typescript-eslint/no-explicit-any': 'error' },
  },
  {
    // A primitive reaching for its own barrel is a cycle: the barrel is still
    // initialising when the component asks for a sibling, so the sibling is
    // undefined. Inside the layer, import the sibling directly.
    files: ['src/app/shared/ui/**/*.ts'],
    rules: {
      'no-restricted-imports': [
        'error',
        {
          patterns: [
            {
              group: ['@shared/ui', '@shared/ui/*'],
              message: 'Import the sibling directly; the barrel is for consumers.',
            },
          ],
        },
      ],
    },
  },
  {
    // Specs probe library option objects whose own types are deliberately loose.
    files: ['**/*.spec.ts'],
    rules: {
      '@typescript-eslint/no-explicit-any': 'off',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_', varsIgnorePattern: '^_' }],
    },
  },
);
