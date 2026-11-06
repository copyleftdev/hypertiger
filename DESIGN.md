---
name: hypertiger landing page
description: A cobalt film plate on which every order is a particle track from one real simulator run.
colors:
  plate-cobalt: "#0a1c8a"
  plate-cobalt-deep: "#081569"
  track-paper: "#f4f1e6"
  doubt-yellow: "#ffc800"
  never-red: "#ff3d2e"
  caption-tint: "#cfd2ee"
  hairline-rule: "rgb(244 241 230 / 0.28)"
typography:
  display:
    fontFamily: "League Gothic, Arial Narrow, sans-serif"
    fontSize: "min(16.2vw, 23vh)"
    fontWeight: 400
    lineHeight: 0.9
    letterSpacing: "-0.008em"
  display-close:
    fontFamily: "League Gothic, Arial Narrow, sans-serif"
    fontSize: "min(15.5vw, 34vh)"
    fontWeight: 400
    lineHeight: 0.82
    letterSpacing: "-0.008em"
  headline:
    fontFamily: "League Gothic, Arial Narrow, sans-serif"
    fontSize: "clamp(48px, 7.2vw, 108px)"
    fontWeight: 400
    lineHeight: 0.92
  title:
    fontFamily: "League Gothic, Arial Narrow, sans-serif"
    fontSize: "clamp(26px, 2.7vw, 38px)"
    fontWeight: 400
    lineHeight: 1.02
  action:
    fontFamily: "League Gothic, Arial Narrow, sans-serif"
    fontSize: "22px"
    fontWeight: 400
    lineHeight: 1
    letterSpacing: "0.02em"
  lede:
    fontFamily: "Libre Franklin, Franklin Gothic Book, sans-serif"
    fontSize: "clamp(17px, 1.4vw, 20px)"
    fontWeight: 400
    lineHeight: 1.5
  answer:
    fontFamily: "Libre Franklin, Franklin Gothic Book, sans-serif"
    fontSize: "clamp(15px, 1.42vw, 21px)"
    fontWeight: 600
    lineHeight: 1.32
  body:
    fontFamily: "Libre Franklin, Franklin Gothic Book, sans-serif"
    fontSize: "17px"
    fontWeight: 400
    lineHeight: 1.55
  label:
    fontFamily: "Fragment Mono, ui-monospace, monospace"
    fontSize: "12px"
    fontWeight: 400
    lineHeight: 1.35
  code:
    fontFamily: "Fragment Mono, ui-monospace, monospace"
    fontSize: "clamp(12.5px, 1.04vw, 15px)"
    fontWeight: 400
    lineHeight: 1.62
rounded:
  none: "0"
spacing:
  gap-tight: "6px"
  gap-step: "14px"
  gap-block: "22px"
  plate-edge: "clamp(16px, 2.2vw, 32px)"
  section-block: "clamp(70px, 12vh, 140px)"
  log-block: "clamp(70px, 11vh, 130px)"
components:
  copy-button:
    backgroundColor: "{colors.doubt-yellow}"
    textColor: "{colors.plate-cobalt}"
    typography: "{typography.action}"
    rounded: "{rounded.none}"
    padding: "10px 14px"
  copy-button-hover:
    backgroundColor: "{colors.track-paper}"
    textColor: "{colors.plate-cobalt}"
  transport-button:
    backgroundColor: "{colors.plate-cobalt}"
    textColor: "{colors.track-paper}"
    rounded: "{rounded.none}"
    padding: "0 11px"
    height: "42px"
  transport-button-hover:
    backgroundColor: "{colors.track-paper}"
    textColor: "{colors.plate-cobalt}"
  film-data-box:
    backgroundColor: "{colors.plate-cobalt}"
    textColor: "{colors.track-paper}"
    rounded: "{rounded.none}"
    padding: "8px 10px"
  track-tag:
    backgroundColor: "{colors.plate-cobalt}"
    textColor: "{colors.doubt-yellow}"
    typography: "{typography.label}"
    rounded: "{rounded.none}"
    padding: "2px 6px"
  film-strip:
    backgroundColor: "{colors.plate-cobalt-deep}"
    rounded: "{rounded.none}"
    height: "42px"
  scan-log:
    backgroundColor: "{colors.track-paper}"
    textColor: "{colors.plate-cobalt}"
    rounded: "{rounded.none}"
