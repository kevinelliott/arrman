---
name: Arrman Engineering Watch
description: A flat, keyboard-first engineering watch shared by terminal and browser operations.
colors:
  ink: "#070e14"
  panel: "#0d1922"
  panel-active: "#182d38"
  panel-raised: "#11232e"
  steel: "#4d6470"
  steel-dim: "#2b414c"
  text: "#dae5e7"
  muted: "#899ea6"
  sea-glass: "#68d3be"
  ready: "#70cf89"
  warning: "#efb958"
  fault: "#ef686f"
  information: "#6fafdb"
  focus: "#9ae8d9"
typography:
  display:
    fontFamily: "ui-monospace, SFMono-Regular, Cascadia Code, Roboto Mono, Consolas, Liberation Mono, monospace"
    fontSize: "clamp(1.6rem, 1.1rem + 2.4vw, 3.1rem)"
    fontWeight: 700
    lineHeight: 1.12
    letterSpacing: "-0.035em"
  headline:
    fontFamily: "ui-monospace, SFMono-Regular, Cascadia Code, Roboto Mono, Consolas, Liberation Mono, monospace"
    fontSize: "clamp(1.15rem, 1rem + 0.65vw, 1.65rem)"
    fontWeight: 700
    lineHeight: 1.12
    letterSpacing: "-0.025em"
  title:
    fontFamily: "ui-monospace, SFMono-Regular, Cascadia Code, Roboto Mono, Consolas, Liberation Mono, monospace"
    fontSize: "clamp(1rem, 0.94rem + 0.3vw, 1.25rem)"
    fontWeight: 700
    lineHeight: 1.12
    letterSpacing: "-0.015em"
  body:
    fontFamily: "ui-monospace, SFMono-Regular, Cascadia Code, Roboto Mono, Consolas, Liberation Mono, monospace"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.5
  label:
    fontFamily: "ui-monospace, SFMono-Regular, Cascadia Code, Roboto Mono, Consolas, Liberation Mono, monospace"
    fontSize: "0.74rem"
    fontWeight: 700
    lineHeight: 1.5
    letterSpacing: "0.04em"
rounded:
  none: "0px"
spacing:
  terminal-cell: "1ch"
  terminal-row: "1lh"
  micro: "0.35rem"
  compact: "0.55rem"
  control: "0.8rem"
  section: "1rem"
  fluid-page: "clamp(0.7rem, 1.7vw, 1.4rem)"
components:
  panel:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.text}"
    rounded: "{rounded.none}"
    padding: "{spacing.terminal-cell}"
  selected-row:
    backgroundColor: "{colors.panel-active}"
    textColor: "{colors.text}"
    rounded: "{rounded.none}"
    padding: "0 1ch"
  tab-active:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.sea-glass}"
    typography: "{typography.label}"
  status-ready:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.ready}"
    typography: "{typography.label}"
  status-wait:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.warning}"
    typography: "{typography.label}"
  status-fault:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.fault}"
    typography: "{typography.label}"
  status-off:
    backgroundColor: "{colors.panel}"
    textColor: "{colors.muted}"
    typography: "{typography.label}"
  button-default:
    backgroundColor: "{colors.panel-raised}"
    textColor: "{colors.text}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "0.48rem 0.78rem"
  button-primary:
    backgroundColor: "{colors.sea-glass}"
    textColor: "{colors.ink}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "0.48rem 0.78rem"
  button-danger:
    backgroundColor: "{colors.panel-raised}"
    textColor: "{colors.fault}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "0.48rem 0.78rem"
  field:
    backgroundColor: "{colors.ink}"
    textColor: "{colors.text}"
    typography: "{typography.body}"
    rounded: "{rounded.none}"
    padding: "0.65rem 0.75rem"
  operation-rail:
    backgroundColor: "{colors.panel-active}"
    textColor: "{colors.text}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "0.22rem 1rem"
---

# Design System: Arrman Engineering Watch

## Overview

**Creative North Star: "The Engineering Watch"**

The interface behaves like a shipboard engineering watch: dark equipment fields,
plain steel rules, one clear focus signal, and operational state written out in
full. It is information-dense without becoming frantic. The hierarchy starts
with host condition, moves into the service watch and selected interlocks, and
ends in a persistent command channel.

