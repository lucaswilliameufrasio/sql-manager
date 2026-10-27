# Visual QA findings — 2026-10-27

Status: recorded, not fixed. These are local findings, not published tracker issues.

## Reproduction environment

- Revision: `ac071a6db913c573faa48a522f92dbef1b67fa99`.
- Linux via SSH; eframe 0.36.2, glow, exclusive Xvfb/Openbox display.
- Mesa 26.2.5, llvmpipe (LLVM 23.1.3), OpenGL 4.6; software rendering.
- Fresh, isolated HOME/XDG storage, synthetic inputs, no saved real connections.
- Window 1360×860; minimum 1024×680 also exercised with scrolling.
- Ten real PNGs were captured and inspected. Screenshots and action logs remain
  in the operator's private `sql-manager-validation` artifact directory, not in
  this repository. Image filenames below identify those retained artifacts.

## VQA-001 — Missing glyphs in icon-like controls

**Priority:** medium. **State:** open; observed in the environment above.

**Reproduce:** launch with a fresh profile. Inspect the header mark, New/Create
connection buttons, empty-state symbol and Backup indicator.

**Observed:** multiple symbols render as empty square glyphs, not their intended
icons. Evidence: `01-empty.png`, `02-connection-form.png`.

**Expected:** visible, recognizable symbols or explicit text labels on supported
platforms without depending on incidental system font coverage.

**Implementation lead:** inspect Unicode literals in `apply_app_theme`,
`eframe::App::ui` and `show_connection_manager` in `src/main.rs`. Prefer a
deliberate embedded icon/font/vector strategy; verify any font licensing before
bundling. The exact root cause and behavior on macOS/production GPUs remain
unverified.

**Acceptance:** the real app displays each affected mark correctly with a fresh
profile under the supported font configurations; labels remain understandable
and keyboard focus visible. Inspect actual screenshots, not only widget tests.

## VQA-002 — Required-field error has a success-colored indicator

**Priority:** medium. **State:** open; UI and source evidence.

**Reproduce:** Create connection → leave required fields empty → Save profile.

**Observed:** status text says `Name, host, database, and username are required`,
but its indicator stays green. Evidence: `03-validation-error.png`.
An invalid URL instead gets a red indicator (`09-url-error.png`).

**Source evidence:** the status bar guesses severity from the substrings
`failed`, `could not`, `disconnected` and `error`; this required-field message
matches none of them. `save_connection` assigns validation prose directly.

**Expected:** an explicit validation-error/warning state, consistently styled
and understandable without relying on color alone.

**Acceptance:** required-field, invalid-port and invalid-URL errors are classified
consistently; success/progress/info states retain their intended appearance.
Prefer typed severity over keyword matching. Add regression coverage for state
classification and inspect the corresponding live UI states.

## VQA-003 — Form error remains after cancelling back to the empty screen

**Priority:** low; confirm desired product behavior. **State:** open observation.

**Reproduce:** Create connection → enter `not-a-postgres-url` → Fill fields →
Cancel.

**Observed:** the empty connection screen returns without saving a profile, but
the status bar still shows the previous parse error. Evidence:
`09-url-error.png`, `10-cancel-return.png`.

**Expected decision:** either clear form-specific errors on cancellation or
clearly contextualize retained history so it does not imply the current screen
has failed. This note does not approve one behavior over the other.

**Acceptance after decision:** cancelling does not leave an unexplained
form-specific error on an unrelated screen; no draft is saved accidentally.

## Automation caveats — not established app defects

- X11 synthetic typing lost accents in `QA - São João` despite a successful tool
  result (`04-text-and-tab.png`). Private-clipboard paste preserved the exact
  string (`05-unicode-paste.png`). Verify text readback before query execution;
  do not report the typing backend's behavior as application text corruption.
- At minimum size, lower form actions require scrolling but are reachable
  (`06-minimum-size.png`, `07-minimum-scrolled.png`). Permanent clipping was not
  reproduced; consider density/action placement as a separate UX decision.
- agent-workspace-linux 0.6.0 rejected KWin 6.8.1 screenshot format 16. Private
  X11 was the working alternative. This is infrastructure compatibility, not a
  SQL Manager rendering failure.

## Checks and coverage limits

Passed: debug build, formatting, warning-free Clippy, 27 normal tests, and three
PostgreSQL integration tests repeated three times. The GUI check covered empty
state, form, errors, Tab, text/paste, resizing, scrolling and cancellation.

Not visually verified: connected result grid/query workflow, keyring/backup and
native dialogs, 150% DPI, light theme, actual GPU, Wayland or macOS. Release
packaging was not run. The owned GUI and disposable test database were stopped;
no application implementation or production data was changed.