---

# Design System: hypertiger landing page

**Scope:** this file covers the visual system of the landing page in `site/`. The library's engineering design is in `docs/DESIGN.md`, which has no authority over visual decisions.

## Overview

**Creative North Star: "The Bubble Chamber"**

The page is one photographic plate from a particle detector. Cobalt film fills the viewport and stays fixed behind everything, and each order in one real simulator run is a track of paper-coloured bubbles curling across it. When a request times out the track forks into both worlds the network could have left, and when the order settles one of those worlds collapses. Everything else on the page is what a physicist would find on the film: etched fiducial crosses, solid data boxes, a perforated strip along the bottom edge, and a single sheet of paper for the written record.

The density is split on purpose. Poster type at the scale of the viewport sits against tiny, exact instrument readouts, with very little in between. The tone is that of a lab record rather than a product page: the plate reports what happened, and the scan log reports what has not happened yet in the same marks.

The direction contract rejects the category default for a developer library, a hero, a code block and three feature columns on a dark ground. A code block may still appear as an exhibit, and it does. It is the formula that is refused.

**Key Characteristics:**
- A single drenched colour field, with one paper inversion.
- State carried by line form first and colour second.
- League Gothic at poster scale, Libre Franklin for reading, Fragment Mono for instruments.
- Square, flat furniture on a plate that has real depth.
- A plate that is a pure function of time, which the visitor can scrub in either direction.

## Colors

A saturated cobalt ground with paper for almost everything on it, and two signal colours that mean something on the film.

