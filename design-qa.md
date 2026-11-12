# Terminal chrome design QA

Date: 2026-11-12

final result: passed

Scope: native Linux/Wayland layout and captured visual states. This is a Warp-inspired adaptation, not a pixel-identical clone or certification of untested pointer interactions. Manual acceptance and macOS runtime verification remain outstanding.

## Reference and comparison

The two user-supplied Warp screenshots are the design reference. `target/ui-preview/comparison.png` places both references beside actual screenshots of the built Rust application. The implementation screenshots are cropped to their window bounds and proportionally scaled for the comparison. Window dimensions, terminal contents, background and fonts differ; this comparison judges hierarchy and layout, not pixel accuracy.

- Retained: compact window controls at the upper right, a clear active-tab surface, horizontal tabs or a left sidebar, and a quiet terminal canvas.
- Intentional differences: Kaku's palette replaces the reference's photographic background; existing native frame and tab surfaces remain separate; no decorative search, account or settings controls without corresponding behavior were added.
- Horizontal tabs use a 40-logical-pixel row. The sidebar uses 224 pixels at 96 DPI, capped at 35% of the window width. Window controls have 34-pixel hit slots around 24-pixel drawn circles.
- The layout switch changes only this window's overrides. Persistent startup selection uses `config.vertical_tab_bar`.

## Visual evidence

All captures below are under `target/ui-preview/`, an ignored local output directory. They are real application renders in an isolated KWin virtual Wayland session using WebGPU, not HTML mockups. They can be removed by `cargo clean`.

| Capture | Observation |
| --- | --- |
| `horizontal-final.png` | Top tabs, aligned close/add controls and visible active state |
| `vertical-final.png` | Left sidebar and terminal contents occupying separate regions |
| `toggled-vertical.png` | Runtime layout change reallocates terminal space |
| `horizontal-narrow.png` | Narrow window without overlapping window controls |
| `overflow.png` | 22 tabs; last active tab remains visible with navigation controls |
| `maximized.png` | Restore glyph replaces the maximize glyph |
| `fullscreen.png` | No window frame; terminal/sidebar remain visible |
| `light.png` | Light terminal/tab palette; fixture explicitly retains a dark titlebar |

Earlier captures exposed unfilled rounded-tab corners. The renderer's background coverage was corrected and the final captures reviewed again. No remaining P0/P1/P2 layout defect was observed in these captured states. Hover and press raster tests do not substitute for human pointer testing.

## Automated checks

- Linux GUI binary build and `cargo check --locked -p kaku-gui -p config`: passed.
- GUI binary tests: 681 passed, 1 ignored, 0 failed.
- Wayland frame tests: 4 passed, including drawing/hit alignment, capability-dependent buttons, matching press/release and raster scales 1/2/3.
- New config compatibility test: 1 passed.
- `make fmt-check`, bundled Lua load check, config release readiness and `git diff --check`: passed.
- Full clippy was not run. Existing Linux compiler warnings remain outside this change.
- `scripts/check_release_config.sh` could not finish because this checkout has no `V*` release tags. No release version was changed.
- macOS `make app` and runtime checks were not available on this Linux host. macOS horizontal defaults/native buttons are preserved in source; this is not a macOS runtime verification claim.
- The isolated virtual compositor did not produce a usable OpenGL capture. Screenshots use an explicit preview-only WebGPU override; the production renderer default is unchanged.

## Manual acceptance

Run from the repository root after `cargo build --locked -p kaku-gui --bin kaku-gui`. This starts a separate process and isolates config, session storage and temporary state without replacing or restarting the installed app:

```bash
preview="$PWD/target/ui-preview/manual"
mkdir -p "$preview"/{home,tmp,config,data,state,cache}
env -u KAKU_CONFIG_FILE -u KAKU_CONFIG_DIR -u KAKU_SESSION \
  -u KAKU_UNIX_SOCKET -u KAKU_ZSH_DIR \
  HOME="$preview/home" TMPDIR="$preview/tmp/" \
  XDG_CONFIG_HOME="$preview/config" XDG_DATA_HOME="$preview/data" \
  XDG_STATE_HOME="$preview/state" XDG_CACHE_HOME="$preview/cache" \
  "$PWD/target/debug/kaku-gui" --skip-config \
  --config 'enable_wayland=true' --config 'front_end="WebGpu"' \
  --config 'restore_previous_session=false' --config 'check_for_updates=false' \
  --config 'default_prog={"bash","--noprofile","--norc"}' \
  --config 'use_fancy_tab_bar=true' --config 'vertical_tab_bar=false' \
  --config 'tab_bar_at_bottom=false' --config 'hide_tab_bar_if_only_one_tab=false' \
  start --always-new-process --class fun.kakux.ui-preview
```

1. Create several tabs with `+`, select each, then click **Sidebar** and **Top tabs**. Existing processes and titles should survive; output should not sit underneath the sidebar.
2. Drag tabs in both layouts. Check final order, close-button alignment, and that clicking a non-dragged tab still selects it. Double-click a title to rename it; middle-click a disposable tab to close it.
3. Create enough tabs to overflow, then use the navigation controls and wheel. The active tab should stay visible; narrow the window and repeat.
4. Hover and press each titlebar button. Press close, move outside its hit region and release: the window must stay open. Repeat by releasing over a different button.
5. Minimize and restore via the desktop task manager; maximize/restore and enter/leave fullscreen. Check glyph, frame, terminal sizing and edge hit areas after each transition.
6. Start `sleep 600` in a disposable tab, click close, and verify the existing confirmation/cancel policy. Do not use an important session for this check.

These taps have not been executed by the agent. No automated synthetic mouse interaction was used.
