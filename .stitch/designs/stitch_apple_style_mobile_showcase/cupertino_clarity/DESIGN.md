---
name: Cupertino Clarity
colors:
  surface: '#fcf8fb'
  surface-dim: '#dcd9dc'
  surface-bright: '#fcf8fb'
  surface-container-lowest: '#ffffff'
  surface-container-low: '#f6f3f5'
  surface-container: '#f0edef'
  surface-container-high: '#eae7ea'
  surface-container-highest: '#e4e2e4'
  on-surface: '#1b1b1d'
  on-surface-variant: '#414755'
  inverse-surface: '#303032'
  inverse-on-surface: '#f3f0f2'
  outline: '#717786'
  outline-variant: '#c1c6d7'
  surface-tint: '#005bc1'
  primary: '#0058bc'
  on-primary: '#ffffff'
  primary-container: '#0070eb'
  on-primary-container: '#fefcff'
  inverse-primary: '#adc6ff'
  secondary: '#4c4aca'
  on-secondary: '#ffffff'
  secondary-container: '#6664e4'
  on-secondary-container: '#fffbff'
  tertiary: '#9e3d00'
  on-tertiary: '#ffffff'
  tertiary-container: '#c64f00'
  on-tertiary-container: '#fffbff'
  error: '#ba1a1a'
  on-error: '#ffffff'
  error-container: '#ffdad6'
  on-error-container: '#93000a'
  primary-fixed: '#d8e2ff'
  primary-fixed-dim: '#adc6ff'
  on-primary-fixed: '#001a41'
  on-primary-fixed-variant: '#004493'
  secondary-fixed: '#e2dfff'
  secondary-fixed-dim: '#c2c1ff'
  on-secondary-fixed: '#0c006a'
  on-secondary-fixed-variant: '#3631b4'
  tertiary-fixed: '#ffdbcc'
  tertiary-fixed-dim: '#ffb595'
  on-tertiary-fixed: '#351000'
  on-tertiary-fixed-variant: '#7c2e00'
  background: '#fcf8fb'
  on-background: '#1b1b1d'
  surface-variant: '#e4e2e4'
typography:
  display-lg:
    fontFamily: Inter
    fontSize: 40px
    fontWeight: '700'
    lineHeight: 48px
    letterSpacing: -0.025em
  display-lg-mobile:
    fontFamily: Inter
    fontSize: 32px
    fontWeight: '700'
    lineHeight: 38px
    letterSpacing: -0.02em
  headline-lg:
    fontFamily: Inter
    fontSize: 28px
    fontWeight: '600'
    lineHeight: 34px
    letterSpacing: -0.02em
  headline-md:
    fontFamily: Inter
    fontSize: 22px
    fontWeight: '600'
    lineHeight: 28px
    letterSpacing: -0.015em
  headline-sm:
    fontFamily: Inter
    fontSize: 18px
    fontWeight: '600'
    lineHeight: 24px
    letterSpacing: -0.01em
  body-lg:
    fontFamily: Inter
    fontSize: 17px
    fontWeight: '400'
    lineHeight: 24px
    letterSpacing: -0.01em
  body-md:
    fontFamily: Inter
    fontSize: 15px
    fontWeight: '400'
    lineHeight: 22px
    letterSpacing: -0.005em
  body-sm:
    fontFamily: Inter
    fontSize: 13px
    fontWeight: '400'
    lineHeight: 18px
    letterSpacing: 0em
  label-md:
    fontFamily: Inter
    fontSize: 14px
    fontWeight: '500'
    lineHeight: 18px
    letterSpacing: -0.005em
  label-sm:
    fontFamily: Inter
    fontSize: 12px
    fontWeight: '500'
    lineHeight: 16px
    letterSpacing: 0.01em
  caption:
    fontFamily: Inter
    fontSize: 11px
    fontWeight: '400'
    lineHeight: 14px
    letterSpacing: 0.02em
rounded:
  sm: 0.25rem
  DEFAULT: 0.5rem
  md: 0.75rem
  lg: 1rem
  xl: 1.5rem
  full: 9999px
spacing:
  gutter: 1rem
  gutter-md: 1.5rem
  margin: 1rem
  margin-md: 2rem
  margin-lg: 3rem
  space-xs: 0.25rem
  space-sm: 0.5rem
  space-md: 1rem
  space-lg: 1.5rem
  space-xl: 2.5rem
---

## Brand & Style

This design system embraces an Apple-inspired minimalist aesthetic defined by quiet confidence, pristine clarity, and functional elegance. The interface acts as a silent canvas that recedes into the background, allowing content, typography, and purposeful data to take center stage.

- **Design Philosophy:** Human Interface-driven modern minimalism. Structural clarity over visual noise, crisp boundaries, rhythmic whitespace, and zero superfluous decoration.
- **Audience:** Demanding professionals and consumers seeking clarity, focus, and intuitive utility without sensory exhaustion.
- **Emotional Resonance:** Seamless, deliberate, restrained, and extraordinarily light.

## Colors

The palette is strictly calibrated for maximum visual calm and high legibility in a pure light mode environment.

