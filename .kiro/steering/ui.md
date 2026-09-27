---
inclusion: always
---
# UI and UX standards

Owner's rule: **"For UI/UX always use best-in-class design, never AI slop."** It is an
acceptance criterion: a UI change that misses a standard below is not done.

## Look and feel
- A calm, dense desktop tool, like the status panes of well-made developer tools; not a
  web landing page. One primary fact per screen, stated first.
- Type: the system stack only (`system-ui, -apple-system, "Segoe UI", …`), no web fonts.
  Scale 12/16 caption · 13/20 body · 20/28 title (size/line height, px). Monospace only
  for technical values: addresses, paths, commands, ids. Numbers use tabular figures.
- Spacing on a 4 px base: 4, 8, 12, 16, 24, 32, 48. No other values without a comment.
- Colour only through semantic CSS custom properties (`--color-text`, `--color-ok`, …),
  with light and dark themes via `prefers-color-scheme`, both complete.
- Never: decorative gradients, glassmorphism or blur, emoji as icons, "AI" sparkle
  motifs, lorem ipsum, placeholder heroes, drop-shadow stacks, remote assets.

## Accessibility (WCAG 2.2 AA)
- Contrast: text 4.5:1; icons, status marks and focus rings 3:1; in both themes.
  `apps/desktop/src/renderer/style.test.ts` checks the token pairs.
- Status is never colour alone: each status has its own shape and its text.
- Keyboard focus is always visible (`:focus-visible`, 2 px outline).
- Respect `prefers-reduced-motion`: no animation that isn't essential.
- Live regions announce state changes only, not every refresh.

## States and copy
- Design every state: empty, loading, error, offline or unreachable, partial data.
- Copy says what happened, then what to do next, in plain language. Commands are shown
  copyable, exactly as typed. No error codes or stack traces as the message.
- No layout shift between states or refreshes; a refresh never clears a selection.
- Set a sensible minimum window size; narrow layouts tighten or stack, never scroll
  sideways.

## Review before a UI pull request
- Screenshot every state in light and dark: `apps/desktop/scripts/screenshot.ts`, with
  `scripts/fake-daemon.ts` to put the daemon into each state.
- Review each against this file, fix what fails, and write the critique and changes in
  the PR under "Design review".
