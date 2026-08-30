---
name: Cool
description: A precise local flight ledger for controlled, reviewable AI operations.
colors:
  mineral-ground: "hsl(86 20% 93%)"
  ledger-paper: "hsl(90 28% 97%)"
  flight-ink: "hsl(160 16% 11%)"
  recorder-green: "hsl(169 62% 22%)"
  recorder-paper: "hsl(90 28% 97%)"
  quiet-instrument: "hsl(111 13% 90%)"
  muted-notation: "hsl(158 11% 38%)"
  active-wash: "hsl(150 18% 88%)"
  ledger-rule: "hsl(140 10% 77%)"
  field-stroke: "hsl(140 10% 72%)"
  authority-amber: "hsl(41 100% 39%)"
  destructive-red: "hsl(0 84.2% 60.2%)"
  recorder-header: "#17201d"
  recorder-header-text: "#eef1ea"
  recorder-header-muted: "#aebcb6"
  live-green: "#34d399"
  dark-ground: "hsl(160 21% 6%)"
  dark-paper: "hsl(160 21% 8%)"
  dark-ink: "hsl(86 20% 93%)"
  dark-recorder-green: "hsl(167 52% 67%)"
  dark-instrument: "hsl(160 19% 14%)"
  dark-notation: "hsl(150 10% 67%)"
  dark-active-wash: "hsl(160 20% 17%)"
  dark-rule: "hsl(157 16% 20%)"
  dark-field-stroke: "hsl(157 16% 25%)"
  dark-authority-amber: "hsl(41 100% 62%)"
typography:
  display:
    fontFamily: "Cool Flight Display, Arial Narrow, sans-serif"
    fontSize: "clamp(1.875rem, 4vw, 2.25rem)"
    fontWeight: 600
    lineHeight: 1.1
    letterSpacing: "-0.015em"
  headline:
    fontFamily: "Cool Flight Display, Arial Narrow, sans-serif"
    fontSize: "1.5rem"
    fontWeight: 600
    lineHeight: 1.2
    letterSpacing: "-0.015em"
  title:
    fontFamily: "Cool Flight Display, Arial Narrow, sans-serif"
    fontSize: "1.125rem"
    fontWeight: 600
    lineHeight: 1.25
    letterSpacing: "-0.015em"
  body:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "1rem"
    fontWeight: 400
    lineHeight: 1.5
    letterSpacing: "normal"
  body-sm:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.25
    letterSpacing: "normal"
  instrument-label:
    fontFamily: "ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif"
    fontSize: "0.6875rem"
    fontWeight: 700
    lineHeight: "1rem"
    letterSpacing: "0.08em"
  mono:
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace"
    fontSize: "0.75rem"
    fontWeight: 500
    lineHeight: "1rem"
    letterSpacing: "normal"
rounded:
  sm: "0.375rem"
  md: "0.5rem"
  lg: "0.625rem"
  xl: "0.75rem"
spacing:
  xs: "0.25rem"
  sm: "0.5rem"
  md: "0.75rem"
  lg: "1rem"
  xl: "1.5rem"
  2xl: "2rem"
components:
  button-primary:
    backgroundColor: "{colors.recorder-green}"
    textColor: "{colors.recorder-paper}"
    typography: "{typography.body-sm}"
    rounded: "{rounded.md}"
    padding: "0.5rem 1rem"
    height: "2.25rem"
  button-warning:
    backgroundColor: "{colors.authority-amber}"
    textColor: "{colors.flight-ink}"
    typography: "{typography.body-sm}"
    rounded: "{rounded.md}"
    padding: "0.5rem 1rem"
    height: "2.25rem"
  button-outline:
    backgroundColor: "{colors.ledger-paper}"
    textColor: "{colors.flight-ink}"
    typography: "{typography.body-sm}"
    rounded: "{rounded.md}"
    padding: "0.5rem 1rem"
    height: "2.25rem"
  button-ghost:
    backgroundColor: "transparent"
    textColor: "{colors.flight-ink}"
    typography: "{typography.body-sm}"
    rounded: "{rounded.md}"
    padding: "0.5rem 1rem"
    height: "2.25rem"
  input:
    backgroundColor: "{colors.ledger-paper}"
    textColor: "{colors.flight-ink}"
    typography: "{typography.body-sm}"
    rounded: "{rounded.md}"
    padding: "0.25rem 0.75rem"
    height: "2.25rem"
  card:
    backgroundColor: "{colors.ledger-paper}"
    textColor: "{colors.flight-ink}"
    rounded: "{rounded.lg}"
    padding: "1.5rem"
  nav-item:
    backgroundColor: "transparent"
    textColor: "{colors.muted-notation}"
    typography: "{typography.instrument-label}"
    rounded: "{rounded.md}"
    padding: "0.5rem"
    height: "2.5rem"
  run-recorder:
    backgroundColor: "{colors.ledger-paper}"
    textColor: "{colors.flight-ink}"
    rounded: "{rounded.lg}"
    width: "15rem"