The terminal remains the densest expression of the system. The browser extends
it as **The Sectional Watch Station** (concept seed `790992d1`): a host and
command rail, fleet ledger, selected-service workbench, recovery bench, and
shared event strip. Both surfaces derive character from alignment, contrast,
square instrument framing, and live operational language rather than ornament
or simulated physical materials.

**Key Characteristics:**

- Ink-dark, flat instrument surfaces with steel dividers.
- Sea-glass focus reserved for navigation and affirmative action.
- Explicit state words backed by a restrained semantic palette.
- A persistent activity channel that makes long operations observable.
- Structural reflow for wide, medium, and compact terminal and browser viewports.

**The Shared Watch Rule.** TUI and web may use different density, but they must
show the same service state, operation ownership, dependency truth, and recovery
consequences in the same semantic language.

## Colors

The palette separates structural neutrals, focus, and operational severity. Ink
and panel establish depth; steel and muted text recede; sea-glass marks current
focus; ready, warning, fault, and information carry semantic signals.

**The Signal Before Hue Rule.** Every semantic color accompanies a text label
such as `[READY]`, `[WAIT]`, `[FAULT]`, or `[OFF]`; color never carries state by
itself.

**The Rare Focus Rule.** Sea-glass belongs to the active tab, selected control,
or affirmative confirmation—not to general decoration.

## Typography

The user's terminal controls its installed typeface. The browser uses the native
system monospace stack so service names, ports, logs, and status columns retain
the same operational rhythm without requiring a font download. Both surfaces
rely on normal body weight and bold labels rather than a decorative display face.

### Hierarchy

- **Display** (700, fluid 1.6–3.1rem, 1.12): authentication-gate statement only.
- **Headline** (700, fluid 1.15–1.65rem, 1.12): primary browser section headings.
- **Title** (700, fluid 1–1.25rem, 1.12): workbench and subsection titles.
- **Body** (400, 0.875rem, 1.5): operational copy and readouts, capped near 72 characters where prose is continuous.
- **Label** (700, 0.74rem, 0.04em tracking): uppercase controls, metrics, tabs, and state labels.

Titles and field labels use bold uppercase for fast scanning. Service names,
roles, endpoints, log output, and explanatory copy stay at normal terminal
weight. Compact state badges use bold bracketed words with stable widths.

**The Cell Grid Rule.** Alignment is semantic: labels, values, status badges, and
table columns must remain legible in terminal cells before any decorative copy is
added.

## Layout

The first rows are a fixed host rail: product title, CPU/load/memory/disk/uptime,
watch status, and five operational tabs. A two-row action footer remains anchored at
the bottom. The service surface reserves a live command channel beneath its main
content, growing from three to seven rows as terminal height permits.

At 120 columns and above, the service table and inspector sit side by side with a
one-cell seam and the table receives the greater share. From 88 through 119
columns, the same topology remains but the endpoint column is removed. Below 88
columns, the selected channel stacks beneath the table and Enter opens full
detail. The Recovery surface splits its bundle ledger and inspection bench at
100 columns and stacks them below that width. Below 40 by 16 cells, the interface
presents a resize instruction instead of a broken layout.

The browser uses a sectional watch station rather than a grid of cards. A sticky
host and command rail leads into the fleet ledger and selected-service workbench,
with the shared event strip following the active view. At 1120 pixels the
endpoint column yields space and recovery becomes one column; at 860 pixels the
sticky rail releases, the workbench stacks below the ledger, and integration and
diagnostic rows become vertical; at 620 pixels host metrics, headings, actions,
and event rows reflow for one-handed phone use without horizontal page scrolling.

**The Watch Order Rule.** Host condition comes first, selectable services second,
interlocks third, and command evidence last. Responsive modes may reflow that
order but never invert it.

## Elevation & Depth

The system has no cast shadows, blur, gradients, or fake material effects. Depth
comes from four tonal layers—ink, panel, raised panel, and active panel—plus plain
steel rules. The browser's inset three-pixel selected-row signal is a structural
rule, not elevation. Terminal modal operations clear and replace the bounded
center region. The browser uses a native square dialog with a flat ink backdrop.
Both exist to protect a consequential confirmation, not as decorative elevation.

