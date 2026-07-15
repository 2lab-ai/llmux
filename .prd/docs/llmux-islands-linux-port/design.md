# llmux Islands presentation contract

Status: implementation contract for the macOS preservation boundary and the
Linux/KDE alignment pass.

## Platform boundary

The two native shells share semantic state, actions, privacy rules, and
receipts. They do not share a widget tree or a visual redesign.

- macOS preserves the native SwiftUI/AppKit presentation from commit
  `57df760cb57ba811d2a29bcddeb149eb7c4a04ad`, the parent of
  `5a82089 refactor(islands): renew native surfaces with advanced disclosure`.
  The visible restoration is judged against that tree, not against a newly
  interpreted approximation. Non-visual canonical account fields, privacy-safe
  accessibility labels, compile guards, shared-core behavior, and existing
  receipt content remain.
- Linux/KDE keeps the current monochrome, minimal presentation and its
  contextual `Advanced` disclosure. This document only normalizes geometry,
  alignment, and rhythm; it does not introduce another visual direction.
- The named `openai` UI/UX reference applies only to the Linux/KDE shell in
  this pass: black canvas, white ink, opacity tiers, square internal controls,
  flat surfaces, and no decorative color, shadow, gradient, or blur.

## Linux alignment tokens

All dimensions are device-independent QML pixels. The scale is based on 4px
increments, with two documented optical exceptions for segmented-control
insets and icon-to-label spacing.

| Token | Value | Use |
|---|---:|---|
| `spaceXs` | 4 | compact metadata separation |
| `spaceSm` | 8 | adjacent actions, peer cards, ordinary rows |
| `spaceMd` | 12 | header item rhythm and medium separation |
| `spaceLg` | 16 | form columns and strong internal separation |
| `pagePadding` | 24 | left and right page/header gutter |
| `sectionSpacing` | 24 | separation between top-level sections |
| `cardPadding` | 16 | content inset inside bordered cards |
| `peerGap` | 8 | gap between equal-rank cards and metric tiles |
| `formColumnGap` | 16 | gap from a field label to its control |
| `fieldLabelWidth` | 104 | right-aligned form-label column |
| `controlHeight` | 32 | button, field, combo, switch, checkbox, delegate |
| `controlPaddingX` | 12 | horizontal button/field content inset |
| `iconTextGap` | 6 | optical exception between icon and label |
| `segmentInset` | 2 | optical exception inside segmented control |
| `segmentItemHeight` | 28 | selected/unselected segment target |
| `chipPaddingX` | 8 | compact status-chip horizontal inset |
| `chipPaddingY` | 4 | compact status-chip vertical inset |
| `headerHeight` | 56 | expanded shell toolbar height |
| `navigationWidth` | 300 | three-route segmented navigation width |

## Linux geometry rules

1. At the 960px receipt width, every page spans `x=24...936`. The header brand
   begins at `x=24`, and the connection status ends at `x=936`; native toolbar
   padding must not add another inset.
2. Card content spans `x=40...920` when the card itself spans the page. Forms
   use a 104px right-aligned label column and a 16px column gap, so controls in
   a full-width card begin at `x=160`.
3. Buttons, text fields, combo boxes, switches, checkboxes, item delegates,
   segmented navigation, and status controls use a 32px row height. Labels and
   icons are vertically centered inside that invariant box.
4. Adjacent actions are content-width, share 12px horizontal padding, and are
   separated by exactly 8px. Primary emphasis may invert fill/ink but must not
   change bounds.
5. Two-column account cards, summary metrics, heatmap cards, and platform
   diagnostic peers use equal-width cells with an 8px gutter. Content length
   must not resize one peer relative to another.
6. Form rows use an 8px vertical gap and a 16px label/control gap. No page may
   inherit platform-style layout spacing for these relationships.
7. Body and control labels remain system sans at 12px. Form/metadata labels
   remain 11px; identifiers, timestamps, endpoints, and tabular telemetry may
   use the existing monospace family. Text alignment uses actual center/right
   properties rather than padding guesses.
8. Keyboard focus, disabled-state legibility, semantic warning/error/success
   color, and the default-versus-Advanced information hierarchy remain intact.

## Verification

The implementation is accepted only when all of the following are true:

- the macOS presentation matches the `57df760` pre-renewal tree at the defined
  visual boundary; its Usage, Statistics, and Menu full-surface captures plus
  the readable receipt-detail crop retain the original visual language;
- Linux component-contract tests assert both token values and their component
  bindings;
- the Linux test suite passes with local parallelism capped at `-j 2`;
- all seven Linux production-renderer receipts show complete shell chrome,
  equal peer widths, 24px outer gutters, 8px action/card gaps, and vertically
  centered 32px controls;
- no local Docker build is used; authoritative Arch/KDE rendering remains in
  the existing GitHub Actions job.