---

# Design System: Cool

## Overview

**Creative North Star: "The Flight Ledger"**

Cool is a local AI operations console treated as a precise flight ledger. Tasks are missions,
runs are recorded operations, and authority, budget, execution, and evidence remain visible
without turning the workspace into theatre. The interface feels like a dependable recorder:
grounded, legible, and ready for long technical sessions.

The world is built from mineral paper, recorder green, authority amber, condensed display
headings, instrument labels, mono identifiers, and restrained ruled lines. Persistent surfaces
stay flat; transient overlays earn stronger depth. The active task remains central while the
live record exposes what the agent intends, may do, is doing, and has left for review.

**Key Characteristics:**

- Mineral-paper canvas with semantic 32px ledger ruling.
- Recorder-green operational actions and amber consequential or authority states.
- Condensed display headings, system-sans body copy, instrument labels, and mono identifiers.
- Persistent desktop run recorder with Intent / Authority / Execution / Review.
- Explicit draft, live, completed, failed, and approval language; color never carries state alone.
- Compact Radix recorder panel and visible Record control below the `xl` breakpoint.

## Colors

The palette combines warm mineral surfaces with deep green-black ink; recorder green drives
ordinary action, while amber is reserved for authority, consequence, and attention.

### Primary

- **Recorder Green:** the operational action color for starting, connecting, selecting, and
  confirming routine work. It also supplies the focus ring and active progress.
- **Recorder Paper:** the high-contrast text and icon color used on Recorder Green.

### Secondary

- **Authority Amber:** pending approval, consequential controls, budget pressure, and attention
  states. Always pair it with explicit wording such as “Approval required.”
- **Live Green:** the recorder's live/completed signal. It accompanies `Live`, `Completed`, or
  comparable text and never stands alone.
- **Destructive Red:** denial, deletion, failure, and irreversible committing actions.

### Neutral

- **Mineral Ground:** the ruled application canvas and default page ground.
- **Ledger Paper:** cards, sidebar, fields, panels, and other persistent working surfaces.
- **Flight Ink:** primary copy, icons, and high-emphasis structure.
- **Quiet Instrument:** low-contrast grouping, inactive steps, and nested operational wells.
- **Muted Notation:** descriptions, placeholders, metadata, and inactive navigation.
- **Active Wash:** selected rows and quiet hover feedback.
- **Ledger Rule:** structural borders, dividers, and the translucent 32px canvas rule.
- **Field Stroke:** the slightly stronger boundary for inputs and outlined controls.
- **Recorder Header:** the deep green-black cap of the live recorder, with Recorder Header Text
  and Recorder Header Muted for its label hierarchy.
- The dark theme uses the corresponding green-black ground, paper, notation, rule, and action
  tokens. It preserves the same operational hierarchy rather than becoming a separate identity.

**The Recorder Green Rule.** Recorder Green means ordinary forward operation. Do not spend it
on decorative panels, gradients, or generic emphasis.

**The Amber Authority Rule.** Amber marks consequence, approval, budget pressure, or attention;
it never substitutes for routine primary action.

## Typography

**Display Font:** Cool Flight Display, the self-hosted OFL IBM Plex Sans Condensed Semibold,
with Arial Narrow and sans-serif fallbacks  
**Body Font:** system UI sans-serif, with Segoe UI and Roboto fallbacks  
**Label/Mono Font:** system UI sans-serif for instrument labels; platform monospace for run and
model identifiers

**Character:** Condensed headings give Cool the bearing of an instrument panel without making
body copy mechanical. The body remains familiar and quiet; machine identifiers switch to mono
only when their exact form matters.

### Hierarchy

- **Display** (600, responsive `1.875rem`–`2.25rem`, 1.1 line-height): first-run questions and
  major mission framing.
- **Headline** (600, `1.5rem`, 1.2 line-height): primary operational page headings.
- **Title** (600, `1.125rem`, 1.25 line-height): panels, dialogs, recorder panel headings, and
  compact sections.