**The Flat Instrument Rule.** Use tonal layering and structural rules for
hierarchy; never add cast shadows, glow, blur, or bevelled equipment.

## Shapes

All frames have square corners and plain rules. Statuses are bracketed labels,
not pills. Selection uses a one-cell terminal marker or a three-pixel browser
signal with an active-panel field. This geometry remains consistent across
tables, inspectors, activity, diagnostics, integrations, recovery, help, and
confirmations.

## Components

### Host rail

Keeps global condition visible. Metric names are bold and values carry semantic
color only when thresholds require it; disk pressure at or above 90 percent also
changes the label to `DISK!`.

### Service watch

Rows lead with a textual state badge, then service, role, issue/interlock signal,
and endpoint when width permits. The selected row uses the active-panel field and
a one-cell cursor. Movement operates on cached state and never waits for Docker.

### Interlock inspector

Shows the selected service's purpose, runtime state, endpoint, image, ports,
mounts, healthcheck declaration, dependency readiness, and current log finding.
On compact layouts it becomes a three-line selected channel with full detail on
Enter.

### Tabs and command footer

Tabs are numbered operational surfaces, with sea-glass and bold weight marking
the active surface. The footer shows only keys relevant to the current surface
and shortens at constrained widths.

### Buttons and fields

Buttons are square, compact, uppercase instruments. Default actions use the
raised-panel field, affirmative actions invert to sea-glass on ink, and dangerous
actions retain an explicit fault-colored label and border. Fields sit on ink
with a steel rule; hover raises contrast and focus uses the brighter focus cyan.
All mutation controls become visibly disabled while another protected operation
owns the shared permit.

### Activity channel

Every background operation emits timestamped progress beneath the work surface;
errors use the fault signal while normal output remains body text. The dedicated
Activity tab preserves the longer history.

### Browser authentication and command rail

The authentication gate is a focused instrument, not a branded landing page. It
names the token source, permits reveal/hide, and exposes failures inline. Once
authenticated, the sticky rail carries identity, connection/sample age, host
metrics, primary views, the active operation, and session exit. It never exposes
the bearer token in copy, links, or the activity stream.

### Recovery bench

Recurring backup health states its interval, retention, storage path, last
attempt, last success, next due time, overdue or failed state, and recovery guidance. Backups
are a ledger of timestamp, kind, consistency, size, and protected actions.
Upload and HTTPS import share one inspection result before restore is offered.
Private-data, exclusions, checksum, and online-consistency warnings remain
adjacent to the confirmation; they are never relegated to a toast. Download
links are short-lived one-time tickets rather than bearer credentials embedded
in URLs.

### Confirmation and port fields

Protected operations use centered square dialogs with an explicit title, exact
affected scope, affirmative and cancel keys, and plain-language recovery detail.
The port field shows the current binding and states that validation and backup
occur before recreation.

## Do's and Don'ts

### Do:

- **Do** preserve explicit state labels wherever semantic colors appear.
- **Do** show exact dependency scope before enabling or disabling a service.
- **Do** keep long-running output in the activity channel while navigation stays live.
- **Do** reflow structurally at the established terminal breakpoints.
- **Do** keep host pressure visible, including the textual `DISK!` threshold.
- **Do** preserve the ledger/workbench/event-strip order when the browser reflows.
- **Do** show the active operation and its progress in every connected surface.
- **Do** keep setup-bundle sensitivity and consistency warnings in the recovery bench.
- **Do** surface recurring-backup failures with last attempt, last success, next due time, storage path, and recovery guidance.

### Don't:

- **Don't** replace square instrument fields with rounded cards or soft shadows.
- **Don't** use color alone to distinguish ready, waiting, faulted, or disabled state.
- **Don't** let Docker output escape above or overwrite the terminal interface.
- **Don't** hide command progress inside a blocking modal.
- **Don't** truncate the affected service scope in a consequential confirmation.
- **Don't** turn the browser surface into a dashboard of interchangeable rounded cards.
- **Don't** place bearer tokens, API keys, or setup secrets in URLs or activity copy.
- **Don't** allow a second mutation control to look actionable while the shared operation permit is busy.
