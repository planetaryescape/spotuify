# Third-party licenses

Code in this repository that was copied or adapted from another project, with
the licence it arrived under. Vendored dependencies pulled in by Cargo are not
listed here — `cargo license` covers those.

## cliamp

- Upstream: <https://github.com/bjarneo/cliamp>
- Licence: MIT
- Copyright: © Bjarne Øverli

The spectrum visualizer styles are ported from cliamp's Go renderers, and the
equalizer preset table is copied from its preset definitions. Derived files,
each carrying a header comment naming its cliamp source:

| spotuify file | cliamp source |
| --- | --- |
| `crates/spotuify-tui/src/widgets/viz/helpers.rs` | `ui/visualizer.go`, `ui/vis_braillegrid.go` |
| `crates/spotuify-tui/src/widgets/viz/bars_dot.rs` | `ui/vis_bars_dot.go` |
| `crates/spotuify-tui/src/widgets/viz/bars_outline.rs` | `ui/vis_bars_outline.go` |
| `crates/spotuify-tui/src/widgets/viz/bricks.rs` | `ui/vis_bricks.go` |
| `crates/spotuify-tui/src/widgets/viz/columns.rs` | `ui/vis_columns.go` |
| `crates/spotuify-tui/src/widgets/viz/classic_peak.rs` | `ui/vis_classic_peak.go` |
| `crates/spotuify-tui/src/widgets/viz/classic_led.rs` | `ui/vis_classic_led.go` |
| `crates/spotuify-tui/src/widgets/viz/mirror.rs` | `ui/vis_mirror.go` |
| `crates/spotuify-tui/src/widgets/viz/scatter.rs` | `ui/vis_scatter.go` |
| `crates/spotuify-tui/src/widgets/viz/rain.rs` | `ui/vis_rain.go` |
| `crates/spotuify-tui/src/widgets/viz/matrix.rs` | `ui/vis_matrix.go` |
| `crates/spotuify-tui/src/widgets/viz/flame.rs` | `ui/vis_flame.go` |
| `crates/spotuify-tui/src/widgets/viz/retro.rs` | `ui/vis_retro.go` |
| `crates/spotuify-tui/src/widgets/viz/pulse.rs` | `ui/vis_pulse.go` |
| `crates/spotuify-tui/src/widgets/viz/wave.rs` | `ui/vis_wave.go` |
| `crates/spotuify-tui/src/widgets/viz/scope.rs` | `ui/vis_scope.go` |
| `crates/spotuify-tui/src/widgets/viz/heartbeat.rs` | `ui/vis_heartbeat.go` |
| `crates/spotuify-tui/src/widgets/viz/sakura.rs` | `ui/vis_sakura.go` |
| `crates/spotuify-tui/src/widgets/viz/firework.rs` | `ui/vis_firework.go` |
| `crates/spotuify-tui/src/widgets/viz/bubbles.rs` | `ui/vis_bubbles.go` |
| `crates/spotuify-tui/src/widgets/viz/terrain.rs` | `ui/vis_terrain.go` |
| `crates/spotuify-tui/src/widgets/viz/firefly.rs` | `ui/vis_firefly.go` |
| `crates/spotuify-tui/src/widgets/viz/mosaic.rs` | `ui/vis_mosaic.go` |
| `crates/spotuify-tui/src/widgets/viz/sand.rs` | `ui/vis_sand.go` |
| `crates/spotuify-tui/src/widgets/viz/geyser.rs` | `ui/vis_geyser.go` |
| `crates/spotuify-tui/src/widgets/viz/butterfly.rs` | `ui/vis_butterfly.go` |
| `crates/spotuify-tui/src/widgets/viz/binary.rs` | `ui/vis_binary.go` |
| `crates/spotuify-tui/src/widgets/viz/ascii.rs` | `ui/vis_ascii.go` |
| `crates/spotuify-core/src/lib.rs` (`EQ_PRESETS` and the band centre frequencies) | `ui/model/eq_presets.go` |

The `bars` style is spotuify's own renderer
(`crates/spotuify-tui/src/widgets/spectrum.rs`) and is not derived from cliamp.

```text
MIT License

Copyright (c) Bjarne Øverli

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