- **Body** (400, `1rem`, 1.5 line-height): explanations and readable output; prose stays near
  60–75 characters wide.
- **Small Body** (400, `0.875rem`, 1.25 line-height): controls, rows, event values, and secondary
  descriptions.
- **Instrument Label** (700, `0.6875rem`, `0.08em` tracking, uppercase): navigation groups,
  recorder stages, and compact operational labels.
- **Mono Identifier** (500, `0.75rem`, `1rem` line-height): run numbers, model names, tokens,
  timings, tool names, and code-adjacent facts.

**The Instrument, Not Costume Rule.** Use condensed display type for hierarchy and instrument
labels for structure. Do not turn ordinary prose into condensed, uppercase, or monospace copy.

## Layout

The full-height shell divides the workspace into a fixed 18rem desktop sidebar, a flexible
mission canvas, and—on active chat screens at `xl` and above—a persistent 15rem run-recorder
rail. Conversation content and the composer remain centered at a 48rem maximum width. First-run
content uses a broader 64rem composition with the main setup journey beside a 15rem record
preview.

Spacing follows a 4/8/12/16/24/32px rhythm. The canvas ledger repeats every 32px and appears
only on the working ground; it is semantic orientation, not wallpaper for every surface. The
first-run path reads Connect / Choose / Run, and setup copy makes the editable-draft transition
explicit before an operation begins.

At 768px, the desktop sidebar becomes a modal navigation drawer and controls meet a 44px minimum
touch target. Below 1280px, the persistent recorder becomes a visibly labelled `Record` control
in the chat header; it opens a compact Radix panel from the right, up to 23rem wide and no wider
than the viewport minus 2rem. The panel remains a recorder, not a second navigation system.

**The Working Width Rule.** Give operations room, but keep reading, composing, and decisions
bounded; never stretch the task simply because the viewport is wide.

**The Record Never Disappears Rule.** Keep the recorder persistent on wide desktop and directly
reachable through the labelled Record control everywhere below `xl`.

## Elevation & Depth

Cool is flat by default. Persistent cards, sidebar regions, ruled canvas, and the desktop
recorder are separated through tone and one-pixel rules. Small action controls may use a low
shadow; dialogs, drawers, sheets, and temporary artifact overlays use the strong shadow their
transient hierarchy requires. Radix overlays use a dark scrim with restrained backdrop blur.

### Shadow Vocabulary

- **Control Lift** (`0 1px 2px rgb(0 0 0 / 0.05)`): primary, warning, and secondary controls.
- **Floating Surface** (`0 10px 15px -3px rgb(0 0 0 / 0.10), 0 4px 6px -4px rgb(0 0 0 / 0.10)`):
  dialogs and compact panels above the working canvas.
- **Transient Overlay** (`0 20px 25px -5px rgb(0 0 0 / 0.10), 0 8px 10px -6px rgb(0 0 0 / 0.10)`):
  temporary artifact overlays and similarly consequential floating surfaces.

**The Flat Recorder Rule.** Persistent operational surfaces stay flat. Strong shadows belong
only to transient overlays that interrupt or temporarily cover the ledger.

## Shapes

The form language uses compact, gently rounded rectangles and crisp one-pixel rules. Small
controls use a 0.5rem radius, standard cards and recorder strips use 0.625rem, and larger
dialog surfaces may use 0.75rem. The mobile recorder panel is intentionally squared to the
right viewport edge; only the contained recorder card is rounded. Status marks are small
squares rather than ornamental pills, matching the recorded-instrument character.

**The Contained Radius Rule.** Round the instrument, not the entire workspace. Nested controls
must remain tighter than their containing surface.

## Components

Components are calm operational instruments. Every consequential state combines color with
plain language, and every mobile control preserves a 44px target.

### Buttons

- **Shape:** compact rectangle with a 0.5rem radius and 2.25rem desktop height; mobile targets
  expand to at least 2.75rem.
- **Primary:** Recorder Green with Recorder Paper, semibold small-body text, `0.5rem 1rem`
  padding, and a low control shadow.
- **Warning:** Authority Amber with Flight Ink for approval, budget, and consequential actions.
- **Hover / Focus:** hover adjusts the existing semantic fill; keyboard focus uses a two-pixel
  Recorder Green ring with a ground-color offset.
- **Outline / Ghost:** outline uses Ledger Paper and Field Stroke; ghost remains flat until an
  Active Wash hover. Link actions use Recorder Green and underline on hover.

### Chips

- **Style:** small, semibold state labels with compact geometry. Status marks may be square;
  textual chips use the component radius.
