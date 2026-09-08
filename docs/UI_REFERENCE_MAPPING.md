# UI Reference Mapping

`uiuxreferences/` is the visual source of truth. Every supplied screen is
1672×941 px; visual review captures the same viewport before a release is
accepted. Native Slint implementation is required—no browser or web-runtime
substitution.

| Reference | DevCore surface |
| --- | --- |
| `file_000000005fb882118c96a70260dea2e2.png` | Shell overview, dark |
| `file_00000000683082118e6c3d4104cc138a.png` | Shell overview, light |
| `file_0000000019d48211a5d11d7238919cbd.png` | Shell project hub and Studio launcher |
| `file_000000001eec8211bfb268e7672525ba.png` | System resource dashboard, dark |
| `file_0000000080688211a74483cd0a756a87.png` | Studio build and test centre, dark |
| `file_00000000deac82118095d40f05c36ac2.png` | Studio editor workspace, dark |
| `file_00000000bb1882119b8fef3fe03cdede.png` | Studio environment manager, light |
| `file_00000000d2708208a30c24d3ac0736b5.png` | System hardware, security, and updates, light |
| `file_00000000913c8211a583fc8ef909b2b0.png` | Onboarding greeter |
| `file_000000001f2c821183f455357c6edf65.png` | Installer and first-boot wizard language |

## Shared system

- Inter, with Noto Sans fallback; titles 28/34 px, body 13–14 px, metadata
  11–12 px.
- Header 60 px; outer gutter 24 px; standard gap 16 px; cards are 14–16 px
  radius with a one-pixel low-contrast border.
- Light canvas `#F7FAFC`, text `#101828`, muted `#667085`, border `#E5EAF0`,
  blue `#1683FF`, success `#24C98A`, warning `#F5AF26`, danger `#EF5350`.
- Dark canvas runs from `#030B14` to `#071827`; surfaces use `#0B1622` and
  `#101B28`; primary blue `#2387FF`; text `#F7F9FC`; muted `#9AA8BB`.
- Rails use icon-plus-label navigation and a blue indicator/tinted active
  pill. Tabs use a two-pixel blue underline. Dark shell and greeter retain the
  low-contrast blue/teal wave treatment rather than a flat background.

## Native installer acceptance

The installer is a normal live-desktop window; it never replaces the desktop
or terminal. Its visual frame follows the supplied nine-step wizard: top
logo/stepper, illustration/form split body, and fixed bottom action bar. The
MVP maps those states to Welcome, Disk, Locale/keyboard, Account, Password,
Profile, Destructive confirmation, Progress, and Finish.

Disk review displays the stable ID, capacity, GPT/1 GiB ESP/ext4 plan, and a
destructive warning. The install control remains disabled until the exact
`ERASE <disk suffix>` phrase is supplied. Progress keeps the same wizard
chrome, offers cancellation only before storage writes, and presents a clear
retry/diagnostic state on post-erasure failure.

Release screenshots must show the target screen/theme at 1672×941, plus no
clipping at 1280×720 and 200% text scale. Verify 60±2 px header height,
24±2 px gutter, 14–16 px radii, 16±2 px gaps, 4.5:1 minimum contrast, and a
visible two-pixel keyboard focus ring. Screenshot comparison does not replace
keyboard or screen-reader testing.
