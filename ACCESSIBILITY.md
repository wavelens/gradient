# Accessibility

Every page of the Gradient frontend should work with a keyboard and a screen reader, without relying on color or motion.

## In Place

- Light, dark and system theme.
- No animation of status icons, progress bars and the command palette under `prefers-reduced-motion`.
- Command palette on `/`, navigable with the arrow keys or `Ctrl+J` / `Ctrl+K`.
- Status icons carry a text label for screen readers.

## Reporting a Barrier

Open an [issue](https://github.com/wavelens/gradient/issues/new?labels=frontend) with:

- The page and the element that is hard or impossible to use.
- The assistive technology, browser and operating system.
- The expected behavior.

Accessibility issues are bugs and get the same priority as functional ones.