- **Canvas & Backgrounds:** Pure white (`#FFFFFF`) serves as elevated cards and content sheets, while Canvas Secondary (`#F8F9FA`) and Canvas Tertiary (`#F2F4F7`) establish spatial grouping without harsh breaks.
- **Accents:** A single refined iOS blue (`#007AFF`) handles direct actions, primary CTAs, links, and active selection indicators. An optional restrained indigo (`#5856D6`) is reserved strictly for specialized status indicators. Rainbow gradients, aggressive saturation, and arbitrary decorative colors are strictly prohibited.
- **Hairlines & Dividers:** Single-pixel neutral hairline borders (`#E5E7EB`) outline surfaces and separate structural elements.
- **Text & Foreground:** Structured neutral hierarchy using primary ink (`#1C1C1E`), secondary metadata (`#636366`), and subtle tertiary captions (`#8E8E93`).

## Typography

The typographic engine implements a modern, neutral neo-grotesque system that emulates Apple's SF Pro Text and Display families. 

- **Font Stack:** Defaults to `Inter, -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", "Segoe UI", sans-serif`.
- **Numerical Treatment:** All tabular data, metrics, pricing, timestamps, and charts must enable tabular figures via `font-feature-settings: "tnum" 1` to guarantee vertical column alignment.
- **Optical Tracking:** Tighter tracking on displays and large headings ensures punchy editorial cohesion; slightly relaxed tracking on small labels and captions retains extreme legibility.

## Layout & Spacing

Layouts follow an organized, fluid grid structure structured upon 8-point base intervals with 4-point micro-adjustments.

- **Breakpoints:**
  - Mobile (`< 768px`): 4 columns, `margin: 1rem`, `gutter: 1rem`. Full-width inset cards.
  - Tablet (`768px - 1024px`): 8 columns, `margin-md: 2rem`, `gutter: 1.5rem`.
  - Desktop (`> 1024px`): 12 columns, max-width constrained to `1200px`, `margin-lg: 3rem`, `gutter-md: 1.5rem`.
- **Structure:** Content surfaces sit on an open `#F8F9FA` or `#F2F4F7` background canvas. Sections maintain ample vertical breathing room using `space-xl` intervals, avoiding cramped arrangements.

## Elevation & Depth

Depth is established primarily through translucent material layers and ultra-delicate hairlines, foregoing dense or muddy drop shadows.

- **Materials & Blurs:** Navigation headers, floating toolbars, and sticky controls employ frosted glass materials: `background: rgba(255, 255, 255, 0.72)` coupled with `backdrop-filter: blur(20px) saturate(180%)`.
- **Hairlines:** Structural surfaces, cards, and modal sheets are bordered with a crisp `1px solid #E5E7EB` (or `rgba(0, 0, 0, 0.08)` on high-density displays).
- **Shadows:** Kept exceptionally soft and diffused.
  - *Standard Surface:* `0 1px 3px rgba(0, 0, 0, 0.04), 0 1px 2px rgba(0, 0, 0, 0.02)`.
  - *Modal / Popover Sheet:* `0 12px 32px rgba(0, 0, 0, 0.08), 0 2px 6px rgba(0, 0, 0, 0.04)`.

## Shapes

The geometric identity mirrors Apple's continuous squircle and rounded geometry.

- **Level 2 Scaling:**
  - Controls, badges, chips, and small text fields: `0.5rem` (8px).
  - Cards, content tiles, and grouped table containers: `1rem` (16px).
  - Floating action modals, sheets, and dialog surfaces: `1.25rem` to `1.5rem` (20px - 24px).
  - Full pill corners (`9999px`) are restricted solely to segmented controls, toggles, and compact inline status pills.

## Components

- **Buttons:**
  - *Primary:* `#007AFF` solid background, pure white text, subtle hover reduction to `#0066D6`, active press transform of `scale(0.98)`.
  - *Secondary / Tinted:* `rgba(0, 122, 255, 0.08)` fill with `#007AFF` typography.
  - *Tertiary / Plain:* Transparent fill with `#007AFF` or `#1C1C1E` label, turning subtle gray on hover (`#F2F4F7`).
- **Cards & Grouped Lists:**
  - White surface (`#FFFFFF`) with 16px corner radius and hairline border (`1px solid #E5E7EB`).
  - Table-view lists feature edge-to-edge row items with inset 0.5px dividers that indent to match text alignment, omitting the line on the terminal row.
- **Chips & Segmented Controls:**
  - Segmented controls feature a unified container filled with `#F2F4F7` housing pill or softly rounded buttons. The active segment is white (`#FFFFFF`) with a hairline border and an ambient micro-shadow (`0 1px 3px rgba(0,0,0,0.06)`).
- **Form Inputs:**
  - Clean white or `#F8F9FA` backgrounds with hairline border (`#E5E7EB`), transitioning to a crisp 1px active ring in `#007AFF` with a subtle focus glow (`0 0 0 3px rgba(0, 122, 255, 0.15)`).
- **Checkboxes & Radios:**
  - Hairline border unselected; solid `#007AFF` fill with a centered crisp white checkmark or inner dot when active. Smooth spring-like scale transitions on state change.
- **Glass Headers & Navigation Bars:**
  - Translucent sticky bars with hairline border bottom (`#E5E7EB`), preserving seamless backdrop legibility as list items scroll beneath.