### Primary
- **Plate Cobalt** (#0a1c8a): The film itself, the page background, the fill of every data box, and the ink on the paper sheet. Paper on cobalt reads at 12:1 in both directions.

### Secondary
- **Doubt Yellow** (#ffc800): On the plate and the film strip, an order in doubt, and in the bound chart, the range the engine counted. Off the film it is the page's one accent: the copy button, the alpha warning, link hover, the focus ring, selection, the scrollbar thumb, code comments, code in headlines and the reader headings.

### Tertiary
- **Never Red** (#ff3d2e): An order proven never to have landed. It appears on settled track stubs, on film strip ticks and on the bracket of a never-landed tag, and nowhere else. It reaches 3.85:1 on cobalt, which is enough for marks and not for text.

### Neutral
- **Track Paper** (#f4f1e6): Bubbles, headlines and reading text on the plate, and the ground of the scan log. Solid paper tracks are orders that landed or are simply in flight.
- **Caption Tint** (#cfd2ee): Secondary text on cobalt, such as notes, counter labels, chart ticks and the footer.
- **Plate Cobalt Deep** (#081569): The film strip's bed and the scrollbar track.
- **Hairline Rule** (rgb(244 241 230 / 0.28)): Dividers in the counters and tables, chart grid and axes, the transport button border and the leaders of docked tags. On paper the same hairline is cobalt at 24%.

### Named Rules

**The Line Form First Rule.** A state must read without its colour: solid is seen or landed, dashed is in doubt, and a stub barred at its end never happened. The scan log proves it by drawing all three in one cobalt ink.

**The One Inversion Rule.** The page inverts to paper exactly once, for the record of what has and has not been seen. Selection, focus and link hover flip to cobalt inside it. A second paper section would spend the only inversion.

**The Red Marks Only Rule.** Never Red never sets text. A never-landed tag keeps its words in paper and gives red to its bracket.

The palette is not only in the stylesheet. The shaders, the flat fallback and the strip's canvas repeat these values as literals, so a colour change has to reach `site/style.css`, `site/js/plate.js`, `site/js/tracks.js`, `site/js/main.js`, the page's theme colour, the favicon and the web manifest together.

## Typography

**Display Font:** League Gothic (with Arial Narrow as the load fallback)
**Body Font:** Libre Franklin at 400 and 600 (with Franklin Gothic Book)
**Label/Mono Font:** Fragment Mono (with ui-monospace)

**Character:** A condensed gothic as tall as the viewport, in the voice of a wood-type poster, against a plain American grotesque for reading and a mono that belongs to the instruments. All faces are self-hosted, and nothing is uppercased by CSS.

### Hierarchy
- **Display** (400, min(16.2vw, 23vh), 0.9): The opening question, on three staggered lines that fill the width. It tightens to min(20.5vw, 12vh) and wraps at the narrow breakpoint.
- **Display Close** (400, min(15.5vw, 34vh), 0.82): The closing line, at 25vw on narrow screens. Both display lines are lowercase, like the name.
- **Headline** (400, clamp(48px, 7.2vw, 108px), 0.92): Section headings, in sentence case. The pinned story's title steps down to clamp(40px, 5.2vw, 76px) to leave the plate open.
- **Title** (400, clamp(26px, 2.7vw, 38px), 1.02): Story captions, reader headings and scan log claims, which sit within a few pixels of this size.
- **Action** (400, 22px, 1, 0.02em): The copy button. A button in the display face, so it reads as part of the poster.
- **Answer** (600, clamp(15px, 1.42vw, 21px), 1.32): The short statement set beside the headline, and the only sizeable semibold on the page.
- **Lede** (400, clamp(17px, 1.4vw, 20px), 1.5): Section introductions, to 66ch.
- **Body** (400, 17px, 1.55): Reading text, to 62ch. Notes and fine print use the same face at 13 to 14px in Caption Tint.
- **Label** (400, 12px, 1.35): Track tags and the scan readout. Counters and table cells run 12.5 to 13px and chart ticks 11.5px, always with tabular figures.
- **Code** (400, clamp(12.5px, 1.04vw, 15px), 1.62): Code exhibits. Inline code runs at 0.92em inside its sentence.

### Named Rules

**The Instrument Voice Rule.** Fragment Mono is for what the machine says: code, counters, clocks, readouts, ticks, table headers and the transport controls beside the film. A person's sentence, including evidence with numbers in it, stays in Libre Franklin with tabular figures.

**The Rag Rule.** Opening lines are measured, and the answer and the command sit in the rags of the second and third lines, each starting 22px past the end of its line. On narrow screens the rag is too short to hold anything, so they stack beneath it.

## Layout

The plate is fixed to the viewport, and the sections scroll over it as film passes a gate. Sections run edge to edge, with an inner edge of clamp(16px, 2.2vw, 32px). Reading sections hold a 1180px sheet, and the scan log a 1120px one, with block padding of clamp(70px, 12vh, 140px) and clamp(70px, 11vh, 130px).

The first viewport is a full-height grid: open plate above, type in the lower half, film strip along the bottom edge, and data boxes pinned to the top corners. The story pins for 720vh while scroll drives simulated time and the camera together, with its caption held at the lower left and the framing pushed up and to the right to clear it. The reader section splits into two equal columns with a clamp(24px, 5vw, 80px) gutter.

There is one breakpoint, at 860px. Below it the hero stacks, columns collapse, the scan log becomes a two-column list of mark and claim, and the top corner boxes share the width at under half each. Spacing inside boxes runs tight, 2 to 14px, so it reads like film annotation rather than cards.

## Elevation & Depth

CSS adds no shadow at all. Furniture is flat, and order comes from stacking above the plate. The depth lives inside the plate, where a perspective camera looks through a chamber: fiducial crosses are etched at two depths, so they shift against each other when the camera moves, and pointer position tilts the view slightly. The ground is shader film with uneven development, scratches and grain.

### Named Rules

**The Box Or Dim Rule.** Small text never shares the live plate with tracks. On a running plate it sits in a solid cobalt film box; where a whole section reads directly on the film, the plate steps back behind it, dropping its bubbles to 7%. Only poster type stands on the live plate unprotected.

**The Pure Function Rule.** The picture is computed from the time value alone, so scrubbing backward costs the same as playing and no state can drift from the run it shows. Timing, side, size and outcome come from the run data, and only a track's placement and the exact shape of its curl are arrangement. The film's own furniture, its ground, scratches, grain and fiducials, is the only thing on the plate that does not come from the run.

## Shapes

Every rectangle is square: boxes, buttons, swatches, the scrub handle and the film strip all have 0 radius. Circles belong to the physics. Bubbles are round, tracks curl into spirals as particles lose energy, and bursting bubbles swell as they fade. Marks are 1px hairlines and 3px heavy rules, with the heavy rule opening a table or code exhibit. The disclosure chevron is two CSS borders and the transport icons are inline SVG.

## Components

### Buttons
- **Shape:** Square-cornered (0).
- **Primary (Copy):** Solid Doubt Yellow with cobalt display type, 78px minimum width. It says what happened in place, "Copied" or "Press Ctrl+C", for 2.4 seconds, and a live region repeats it.
- **Secondary (Transport):** Cobalt with a hairline border and paper content, 42px tall and at least 42px wide, in mono or with an inline SVG icon.
- **Hover / Focus:** Both fill with paper over 180ms, and Copy presses down 1px when active. Focus everywhere is a 2px yellow outline offset 3px, which turns cobalt on paper.

### Navigation
There is no navigation bar. The name sits in the top-left box as a mark in the display face, and links appear in flat rows where they are needed. Links are underlined at 1px with a 0.22em offset, and turn yellow on hover. On paper they stay cobalt and thicken to 3px.

### Film Plate
The signature. A full-viewport WebGL canvas behind all content, replaying the run at half speed. A bubble nucleates with a brief swell, then fades with age. A dashed yellow fork means doubt; on settlement the losing world bursts over 0.32 simulated seconds, unevenly, and the survivor goes solid paper or stays as a red stub barred at the deadline. Pixel ratio caps at 1.75 and drops to 1 on a weak GPU, which loses resolution rather than frames.

### Film Data Box
A solid cobalt panel with no border or shadow, used for anything that has to stay legible over tracks. Its inset is small, as low as 2px on one-line statements. The top-right box holds the run clock and counters, ruled by hairlines.

### Track Tag
A compact mono label following its fork: state in yellow or paper, size in tint, with a one-pixel bracket connecting it to the track. The bracket takes the state's colour. Tags cap at three, one on small screens and two in the story, and they dock with a hairline leader when a narrow frame would cover the track.

### Film Strip
The scrubber along the plate's edge: a 42px deep-cobalt bed with paper perforations (7 by 4px every 19px, top and bottom), yellow bars for each period of doubt, and a closing tick for each settlement, full height in paper for landed and short in red for never. A 3px paper bar in an 11px translucent handle marks now.

### Scan Log
The one paper section, a table opening on a 3px cobalt rule. Each row carries a 64px mark, a claim in the display face and evidence in Libre Franklin, separated by cobalt hairlines at 24%.

### Bound Chart
Flat areas in paper and yellow on dimmed film, on a dotted hairline grid with mono ticks and the coin name in display type. It reads on hover or arrow keys into a live text line, and offers its samples as a disclosed table.

### Code Exhibit
Code with no panel behind it, framed by a 3px paper rule above and a 1px rule below, with comments in yellow.

## Do's and Don'ts

### Do:
- **Do** drench the page in Plate Cobalt (#0a1c8a) and let the film show through everywhere except the scan log.
- **Do** give every state a line form before a colour: solid, dashed, or a stub barred at its end.
- **Do** put small text on the live plate in a solid film box, or dim the plate behind the whole section.
- **Do** derive every track and state mark from the run data, and keep invented values to a track's placement and curl.
- **Do** ease interface state with cubic-bezier(0.16, 1, 0.3, 1), at 180ms for controls and 420 to 520ms for captions.
- **Do** give reduced motion a real frame: a still of the plate with both story orders in doubt, no autoplay, no pinning and no tilt, with the story set out as a plain list.
- **Do** say what happened in the control that did it, as the copy button does.

### Don't:
- **Don't** assemble the category default of a hero, a code block and three feature columns on a dark ground.
- **Don't** add a CSS shadow or a corner radius to furniture. Round forms are for bubbles and tracks.
- **Don't** set text in Never Red (#ff3d2e). It is 3.85:1 on cobalt.
- **Don't** add a second paper section. The inversion happens once.
- **Don't** let state rest on hue alone, on the plate or off it.
- **Don't** replace the film ground with a CSS gradient. Its variation is development, scratches and grain from the shader.
- **Don't** set a person's sentences in Fragment Mono.
- **Don't** draw a track or state mark that does not come from the run. Only the film's ground and fiducials are exempt.