- **State:** explicit wording accompanies semantic color. Draft, Live, Completed, Failed, and
  Approval required must remain readable without hue.

### Cards / Containers

- **Corner Style:** 0.625rem for standard cards and recorder strips; 0.75rem for larger dialogs.
- **Background:** Ledger Paper over Mineral Ground; Quiet Instrument for nested wells.
- **Shadow Strategy:** flat at rest, lifted only when transient.
- **Border:** one-pixel Ledger Rule; the recorder also uses a subtle inset top highlight.
- **Internal Padding:** 0.75rem for compact rows and 1.5rem for canonical content cards.

### Inputs / Fields

- **Style:** Ledger Paper field, one-pixel Field Stroke, 0.5rem radius, 2.25rem desktop height,
  and 0.75rem horizontal padding.
- **Focus:** border and one-pixel ring shift to Recorder Green; avoid duplicate outlines.
- **Error / Disabled:** destructive text or stroke plus explicit error copy; disabled fields
  retain shape and drop to 50% opacity.

### Navigation

- Desktop navigation lives in an 18rem Ledger Paper sidebar with the Cool mark, `LOCAL FLIGHT
  LEDGER` instrument label, a Recorder Green new-conversation action, project/conversation lists,
  and grouped Knowledge / Agents / Workflows / Operations destinations.
- Active rows receive Active Wash and Flight Ink; inactive rows use Muted Notation and strengthen
  on hover. Group labels use instrument typography; destination rows remain human-readable.
- Mobile navigation becomes an opaque Radix drawer with a scrim, strong transient depth, and
  44px row targets.

### Dialogs and Overlays

- Radix dialogs use a 60% black scrim, subtle backdrop blur, bordered Ledger Paper surfaces,
  responsive 1rem / 1.5rem padding, and a maximum 85vh height.
- Close controls keep a full 44px mobile target and a visible focus ring.
- The mobile flight-ledger panel is full-height, right-aligned, square to the viewport edge,
  and labelled `Active record` / `Flight ledger` before the recorder content.

### Live Run Recorder

The Live Run Recorder is Cool's signature component and must remain beside the active task on
wide desktop. Its deep recorder header names `COOL RECORDER`, shows a mono `RUN / DRAFT` or
zero-padded run identifier, and pairs a square signal with explicit Draft, Live, or terminal
status text. Four ordered rows expose **Intent**, **Authority**, **Execution**, and **Review**;
each row combines a four-pixel semantic edge with an instrument label and explicit value. A
muted footer records the selected model in mono.

Draft is not a run. Before execution, the recorder says `RUN / DRAFT` and uses waiting language.
Once the run is registered, the durable run identifier and real status replace it. New recorder
entries arrive with a 260ms `cubic-bezier(0.16, 1, 0.3, 1)` transition; the live signal pulses
every 1.8s. Both animations are disabled under `prefers-reduced-motion`.

**The State Reads Twice Rule.** Critical state always pairs color with explicit text, and often
an icon or position. Hue alone is never evidence.

**The Draft Is Not a Run Rule.** Preserve the visual and verbal distinction between an editable
draft and a registered durable run.

## Do's and Don'ts

### Do:

- **Do** use Mineral Ground, Ledger Paper, Flight Ink, and restrained 32px rules as the quiet
  base of the system.
- **Do** use Recorder Green for ordinary forward operation and Authority Amber for consequence,
  approval, budget pressure, or attention.
- **Do** keep the user's task central while exposing Intent / Authority / Execution / Review
  beside it or behind the labelled Record control.
- **Do** preserve the draft/run distinction with explicit wording and a durable run identifier.
- **Do** use condensed display type for hierarchy, instrument labels for structure, and mono for
  exact machine identifiers.
- **Do** keep persistent surfaces flat, transient overlays visibly elevated, and mobile targets
  at least 44px.
- **Do** disable recorder entry and pulse motion when reduced motion is requested.

### Don't:

- **Don't** replace the Flight Ledger with a generic neutral AI-console identity.
- **Don't** make Cool look like a generic purple admin dashboard or theatrical sci-fi cockpit.
- **Don't** use ledger ruling as ornamental texture on cards, dialogs, or every nested panel.
- **Don't** hide the run record behind an icon-only control or remove the persistent desktop rail.
- **Don't** collapse Draft, Live, Completed, Failed, and Approval required into color-only status.
- **Don't** add strong shadows to persistent cards, the sidebar, or the desktop recorder.
- **Don't** use condensed display or monospace for ordinary body copy.
