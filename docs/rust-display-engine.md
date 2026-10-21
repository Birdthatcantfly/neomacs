# Neomacs Architecture: Redesigning Emacs in Rust

## What's Wrong with Official Emacs — Redesign Opportunities

The official Emacs was designed in 1984 for single-core CPUs, text terminals, and kilobyte-sized files. Many of its core design decisions are fundamentally mismatched with modern hardware. This section identifies what to redesign and why.

### 1. Single-Threaded Lisp Machine

**The problem**: Modern CPUs have 8-32+ cores. Emacs uses **one**. Everything — Lisp execution, redisplay, process I/O, file I/O, garbage collection — runs sequentially on a single thread.

**What breaks**:
- `lsp-mode` receives JSON from language server → parses on main thread → blocks redisplay
- `magit` runs git process → reads output → blocks typing
- Font-lock fontifies large file → blocks everything
- GC runs → everything freezes (50-200ms pauses)

**What neomacs can do**: True multi-threaded Elisp:
- **Render thread** (already done in neomacs)
- **Process I/O thread** — read subprocess output without blocking Lisp
- **Parallel fontification** — font-lock on background thread
- **Concurrent GC** — no stop-the-world pauses
- **Parallel Lisp execution** — multiple Lisp threads with proper synchronization

### 2. CPU-Only Rendering

**The problem**: Emacs draws every glyph via CPU → X11/Cairo. The GPU sits completely idle. A modern GPU has ~10,000 shader cores designed for exactly this workload (rendering thousands of small textured quads).

**What neomacs already does well**: wgpu rendering, glyph atlas caching, batched instanced drawing, shader effects. This is largely solved.

**What could be better**:
- GPU-side glyph rasterization (currently cosmic-text rasterizes on CPU, uploads to GPU)
- Compute shader for glyph positioning (minor — CPU layout is fast enough)

### 3. Persistent Pixel Buffers vs Scheduled GPU Rendering

Official Emacs backends retain pixels and update changed regions. Neomacs uses
GPU command generation, but that does not imply an unconditional redraw loop.
The renderer retains the active presentation and resource caches, schedules a
frame after invalidation or animation, and tracks coarse composition damage.

```
Official Emacs (retained):
  Frame 1: Draw everything to pixmap
  Frame 2: Diff desired vs current → redraw 3 changed rows
  Frame 3: Diff → redraw 1 changed row
  Cost: O(changed) per frame, but complex bookkeeping (~7k LOC in dispnew.c)

Neomacs (scheduled GPU composition):
  Logical change → prepare and activate a presentation → damage content
  Cursor animation → damage cursor composition → schedule animation frame
  No change → keep active presentation/resources → no content rebuild
```

**What neomacs does well**: Correctness does not depend on backend pixel-copy
operations. Retained layout, glyph, texture, and composition caches are
performance mechanisms; the active presentation remains the semantic source
for drawing and hit testing.

### 4. Character-Cell Thinking

**The problem**: Emacs was designed for fixed-width character terminals. Internally, many things are measured in "columns" not pixels:

- `current-column` returns column number (character count)
- `move-to-column` moves to column N
- `window-width` returns width in characters
- Tab stops are character-aligned
- Truncation/wrapping decisions use column counts
- `hscroll` is measured in columns

This breaks with:
- **Variable-width fonts** — column N isn't at a fixed pixel position
- **Ligatures** — "fi" is one glyph, not two columns
- **Emoji** — may be wider than 2 columns
- **CJK** — 2-column wide but pixel width varies with font

**What neomacs can do**: Pixel-accurate layout from the start. cosmic-text handles variable-width, ligatures, and complex scripts natively. Column-based Lisp functions (`current-column`, etc.) become thin wrappers that convert pixel positions back to approximate column numbers for compatibility.

### 5. No Native Text Shaping

**The problem**: Emacs bolted on HarfBuzz support (Emacs 27+), but it's not deeply integrated:

- Shaping runs per-glyph-string (a run of same-face characters), not per-paragraph
- No native ligature support across face boundaries
- Composition is handled by a separate, older system (`composite.c`)
- Font fallback is per-character, not per-cluster
- No support for OpenType features (stylistic alternates, contextual forms, etc.) without manual configuration

**What neomacs can do**: cosmic-text + rustybuzz provide a modern text shaping pipeline:
- Full OpenType shaping (ligatures, kerning, mark positioning)
- Per-paragraph shaping (correct for Arabic/Devanagari)
- Automatic font fallback per-cluster
- All OpenType features accessible

### 6. Synchronous Process I/O

**The problem**: When a subprocess (LSP server, git, grep) produces output, Emacs reads it in the main thread via `process-filter`. This blocks everything:

```
User types key → command runs → spawns process → reads output →
  BLOCKS (waiting for data) → process filter runs → parses JSON →
  BLOCKS (parsing) → updates buffer → redisplay → finally responds to user
```

With LSP, this is catastrophic. A 100ms language server response means 100ms of frozen UI.

**What neomacs can do**:
- Process I/O on dedicated threads
- Non-blocking process filters that accumulate output
- Parse JSON/protocol data on background thread
- Only deliver parsed results to Lisp thread via message queue

### 7. Stop-the-World Garbage Collection

**The problem**: Emacs uses a mark-and-sweep GC that freezes everything:

```
GC trigger → mark all live objects (traverse entire heap) →
  sweep dead objects → compact → resume

Duration: 50-200ms for large sessions (thousands of buffers, overlays)
```

During GC, no input processing, no redisplay, no process I/O. Users experience visible stuttering.

**What neomacs can do**:
- **Incremental GC** — mark a few objects per Lisp instruction, spread cost over time
- **Generational GC** — young objects die fast, only scan nursery most of the time
- **Concurrent GC** — mark phase runs on background thread
- **Arena allocation** — temporary objects in per-command arenas, freed in bulk

### 8. Internal String Encoding (Not UTF-8)

**The problem**: Emacs uses its own multibyte encoding ("emacs-mule" / internal encoding), not UTF-8. Every interaction with the outside world requires conversion:

```
File on disk (UTF-8) → decode to Emacs internal → process → encode to UTF-8 → write
Subprocess output (UTF-8) → decode → process → encode → send
```

This is slow (constant encoding/decoding overhead), complex (`FETCH_MULTIBYTE_CHAR` is not a simple byte read), and incompatible (every Rust/C library expects UTF-8).

**What neomacs can do**: Use UTF-8 internally. Eliminates all encoding/decoding overhead and makes FFI with Rust trivial (Rust strings are UTF-8 natively).

### 9. Gap Buffer

**The problem**: Emacs uses a gap buffer — a contiguous array with a "gap" at the editing point:

```
Buffer: [H][e][l][l][o][___GAP___][w][o][r][l][d]
                        ↑ cursor
Insert 'X': just put it in the gap, O(1)
Move cursor far away: move the gap = O(gap_distance), memmove
```

Good for sequential editing at one point. Bad for:
- **Multiple cursors** — need to move gap for each cursor, O(n) per cursor
- **Large files** — gap move = memmove of potentially megabytes
- **Cache locality** — gap fragments the text, cache misses when reading across gap
- **Concurrent access** — can't read text while gap is moving (thread-unsafe)

**What neomacs can do**:
- **Piece table** (used by VS Code) — O(log n) insert/delete, immutable original text
- **Rope** (used by Xi editor, Zed) — O(log n) everything, cache-friendly, naturally supports multiple cursors and concurrent reads
- **Hybrid** — keep gap buffer for compatibility, add parallel rope index for layout engine reads

### 10. Redisplay Blocks Lisp

**The problem**: `redisplay_internal()` runs synchronously. While redisplay computes layout, no Lisp can execute. For complex buffers (org-mode with many overlays, magit with thousands of diff lines), redisplay can take 50-100ms. The UI feels sluggish.

**What neomacs can do**: With the Rust display engine:
- Layout the visible region first (above-the-fold rendering)
- Send partial results to GPU immediately
- Continue layout for offscreen regions asynchronously
- Lisp can resume execution while offscreen layout completes

### 11. No SIMD / Vectorization

**The problem**: Text processing (searching, encoding, column counting, whitespace detection) is done byte-by-byte in scalar C code. Modern CPUs have SIMD instructions (AVX2, NEON) that process 32-64 bytes simultaneously.

```
Scalar (Emacs):     for each byte: if byte == '\n' count++    → ~1 byte/cycle
SIMD (possible):    load 32 bytes → compare all → popcount    → ~32 bytes/cycle
```

**What neomacs can do**: Rust has excellent SIMD support via `memchr` crate (10-30x faster byte search), SIMD-accelerated UTF-8 validation (`simdutf`), SIMD line counting and column calculation.

### 12. Lisp Object Memory Layout

**The problem**: Emacs Lisp objects are tagged pointers scattered across the heap. A cons cell, a string, and a symbol may be far apart in memory. Iterating over a list causes cache misses on every `CDR`:

```
Memory:
  [cons1] ... 4KB gap ... [cons2] ... 8KB gap ... [cons3]

  Each CDR → cache miss → ~100ns stall
```

Modern CPUs can do 10 billion operations/second but only 10 million cache misses/second. Memory layout matters more than instruction count.

**What neomacs can do**:
- **Arena-allocated lists** — cons cells for the same list in contiguous memory
- **Struct-of-arrays** for frequently iterated data (face attributes, glyph properties)
- **Small-string optimization** — short strings inline in the Lisp_Object, no pointer chase
- **Bump allocation** for temporary objects (per-command arena)

### Redesign Priority Order

| Priority | Area | Neomacs Status | Impact |
|----------|------|---------------|--------|
| 1 | **Multi-threaded Lisp** | Planned | Eliminates root cause of all UI freezing |
| 2 | **GPU rendering** | Done | 120fps, shader effects, animations |
| 3 | **Rust display engine** | In progress (this doc) | Unified layout+render, TUI backend |
| 4 | **Async process I/O** | Planned | LSP/subprocess won't block |
| 5 | **Concurrent/incremental GC** | Planned | Eliminates GC pauses |
| 6 | **UTF-8 internal encoding** | Planned | Zero conversion overhead, trivial Rust FFI |
| 7 | **SIMD text processing** | Easy wins available | 10-30x faster search/scan |
| 8 | **Rope/piece table** | Future | Multi-cursor, large files |
| 9 | **Cache-friendly memory** | Future | Requires Lisp runtime redesign |

---

# Rust Display Engine (Strategy 4)

Replace Emacs's C display engine (`xdisp.c`, `dispnew.c`, ~40k LOC) with a Rust layout engine that reads buffer data directly and produces GPU-ready glyph batches.

## Current Presentation Transaction

The evaluator and renderer advance independently, so “latest redisplay” and
“currently drawable” are intentionally different concepts:

```text
GNU-compatible Frame/Window logical state
                 |
                 v
redisplay builds Presentation N
  - glyph/render projection
  - window regions and source positions
  - hit-test projection
  - frame ancestry/transforms/clips
                 |
                 v
Prepared N -- send rejected/superseded --> Discarded N
                 |
                 v
Active N (renderer drawing + hit testing)
                 |
                 v
Retired N
```

Platform scanout feedback is a later physical `Presented | Discarded` outcome;
renderer activation does not claim that pixels reached the display.

The important ownership boundaries are:

- `Frame` and `Window` remain the synchronous GNU logical model. Logical
  `window-*` geometry queries do not require a renderer presentation.
- `prepare_display_presentation` constructs immutable
  `PresentationGeometry` and a renderer submission with one monotonic
  `PresentationId`. It does not change active visual geometry.
- `PresentationActivated`, `PresentationDiscarded`, and
  `PresentationRetired` events move evaluator state through the renderer-owned
  lifecycle. Exact `posn-*`, pointer, and visual-anchor operations use only the
  active identity and reject stale queries.
- `PresentationSpatialPlan` compiles window regions, transforms, clipping,
  source positions, and hit-test metadata from one sealed spatial input.
  Render and interaction indexes may differ, but their spatial facts cannot.
- The latest redisplay cache supports GNU logical/TTY behavior. It is private
  state populated by preparation, not an independently published visual truth.

Certified TTY row-query reuse is enabled by default; pass `--rows-query-admission=off` to opt out.

Popup placement follows the same rule. A request carries an anchor rectangle,
preferred side, offset, and constraint policy. Popup layout measures the
content, then resolves flip/shift constraints against the owning frame's
logical viewport. Raw `(x, y)` remains only a GNU-compatible explicit-position
form represented as `AtAnchor`; it is not the internal placement abstraction.

There are deliberately no “prefer presented” getters or fallback publication
APIs. A caller chooses one of two interfaces: synchronous logical geometry or
presentation-qualified active visual geometry.

## How the Official Emacs Display Engine Works

Understanding what we're replacing. The Emacs display engine is a **two-phase system**:

```
Phase 1: BUILD desired_matrix    (xdisp.c — "what should be on screen")
Phase 2: UPDATE current_matrix   (dispnew.c — "make the screen match")
```

Each window has TWO glyph matrices:
- **`desired_matrix`** — what redisplay WANTS to show (rebuilt each cycle)
- **`current_matrix`** — what's ACTUALLY on screen (updated incrementally)

The update phase diffs them and only redraws what changed — like a virtual DOM diff.

### Entry Point: `redisplay_internal()` (xdisp.c:17360)

Called after every command, timer, process output, or explicit `(redisplay)`:

```
redisplay_internal()
  ├─ Check if redisplay is needed (windows_or_buffers_changed?)
  ├─ Run pre-redisplay-function (Lisp hook)
  ├─ prepare_menu_bars()
  ├─ For each visible frame:
  │    └─ For each window in frame's window tree:
  │         └─ redisplay_window(w)
  ├─ Run post-redisplay hooks
  └─ Set windows_or_buffers_changed = 0
```

### Per-Window: `redisplay_window()` (~2000 lines)

Decides what to display. Tries optimizations first, falls back to full layout:

```
redisplay_window(w)
  ├─ Check if window-start is still valid
  ├─ Try optimization: try_window_reusing_current_matrix()
  │    └─ If buffer unchanged, try to reuse existing rows (scroll optimization)
  ├─ Try optimization: try_window_id()
  │    └─ Find minimal diff between current and desired (incremental update)
  ├─ Fallback: try_window()
  │    └─ Full layout from window-start to bottom of window
  ├─ If point not visible → adjust window-start → retry (2-5 passes)
  ├─ Display mode-line, header-line, tab-line
  └─ Set w->window_end_pos, w->window_end_vpos, w->cursor
```

### The Core: `display_line()` (~500 lines)

Generates ONE visual row (glyph row) from buffer content:

```
display_line(it)
  ├─ prepare_desired_row()  — clear row
  ├─ LOOP:
  │    ├─ get_next_display_element(it)  — fetch next thing to display
  │    ├─ PRODUCE_GLYPHS(it)           — compute metrics, append glyph
  │    ├─ Check: line full? (wrapping/truncation decision)
  │    ├─ Check: reached end of window?
  │    └─ set_iterator_to_next(it)     — advance position
  ├─ Handle end-of-line: padding, continuation/truncation glyphs
  └─ Compute row metrics: ascent, descent, height
```

### The Display Iterator (`struct it`)

The central abstraction. A state machine with ~550 fields that walks through display content:

```c
struct it {
    // Where we are
    struct display_pos current;     // Buffer/string position
    ptrdiff_t stop_charpos;         // Next position to check for property changes

    // What we're iterating over
    enum it_method method;          // GET_FROM_BUFFER, GET_FROM_STRING,
                                    // GET_FROM_IMAGE, GET_FROM_STRETCH, etc.

    // What we found
    enum display_element_type what; // IT_CHARACTER, IT_IMAGE, IT_COMPOSITION,
                                    // IT_STRETCH, IT_GLYPHLESS, etc.
    int c;                          // Character code (if IT_CHARACTER)
    int face_id;                    // Resolved face for current element

    // Metrics of current element
    int pixel_width;
    int ascent, descent;
    int current_x, current_y;

    // Nested context stack (for display properties in overlay strings, etc.)
    struct iterator_stack_entry stack[5];  // IT_STACK_SIZE = 5
    int sp;                               // Stack pointer

    // Bidi state
    struct bidi_it bidi_it;

    // Display state
    int hscroll;
    enum line_wrap_method line_wrap;  // TRUNCATE, WORD_WRAP, WINDOW_WRAP

    // ... ~500 more fields
};
```

#### Iterator Methods (9 types)

The iterator is polymorphic — `method` determines how to get the next element:

| Method | Source | Used for |
|--------|--------|----------|
| `GET_FROM_BUFFER` | Buffer text | Normal text editing |
| `GET_FROM_STRING` | Lisp string | Overlay strings, display strings |
| `GET_FROM_C_STRING` | C string | Mode-line format results |
| `GET_FROM_IMAGE` | Image spec | `(image ...)` display property |
| `GET_FROM_STRETCH` | Space spec | `(space ...)` display property |
| `GET_FROM_XWIDGET` | Xwidget | Embedded widgets |
| `GET_FROM_DISPLAY_VECTOR` | Display table | Character display overrides |
| `GET_FROM_VIDEO` | Video spec | `(video ...)` (neomacs extension) |
| `GET_FROM_WEBKIT` | WebKit spec | `(webkit ...)` (neomacs extension) |

#### The Property Check Pipeline

At each `stop_charpos`, the iterator runs property handlers in sequence:

```
handle_stop(it)
  ├─ handle_fontified_prop()     — trigger jit-lock if text not fontified
  │    └─ Call fontification-functions (Lisp callback!)
  ├─ handle_face_prop()          — resolve face at position
  │    └─ face_at_buffer_position()
  │         ├─ Get text property 'face
  │         ├─ Get ALL overlay 'face properties (sorted by priority)
  │         └─ Merge into single realized face ID
  ├─ handle_display_prop()       — check for display overrides
  │    └─ handle_single_display_spec()  (634 lines, 14 spec types)
  │         ├─ String → push_it(), switch to GET_FROM_STRING
  │         ├─ Image → switch to GET_FROM_IMAGE
  │         ├─ Space → switch to GET_FROM_STRETCH
  │         ├─ Fringe bitmap → record for fringe drawing
  │         ├─ Margin spec → redirect to margin area
  │         └─ ... 14 types total, composable in lists/vectors
  ├─ handle_invisible_prop()     — skip invisible text
  │    └─ advance past invisible region, handle ellipsis
  └─ handle_overlay_change()     — load overlay strings
       └─ load_overlay_strings()
            ├─ Scan overlays at position (itree)
            ├─ Collect before-strings and after-strings
            ├─ Sort by priority
            └─ push_it(), switch to GET_FROM_STRING
```

#### The Iterator Stack (5 levels)

When a display property replaces text with a string, the iterator **pushes** its current state and switches to iterating the string. When the string is exhausted, it **pops** back:

```
Buffer text: "Hello [OVERLAY] world"
                     ↓
            overlay has before-string: ">>>"
            ">>>" has display property: (image :file "arrow.png")

Iterator state stack:
  Level 0: GET_FROM_BUFFER  (buffer text)
  Level 1: GET_FROM_STRING  (overlay before-string ">>>")
  Level 2: GET_FROM_IMAGE   (image in display property)

  Pop level 2 → back to ">>>" string
  Pop level 1 → back to buffer text at "world"
```

Maximum nesting: 5 levels (`IT_STACK_SIZE`).

### Glyph Production

`PRODUCE_GLYPHS(it)` → `gui_produce_glyphs(it)` computes metrics and appends a glyph:

```
gui_produce_glyphs(it)
  ├─ IT_CHARACTER:
  │    ├─ Get font metrics (ascent, descent, width) from face->font
  │    ├─ Handle special cases: tab, newline, control chars
  │    └─ append_glyph() → add CHAR_GLYPH to row
  ├─ IT_COMPOSITION:
  │    ├─ Get composed glyph metrics from composition table
  │    └─ append_composite_glyph() → add COMPOSITE_GLYPH
  ├─ IT_IMAGE:
  │    ├─ Compute image dimensions (scaling, slicing)
  │    └─ append_glyph() → add IMAGE_GLYPH
  ├─ IT_STRETCH:
  │    ├─ Compute space width (calc_pixel_width_or_height)
  │    └─ append_stretch_glyph() → add STRETCH_GLYPH
  └─ IT_GLYPHLESS:
       ├─ Missing glyph → show as hex code or empty box
       └─ append_glyphless_glyph()
```

#### Glyph Structure

Each glyph in the matrix:

```c
struct glyph {
    // What to display
    unsigned type : 3;          // CHAR_GLYPH, IMAGE_GLYPH, STRETCH_GLYPH, etc.
    union {
        unsigned ch;            // Character code (CHAR_GLYPH)
        struct { int id; } cmp; // Composition ID
        int img_id;             // Image ID
    } u;

    // How to display it
    unsigned face_id : 20;      // Index into face cache

    // Layout metrics
    short pixel_width;
    short ascent, descent;
    short voffset;              // Vertical offset (for 'raise' property)

    // Buffer position (for cursor, mouse, etc.)
    ptrdiff_t charpos;
    Lisp_Object object;         // Source: buffer, string, or nil

    // Bidi
    unsigned resolved_level : 7;
    unsigned bidi_type : 3;

    // Flags
    bool padding_p;
    bool left_box_line_p, right_box_line_p;
    bool overlaps_vertically_p;
};
```

#### Glyph Row

One visual line:

```c
struct glyph_row {
    struct glyph *glyphs[3];    // LEFT_MARGIN_AREA, TEXT_AREA, RIGHT_MARGIN_AREA
    short used[3];              // Glyph count per area

    int x, y;                   // Position in window
    int pixel_width;
    int ascent, descent, height;

    // Buffer range this row displays
    struct text_pos start, end;
    struct text_pos minpos, maxpos;  // Bidi-aware min/max

    // Flags
    bool enabled_p;
    bool displays_text_p;
    bool mode_line_p, header_line_p, tab_line_p;
    bool truncated_on_left_p, truncated_on_right_p;
    bool reversed_p;            // RTL paragraph

    // Fringe
    int left_fringe_bitmap, right_fringe_bitmap;
};
```

### Phase 2: Updating the Screen (dispnew.c)

After Phase 1 fills `desired_matrix`, Phase 2 makes the screen match:

```
update_window(w)
  ├─ gui_update_window_begin()      — save cursor, setup
  ├─ Optimization: scrolling_window()
  │    └─ Try to reuse rows by scrolling (copy rows up/down)
  ├─ For each row in desired_matrix:
  │    ├─ Compare with current_matrix row
  │    ├─ If different: update_window_line()
  │    │    ├─ update_text_area()   — draw changed glyphs
  │    │    │    └─ Calls rif->write_glyphs() → backend draws
  │    │    └─ Update marginal areas
  │    └─ If same: skip (no redraw needed)
  ├─ Copy desired_matrix → current_matrix
  ├─ set_window_cursor_after_update()
  ├─ gui_update_window_end()
  │    ├─ Draw cursor via rif->draw_window_cursor()
  │    └─ Draw borders via rif->draw_vertical_window_border()
  └─ rif->flush_display()          — commit to screen
```

### The Backend Interface (`struct redisplay_interface`)

The display engine talks to backends (X11, GTK, neomacs, etc.) through function pointers:

```c
struct redisplay_interface {
    void (*produce_glyphs)(struct it *);
    void (*write_glyphs)(struct window *, struct glyph_row *, ...);
    void (*update_window_begin_hook)(struct window *);
    void (*update_window_end_hook)(struct window *, ...);
    void (*draw_glyph_string)(struct glyph_string *);
    void (*draw_window_cursor)(struct window *, struct glyph_row *, ...);
    void (*draw_vertical_window_border)(struct window *, int, int, int);
    void (*draw_fringe_bitmap)(struct window *, struct glyph_row *, ...);
    void (*clear_frame_area)(struct frame *, int, int, int, int);
    void (*scroll_run_hook)(struct window *, struct run *);
    void (*flush_display)(struct frame *);
};
```

In neomacs, this is `neomacs_redisplay_interface` (neomacsterm.c:160). Neomacs **ignores most of these hooks** and instead walks `current_matrix` directly in `neomacs_extract_full_frame()`.

### Face Resolution Pipeline

```
Text at position P with overlays O1 (priority=10), O2 (priority=20):

1. Start with DEFAULT_FACE_ID
2. Merge text property 'face at P       → attrs[]
3. Merge O1's 'face (lower priority)    → attrs[]
4. Merge O2's 'face (higher priority)   → attrs[]
5. Lookup/create realized face from attrs[] → face_id

face_at_buffer_position(w, P, ...)
  → Returns single face_id with all attributes merged
```

Face attributes (from `struct face`):
- `foreground`, `background` (pixel colors)
- `font` (`struct font` — metrics, shaping)
- `underline` (style: single/wave/double/dots/dashes, color)
- `overline`, `strike_through` (bool + color)
- `box` (type: none/line/3D, width, corner_radius, color)
- `bold`, `italic` (via font selection)

### Window-Start Computation

The hardest part of redisplay — deciding which part of the buffer to show:

```
redisplay_window(w)
  ├─ Is current w->start still good?
  │    └─ try_window(w->start)
  │         ├─ Layout from w->start to bottom
  │         ├─ Is point visible? → YES: done
  │         └─ Is point in scroll margin? → return -1 (need adjustment)
  │
  ├─ Point not visible → compute new window-start:
  │    ├─ scroll_conservatively > 0?
  │    │    └─ Try small adjustments to show point
  │    ├─ scroll_step > 0?
  │    │    └─ Scroll by scroll_step lines
  │    └─ Fallback: center point in window
  │         └─ move_it_vertically_backward() to find start
  │
  └─ Set w->start = new_start, retry layout
```

This can take **2-5 layout passes** per frame.

### Mode-Line Rendering

```
display_mode_line(w)
  ├─ format_mode_line(mode_line_format)  — evaluate Lisp format
  │    └─ Recursive: handles %b (buffer name), %l (line), %c (column),
  │       conditionals, Lisp expressions, etc.
  │    └─ Returns: Lisp string with text properties (faces)
  ├─ init_iterator() for mode-line row
  ├─ display_string() — layout the formatted string
  └─ Set w->mode_line_height from row height
```

### Key Design Principles of the Official Engine

1. **Incremental update**: Only redraw what changed (desired vs current matrix diff).
2. **Lazy computation**: Don't compute until needed. `jit-lock` fontifies on demand. `window-end` only computed when queried.
3. **Property-change-driven scanning**: The iterator jumps between `stop_charpos` positions where properties change, skipping unchanged regions.
4. **Stack-based nesting**: Display properties, overlay strings, and display strings nest via a 5-level stack, avoiding recursion.
5. **Backend abstraction**: Drawing is separated from layout via `redisplay_interface` function pointers.
6. **Persistent pixel buffers**: Official backends (X11, GTK) draw once and persist. Only changed regions are redrawn. (Neomacs differs — it clears and rebuilds every frame.)

### Why It's Complex (~40k LOC)

- **xdisp.c alone is 39,660 lines** because it handles every edge case: bidi, 14 display spec types, 5-level nesting, overlay string interleaving, incremental matrix optimization, scroll heuristics, 3-tier window-start computation, mode-line format evaluation, fringe bitmaps, margin areas, tab/header lines, minibuffer resize, mouse highlighting, and glyphless character rendering.
- Much of this complexity is **optimization** (reusing matrices, scrolling rows, diffing) that neomacs doesn't need since GPU clears and rebuilds every frame.
- Another chunk is **backend-specific code** (X11/GTK drawing) that a Rust engine replaces entirely.

## Current Architecture

```
Emacs redisplay (C, xdisp.c ~30k LOC)
  → glyph matrices (current_matrix)
    → neomacsterm.c extracts glyphs (FFI boundary)
      → FrameGlyphBuffer sent to Rust via crossbeam
        → wgpu renders
```

### Pain Points

- **Double work**: Emacs builds a full CPU-side glyph matrix, then we serialize it again into `FrameGlyphBuffer`.
- **FFI friction**: Every new feature (cursors, borders, images, animations) requires C-side extraction code + Rust-side handling.
- **CPU-centric design**: Emacs redisplay was designed for `XDrawString` / terminal cells, not GPU batched rendering.
- **No GPU awareness**: Layout doesn't know about GPU capabilities (instancing, atlas caching, compute shaders).

## Proposed Architecture

**IMPORTANT**: Layout must run on the **Emacs thread**, not the render thread. See [Critical Design Constraint: Synchronization](#critical-design-constraint-synchronization) for why.

```
Emacs buffer/overlay/face data
  → Rust layout engine (called on Emacs thread during redisplay)
    → LayoutOutput (backend-agnostic glyph positions)
      → sent to render thread via crossbeam
        → wgpu renders (GPU) or TuiRenderer (terminal)
```

Layout results (window-end, cursor position, visibility) are also fed back to Emacs so that Lisp query functions (`pos-visible-in-window-p`, `window-end`, etc.) continue to work.

## Critical Design Constraint: Synchronization

Many Emacs Lisp functions require **synchronous access** to layout results. These aren't obscure — they're called on every keystroke by popular packages:

| Function | Who calls it | What it does |
|----------|-------------|--------------|
| `pos-visible-in-window-p` | posframe, company, corfu, vertico | Runs the display iterator from scratch |
| `window-end` | helm, ivy, magit, every scroll package | Runs `start_display()` + `move_it_vertically()` |
| `vertical-motion` | `forward-line`, `recenter`, all movement | Runs the full iterator with wrapping |
| `posn-at-point` / `posn-at-x-y` | Mouse tracking, popup positioning, tooltips | Queries glyph matrix |
| `move-to-column` | Indentation, `kill-rectangle`, column movement | Scans line with tab/display prop handling |
| `compute-motion` | Low-level motion, `goto-line` | Full layout computation |

Additionally, **fontification runs DURING layout**. `jit-lock` calls `fontification-functions` as the iterator advances through unfontified text. This is Lisp code that must execute on the Emacs thread. Layout cannot be purely on the render thread — it calls back into Lisp.

**Consequence**: The Rust layout engine must run on the Emacs thread (called during `redisplay_internal()`). Layout results are then sent to the render thread for rendering. Parallel layout (layout on render thread while Emacs executes Lisp) is a future optimization (Phase 8+), not a starting point.

### Synchronous Lisp Function Categories

**Category A — Read cached state (safe to defer):**
`window-start`, `window-hscroll`, `window-vscroll`, `window-body-width`, `window-body-height`, `window-text-width`, `window-text-height`, `coordinates-in-window-p`

**Category B — Read current matrix (needs up-to-date layout):**
`window-line-height`, `window-lines-pixel-dimensions`, `posn-at-x-y`

**Category C — Trigger layout computation (CRITICAL — blocks Lisp):**
`window-end(update=t)`, `pos-visible-in-window-p`, `vertical-motion`, `move-to-column`, `compute-motion`, `move-to-window-line`, `recenter`

**Category D — Modify state and trigger redisplay:**
`set-window-start`, `set-window-hscroll`, `set-window-vscroll`, `scroll-up`/`scroll-down`, `scroll-left`/`scroll-right`

**Category E — Hooks that expect synchronous state:**
`pre-redisplay-function` (runs INSIDE `redisplay_internal()`), `fontification-functions` (runs DURING iterator scanning), `window-scroll-functions`, `post-command-hook`

## Display Engine API Contract

The display engine sits between the Emacs C core and the rendering backend. Understanding exactly what flows in each direction is critical — the Rust replacement must satisfy this contract.

```
            EMACS C CORE PROVIDES                    DISPLAY ENGINE PROVIDES BACK
            ═══════════════════                      ═══════════════════════════

  Buffer text (gap buffer) ──────┐         ┌──── w->window_end_pos/vpos/valid
  Text properties (face,         │         │     w->cursor (hpos, vpos, x, y)
    display, invisible,          │         │     w->phys_cursor (+ width/height)
    fontified, composition) ─────┤         │     w->mode_line_height
  Overlays (face, before/after   │         │       /header_line_height
    string, invisible,           │    ┌────┴──┐    /tab_line_height
    priority, window) ───────────┼───►│DISPLAY│───►w->current_matrix (glyph rows)
  Face cache (colors, fonts,     │    │ENGINE │   windows_or_buffers_changed
    decorations) ────────────────┤    └────┬──┘   f->cursor_type_changed
  Window state (start, hscroll,  │         │
    pointm, dimensions) ─────────┤         ├──── pos-visible-in-window-p()
  Buffer-local vars (truncate,   │         │     window-end()
    word-wrap, tab-width,        │         │     vertical-motion()
    bidi, line-spacing) ─────────┤         │     move-to-column()
  Global vars (scroll-margin,    │         │     compute-motion()
    scroll-step, display-        │         │     posn-at-point/x-y()
    line-numbers) ───────────────┘         │     current-column()
                                           │     line-pixel-height()
  fontification-functions ◄────────────────┘     format-mode-line()
  (callback DURING layout)                       (Lisp query functions)
```

### Inputs: What Emacs C Core Provides to the Display Engine

#### Buffer Data

| Input | How accessed | Purpose |
|-------|-------------|---------|
| Buffer text | `BYTE_POS_ADDR`, `FETCH_MULTIBYTE_CHAR` | Raw characters to display |
| Narrowing bounds | `BEGV`, `ZV` | Visible region of buffer |
| Point position | `PT` | Where cursor goes |
| Multibyte flag | `BVAR(buf, enable_multibyte_characters)` | Character encoding mode |

#### Text Properties (at each position)

| Property | Purpose |
|----------|---------|
| `face` | Text styling (colors, bold, italic, underline, etc.) |
| `display` | Override rendering (images, strings, spaces, margins, fringes) |
| `invisible` | Hide text or show ellipsis |
| `fontified` | Triggers lazy fontification via `fontification-functions` |
| `composition` | Ligatures, combining characters, emoji sequences |
| `mouse-face` | Hover highlighting |
| `line-height` | Override line height |
| `raise` | Vertical offset |

#### Overlays (at each position)

| Property | Purpose |
|----------|---------|
| `face` | Overlay face (merged with text property face, priority-ordered) |
| `display` | Display overrides |
| `before-string` / `after-string` | Inserted strings with their own properties |
| `invisible` | Hide overlay region |
| `priority` | Stacking order for face merging and string ordering |
| `window` | Per-window overlay filtering |

#### Face Data

| Input | Purpose |
|-------|---------|
| `face_at_buffer_position()` | Merged face at position (text prop + all overlays) |
| `FACE_FROM_ID(f, id)` | Resolve face ID to colors, font, decorations |
| `face_for_char()` | Font fallback for specific character (fontset system) |
| `merge_faces()` | Combine control-char face with base face |

#### Window State

| Field | Purpose |
|-------|---------|
| `w->start` | First visible buffer position (marker) |
| `w->pointm` | Window's copy of point (for non-selected windows) |
| `w->hscroll`, `w->min_hscroll` | Horizontal scroll offset |
| `WINDOW_PIXEL_WIDTH/HEIGHT(w)` | Window dimensions in pixels |
| `w->contents` | Which buffer is displayed |
| Display table | `window_display_table(w)` — character display overrides |

#### Buffer-Local Variables

| Variable | Purpose |
|----------|---------|
| `truncate-lines` | Truncate long lines vs wrap |
| `word-wrap` | Word wrapping mode |
| `tab-width` | Tab character width in columns |
| `line-spacing` / `extra-line-spacing` | Extra vertical space between lines |
| `selective-display` | Hide lines by indentation level |
| `ctl-arrow` | Display control chars as `^X` vs `\NNN` |
| `bidi-display-reordering` | Enable bidirectional text reordering |
| `bidi-paragraph-direction` | Force paragraph direction (left-to-right / right-to-left) |

#### Global Variables

| Variable | Purpose |
|----------|---------|
| `scroll-margin` | Lines to keep visible around point |
| `scroll-conservatively` | How aggressively to scroll |
| `scroll-step` | Lines to scroll at a time |
| `hscroll-margin` / `hscroll-step` | Horizontal scroll parameters |
| `truncate-partial-width-windows` | Truncate in narrow windows |
| `display-line-numbers` | Line number display mode (absolute/relative/visual) |
| `display-line-numbers-width` | Width of line number column |
| `nobreak-char-display` | Display non-breaking spaces/hyphens |
| `auto-composition-mode` | Enable automatic character composition |
| `maximum-scroll-margin` | Cap on scroll margin |

### Outputs: What the Display Engine Must Provide Back

**This is the contract the Rust replacement must satisfy.**

#### Window Fields (set during redisplay, read by Emacs)

| Field | Type | Who reads it | Purpose |
|-------|------|-------------|---------|
| `w->window_end_pos` | `ptrdiff_t` | `window-end` (window.c) | Buffer position of last visible char (as `Z - end_charpos`) |
| `w->window_end_vpos` | `int` | window.c | Matrix row number of last visible char |
| `w->window_end_valid` | `bool` | `window-end`, `window-line-height` | Guard: are end fields valid? Many functions check this first. |
| `w->cursor` | `{x, y, hpos, vpos}` | Cursor drawing, movement commands | Intended cursor position in matrix coordinates |
| `w->phys_cursor` | `{x, y, hpos, vpos}` | neomacsterm.c, window.c | Actual cursor pixel position (window-relative) |
| `w->phys_cursor_type` | `enum` | window.c | Current cursor style: box(0), bar(1), hbar(2), hollow(3) |
| `w->phys_cursor_width` | `int` | neomacsterm.c, window.c | Cursor width in pixels |
| `w->phys_cursor_height` | `int` | neomacsterm.c, window.c | Cursor height in pixels |
| `w->phys_cursor_ascent` | `int` | window.c | Cursor ascent in pixels |
| `w->phys_cursor_on_p` | `bool` | window.c, dispnew.c | Is cursor currently being displayed? |
| `w->mode_line_height` | `int` | `window-mode-line-height`, layout | Mode-line pixel height (-1 if unknown) |
| `w->header_line_height` | `int` | `window-header-line-height` | Header-line pixel height (-1 if unknown) |
| `w->tab_line_height` | `int` | `window-tab-line-height` | Tab-line pixel height (-1 if unknown) |
| `w->last_cursor_vpos` | `int` | xdisp.c | Previous frame's cursor vpos (detects cursor movement) |

#### Frame Fields

| Field | Purpose |
|-------|---------|
| `f->cursor_type_changed` | Signals cursor needs redraw (set true on change, cleared after draw) |
| `f->garbaged` | Cleared after full redraw |
| `f->updated_p` | Frame was updated this redisplay cycle |

#### Global Variables

| Variable | Purpose |
|----------|---------|
| `windows_or_buffers_changed` | Dirty flag: 0 = all cached display data is fresh. Non-zero = needs redisplay. |
| `update_mode_lines` | Mode-line dirty flag |
| `redisplaying_p` | Re-entrancy guard: true while redisplay is running |

#### Current Matrix (`w->current_matrix`)

The glyph matrix is read by code OUTSIDE the display engine:

| Consumer | What it reads | Why |
|----------|--------------|-----|
| `window-line-height` (window.c) | `MATRIX_ROW()`, row height/ascent | Return line dimensions to Lisp |
| `window-cursor-info` (window.c) | Row at `phys_cursor.vpos` → glyph | Get glyph under cursor for width/height |
| `pos-visible-in-window-p` (window.c) | Calls `pos_visible_p()` which runs iterator | Check if position is visible |
| neomacsterm.c | Entire matrix: all rows, all glyphs, all areas | Extract for GPU rendering |

#### Layout Query Functions (must be implemented by the display engine)

| Lisp Function | What it computes | How it works |
|---------------|-----------------|--------------|
| `pos-visible-in-window-p` | Is buffer position visible? | Runs display iterator from `w->start` |
| `window-end` | Last visible buffer position | Reads `window_end_pos` or recomputes via iterator |
| `vertical-motion` | Move N visual lines | Runs iterator with wrapping/truncation |
| `move-to-column` | Move to column N | Scans line handling tabs, display props, wide chars |
| `compute-motion` | Position after hypothetical motion | Full layout computation (7 parameters) |
| `posn-at-point` | Pixel position of point | Calls `pos-visible-in-window-p` internally |
| `posn-at-x-y` | Buffer position at pixel coords | Queries glyph matrix rows and glyphs |
| `current-column` | Column number at point | Scans from line start |
| `line-pixel-height` | Pixel height of current line | Runs iterator for one line |
| `format-mode-line` | Rendered mode-line text | Evaluates Lisp format specs, returns text with properties |

#### Callbacks During Layout

| Callback | When | Direction |
|----------|------|-----------|
| `fontification-functions` | Iterator reaches unfontified text | Display engine → Lisp (pauses layout, calls Lisp, resumes) |
| `pre-redisplay-function` | Start of `redisplay_internal()` | Display engine → Lisp |
| `window-scroll-functions` | After window scroll | Display engine → Lisp |
| `redisplay_interface` hooks | After layout, during drawing | Display engine → rendering backend |

## What We Keep vs Replace

### Keep (Emacs C/Lisp)

- Buffer management (gap buffer, undo, markers)
- Text properties and overlays (Lisp-level API)
- Window tree management (splits, sizing)
- Face definitions and merging (Lisp-level `defface`)
- Fontset/font selection logic
- Mode-line format evaluation (`format-mode-line`)

### Replace (Rust)

- The display iterator (`struct it` — 550+ fields state machine in `xdisp.c`)
- Line layout (`display_line()` — wrapping, truncation, alignment)
- Glyph production (`produce_glyphs()` — metrics, positioning)
- Matrix management (`dispnew.c` — desired/current diff)
- The extraction layer (`neomacsterm.c` glyph walking)

## Reading Lisp Data from Rust

Rust needs access to:

| Data | Where it lives | Access pattern |
|------|----------------|----------------|
| Buffer text | Gap buffer (`BUF_BYTE_ADDRESS`) | Contiguous reads around gap |
| Text properties | Interval tree on buffer | Walk intervals for face/display/invisible |
| Overlays | Sorted linked lists on buffer | Scan overlays at each position |
| Faces | `face_cache->faces_by_id[]` on frame | Lookup by ID, merge multiple |
| Window geometry | `struct window` fields | Read pixel_left/top/width/height |
| Window-start | Marker on window | Read marker position |
| Font metrics | `struct font` on face | Ascent, descent, average width |

### Approach A: Snapshot (recommended, start here)

At `update_end`, C serializes a **layout snapshot** to Rust:

```rust
struct LayoutSnapshot {
    windows: Vec<WindowSnapshot>,
}

struct WindowSnapshot {
    id: i64,
    bounds: Rect,
    buffer: BufferSnapshot,
    window_start: usize,
    hscroll: i32,
    selected: bool,
}

struct BufferSnapshot {
    text: Vec<u8>,                      // Full buffer text (no gap)
    intervals: Vec<PropertyInterval>,   // Text property spans
    overlays: Vec<OverlaySpan>,         // Active overlays
}

struct PropertyInterval {
    start: usize,
    end: usize,
    face_id: Option<u32>,
    display: Option<DisplaySpec>,
    invisible: bool,
}
```

**Pros**: Clean Rust ownership, no unsafe, thread-safe.
**Cons**: Copies all buffer text every frame (~0.1ms for 1MB buffer).

### Approach B: Shared-memory / FFI read (optimize later)

Rust reads Emacs data structures directly via `unsafe` FFI pointers:

```rust
unsafe fn read_buffer_char(buf: *const EmacsBuffer, pos: usize) -> char {
    // Handle gap buffer directly
}

unsafe fn get_face(frame: *const EmacsFrame, face_id: u32) -> &Face {
    // Read from face_cache->faces_by_id[face_id]
}
```

**Pros**: Zero-copy, instant access.
**Cons**: Extremely unsafe, must synchronize with Emacs thread, Emacs struct layout changes break everything.

**Recommendation**: Start with Approach A, optimize to B later for large buffers. The snapshot cost is negligible for typical buffer sizes (<100KB).

## Phased Implementation

### Phase -1: Direct Glyph Hook (Low-Risk Stepping Stone)

Before the full rewrite, eliminate the matrix intermediary without changing the layout engine. Keep `xdisp.c` but hook into `produce_glyphs` / `append_glyph` so it calls into Rust directly as glyphs are generated, instead of extracting from `current_matrix` after the fact.

```
CURRENT:   xdisp.c → glyph matrix → neomacsterm.c extracts → FFI → Rust renders
PHASE -1:  xdisp.c → calls Rust directly during produce_glyphs → Rust renders
```

**Benefits**:
- Keeps 100% compatibility (same layout algorithm, all Lisp functions work)
- Eliminates the matrix → extraction → FFI overhead
- Still gets Rust rendering, cosmic-text, GPU batching
- Doesn't break any packages
- Much lower risk (~2000 LOC changes)
- Proves the architecture before committing to the full rewrite

**Does not enable**: Pixel-level scrolling, parallel layout, TUI backend (those require the full rewrite).

**Scope**: ~2000 LOC. **Difficulty**: Medium. **Risk**: Low.

### Phase 0: Layout Snapshot Infrastructure

Add C function `neomacs_build_layout_snapshot()` that serializes buffer text + property intervals + overlays into a flat buffer. Send snapshot to Rust alongside (or replacing) `FrameGlyphBuffer`. Rust ignores snapshot initially, still uses old glyph path.

Note: Display property serialization is complex — 14 display spec types with composability (lists/vectors of specs), overlay strings with their own properties, 5-level nesting.

**Scope**: ~1000 C, ~500 Rust. **Difficulty**: Medium-Hard.

### Phase 1: Monospace ASCII Layout Engine

Build `RustLayoutEngine` that handles the simplest case:

- Fixed-width font, single face, no overlays, no display properties
- Line breaking at window width (or `truncate-lines`)
- Cursor positioning
- Window-start / point tracking
- Tab stops (`tab-width`, `tab-stop-list`)
- Control characters (`^X` = 2 columns, `\NNN` = 4 columns)
- Wide characters (CJK = 2 columns)
- Continuation glyphs (line wrapping indicators)

This alone covers ~70% of what you see in a typical coding buffer.

Must also implement **layout result feedback** to Emacs: `window-end`, cursor (x,y), and visibility info so that `pos-visible-in-window-p`, `vertical-motion`, `move-to-column` etc. return correct results.

**Scope**: ~2500 Rust. **Difficulty**: Medium.

### Phase 1.5: TUI Renderer

See [TUI Rendering Backend](#tui-rendering-backend) section.

**Scope**: ~1200 Rust. **Difficulty**: Medium.

### Phase 2: Face Resolution

- Read face intervals from snapshot
- Apply face attributes (fg, bg, bold, italic, underline)
- Handle face merging (text property face + all overlay faces, priority-ordered)
- Face realization: compute pixel values, select font from fontset
- Use existing cosmic-text for font selection per face
- Unlimited overlays can contribute faces at one position (`face_at_buffer_position` merges all)

**Scope**: ~1500 Rust. **Difficulty**: Medium-Hard.

### Phase 3: Display Properties

This is the most complex phase. Emacs has 14 display spec types with composability:

**Display spec types:**
1. `(when FORM . VALUE)` — conditional display
2. `(height HEIGHT)` — font height adjustment
3. `(space-width WIDTH)` — width scaling
4. `(min-width (WIDTH))` — minimum width padding
5. `(slice X Y WIDTH HEIGHT)` — image cropping
6. `(raise FACTOR)` — vertical offset (fraction of line height)
7. `(left-fringe BITMAP [FACE])` — left fringe bitmap
8. `(right-fringe BITMAP [FACE])` — right fringe bitmap
9. `((margin LOCATION) SPEC)` — margin display (left/right/nil)
10. `(space ...)` — space/stretch specs (`:width`, `:align-to`, `:relative-width`, `:height`)
11. `(image ...)` — image display
12. `(video ...)` — video display (neomacs extension)
13. `(webkit ...)` — WebKit display (neomacs extension)
14. `string` — replacement string with its own properties

**Composability**: Specs can be nested in lists/vectors. The display iterator uses a 5-level push/pop stack (`IT_STACK_SIZE = 5`) to handle nested contexts: display strings inside overlay strings inside display properties.

**Overlay strings** (`before-string` / `after-string`):
- Priority-based sorting (complex ordering: after-strings before before-strings, decreasing/increasing priority)
- Chunked processing (16 at a time via `OVERLAY_STRING_CHUNK_SIZE`)
- Overlay strings can have their own text properties (including face and display properties) — creating recursive layout
- Window-specific overlay filtering

**Additional features in this phase:**
- `invisible` property — skip text ranges (with ellipsis option)
- `line-prefix` / `wrap-prefix` — continuation line indentation
- Fringe bitmaps — 25 standard types + custom (arrows, brackets, indicators)
- Margin areas — `display-line-numbers-mode`, margin display specs
- `format-mode-line` result integration (text with properties from Lisp)

**Scope**: ~5000 Rust. **Difficulty**: Very hard.

### Phase 4: Mode-line, Header-line & Tab-line

- Evaluate mode-line format (stays in Lisp via `format-mode-line`)
- C sends pre-formatted mode-line/header-line/tab-line strings + faces to Rust
- Rust lays out each special row
- Handle `format-mode-line` which evaluates arbitrary Lisp (conditionals, `%e`, `%@`, etc.)

**Scope**: ~1000 Rust. **Difficulty**: Medium.

### Phase 5: Variable-width & Compositions

- Variable-width font support (cosmic-text already handles this)
- Emoji/composition support (already working in current system)
- Ligatures (cosmic-text + rustybuzz)
- Font fallback chains, metric computation

**Scope**: ~1200 Rust. **Difficulty**: Medium.

### Phase 6: Bidi

- Integrate `unicode-bidi` crate for reordering
- Handle mixed LTR/RTL paragraphs
- Integration with ALL of the above: line breaking, cursor movement, overlay strings, display properties
- Every other phase's complexity increases with bidi
- This is the hardest single feature

**Scope**: ~3000 Rust. **Difficulty**: Very hard.

### Phase 7: Images & Media

- Inline images (already rendered by wgpu, just need layout positioning)
- Video/WebKit (same — just need position from layout)

**Scope**: ~400 Rust. **Difficulty**: Easy.

### Phase 8+: Parallel Layout (Future)

Once Phases 0-7 are complete and stable:
- Move layout to render thread for parallel execution
- Implement synchronous RPC for Category C Lisp functions
- Cache layout results for fast Category B queries
- Enable pixel-level smooth scrolling with async window-start feedback

This phase depends on the entire layout engine being correct and stable first.

### Summary

| Phase | LOC (Rust) | Difficulty | Enables |
|-------|-----------|------------|---------|
| -1: Direct glyph hook | ~2000 | Medium | Proves architecture, low risk |
| 0: Snapshot infra | ~1000 C, ~500 Rust | Medium-Hard | Foundation |
| 1: Monospace ASCII | ~2500 Rust | Medium | Basic editing |
| 1.5: TUI renderer | ~1200 Rust | Medium | Terminal Emacs |
| 2: Faces | ~1500 Rust | Medium-Hard | Syntax highlighting |
| 3: Display props | ~5000 Rust | Very hard | Packages (company, which-key, org) |
| 4: Mode-line | ~1000 Rust | Medium | Status display |
| 5: Variable-width | ~1200 Rust | Medium | Proportional fonts, ligatures |
| 6: Bidi | ~3000 Rust | Very hard | International text |
| 7: Images/media | ~400 Rust | Easy | Already working |

**Total: ~17-18k LOC Rust replacing ~40k LOC C.** The reduction comes from:

- No terminal backend code needed
- No X11/GTK drawing code needed
- No incremental matrix diffing (GPU redraws everything)
- Modern Rust text crates handle Unicode/shaping complexity

### Previously Missing Components (Now Included)

These were absent from the original design and are now incorporated into the phases above:

| Component | Phase | Notes |
|-----------|-------|-------|
| Fringe bitmaps (25 standard + custom) | 3 | Arrows, brackets, debugging indicators |
| Margin areas / `display-line-numbers-mode` | 3 | Very commonly used |
| `vertical-motion` / `move-to-column` feedback | 1 | Cursor movement depends on layout |
| `window-end` feedback to Emacs | 1 | Many packages query this |
| jit-lock / fontification integration | 1 | Rust layout must trigger Lisp fontification |
| Minibuffer resize (`resize-mini-windows`) | 3 | Dynamic height based on content |
| Tab-line | 4 | Window tab bar |
| Iterative window-start computation (2-5 passes) | 1 | Scroll policies: `scroll-margin`, `scroll-step`, etc. |
| Child frames | 3 | posframe, etc. — independent layout per frame |

## What This Unlocks

**Immediate benefits (Phases 0-7):**

1. **Unified Rust codebase** — layout and rendering in same language. No more C extraction layer, no FFI serialization per frame.
2. **Memory safety** — no more buffer overflows in display code. Emacs has had CVEs in xdisp.c.
3. **Modern text stack** — cosmic-text + rustybuzz = proper ligatures, OpenType features, font fallback.
4. **Simpler architecture** — no glyph matrix intermediary. Layout produces render-ready data directly.
5. **TUI backend for free** — same layout engine outputs to terminal.
6. **Testability** — Rust unit tests for layout logic. Currently impossible to test xdisp.c in isolation.
7. **Incremental layout** — Rust can diff against previous layout and only re-layout changed regions.
8. **Ligatures everywhere** — cosmic-text + rustybuzz handle OpenType ligatures natively.

**Future benefits (Phase 8+):**

9. **Pixel-level smooth scrolling** — sub-line viewport with async position feedback to Emacs.
10. **Sub-frame cursor movement** — cursor position computed in render thread, animated instantly.
11. **Parallel layout** — layout on render thread while Emacs executes Lisp.
12. **Custom rendering effects** — animated text insertion, per-character fade-in, elastic overscroll.

## Difficulty Analysis

### What's Actually Easy

1. **Monospace ASCII without properties** — count chars, break at width, position on grid. This is what Alacritty does.
2. **Basic face application** — once positioned, applying fg/bg/bold/italic is straightforward table lookup.
3. **Cursor rendering** — already done in current Rust renderer.
4. **Images/video/webkit** — already rendered by wgpu. Layout just reserves space.
5. **TUI output** — quantize pixel coordinates to cell grid, diff, emit ANSI. Well-understood.
6. **Glyph rasterization** — cosmic-text handles this well.

### What's Hard (Ranked)

**1. Display properties + overlay strings (Phase 3)**

The single hardest phase. 14 display spec types with composability, 5-level nesting via push/pop stack, recursive overlay string properties. This is where most Emacs redisplay bugs live. `handle_single_display_spec` alone is 634 lines of C with deeply nested conditionals. The overlay string machinery adds another ~400 lines. All interactions between these systems must be faithfully replicated.

**2. Synchronization with Emacs Lisp**

Layout results must flow back to Emacs for `pos-visible-in-window-p`, `window-end`, `vertical-motion`, etc. These are called synchronously from Lisp and must return correct results immediately. The Rust layout engine must maintain queryable state that matches what these functions expect.

**3. Iterative window-start computation**

Emacs does 2-5 layout passes per frame to find the correct `window-start`:
- Try displaying from current start → check if point is visible
- If not, adjust `window-start` → retry
- Handle `scroll-margin`, `scroll-conservatively`, `scroll-step`
- Three optimization tiers: `try_window_reusing_current_matrix` → `try_window_id` → `try_window`

This iterative process is deeply intertwined with layout and must work correctly.

**4. jit-lock / fontification integration**

Fontification runs DURING layout iteration, calling into Lisp. The Rust layout engine must pause layout, call into C/Lisp for fontification via FFI callback, then resume with the newly-applied faces. This is architecturally messy.

**5. Face realization + merging**

Not just reading a face_id. Involves: merge text property face + ALL overlay faces (unlimited, priority-ordered), then realize (compute pixel values, select font from fontset). `face_at_buffer_position` is ~130 lines, but font selection from the fontset system adds significant complexity.

**6. Bidi (Phase 6)**

Not just "use unicode-bidi crate." The crate handles the algorithm, but integrating bidi with line breaking, cursor movement, overlay strings, and display properties creates multiplicative complexity. Every other phase gets harder with bidi.

### What's Medium

- **Margin areas / line numbers** — `display-line-numbers-mode` is margin-based, needs its own layout per row.
- **Fringe bitmaps** — 25 standard types + custom. Need bitmap rendering pipeline.
- **Mode-line** — `format-mode-line` evaluates arbitrary Lisp. Serialization of results is the hard part.
- **Minibuffer resize** — full buffer scan for height, dynamic window resizing during redisplay.

## Pros vs Cons

### Pros

1. **Unified Rust codebase** — layout and rendering in same language. No C extraction layer, no FFI serialization per frame.
2. **Memory safety** — no buffer overflows in display code.
3. **Modern text stack** — cosmic-text, rustybuzz for ligatures, proper Unicode support.
4. **Simpler architecture** — no glyph matrix intermediary. Layout produces render-ready data.
5. **TUI backend** — same layout engine for terminal rendering. Currently impossible.
6. **Testability** — Rust unit tests for layout. Currently impossible to test xdisp.c in isolation.
7. **Future GPU optimizations** — layout aware of GPU capabilities.
8. **Incremental layout** — diff against previous layout, only re-layout changed regions.

### Cons

1. **Layout must run on Emacs thread** — the "parallel layout" benefit is deferred to Phase 8+. Many Lisp functions need synchronous layout access. This limits the performance gain vs current architecture.
2. **Scope is ~2x original estimate** — ~17k LOC Rust, not ~8-10k. Display properties and missing components account for the increase.
3. **Compatibility risk** — every Emacs package is a test case. Subtle layout differences = visual bugs. Packages like posframe, company, corfu, org-mode, magit depend on exact redisplay behavior.
4. **Display property complexity** — 14 types with composability, 5-level nesting, recursive overlay string properties. The long tail of edge cases will take months.
5. **jit-lock callback** — Rust calling back into Lisp during layout is architecturally messy. FFI boundary goes both directions.
6. **Regression risk** — xdisp.c is battle-tested over 40 years. A rewrite will have bugs the original doesn't.
7. **Two-way data flow** — layout results must flow back to Emacs for Lisp functions. Not just Emacs→Rust, also Rust→Emacs.
8. **Parallel development burden** — must maintain both old and new display engines during multi-phase transition.
9. **Iterative window-start** — the 2-5 pass window-start computation is deeply tied to layout and scroll policies. Getting this wrong breaks scrolling.

## Compatibility Risk

The biggest risk is Emacs packages that depend on redisplay behavior:

- **posframe** — creates child frames at specific pixel positions via `posn-at-point`
- **company-mode / corfu** — popup overlays positioned relative to point via `pos-visible-in-window-p`
- **which-key** — positioned popups
- **org-mode** — heavy use of display properties, invisible text, overlays, `line-prefix`
- **magit** — thousands of overlays for diff coloring, section folding via `invisible`
- **helm / ivy / vertico** — rapid overlay creation/deletion, face changes on every keystroke
- **lsp-mode / eglot** — diagnostic overlays with `after-string`, inline hints

**Functions that MUST return correct results:**
- `pos-visible-in-window-p` — "is buffer position visible?"
- `posn-at-point` / `posn-at-x-y` — pixel-to-position conversion
- `window-end` — last visible buffer position
- `vertical-motion` — move by visual lines
- `move-to-column` — move to specific column
- `compute-motion` — full motion computation
- `current-column` — get current column

**Mitigation**: Phase -1 (direct glyph hook) proves the rendering architecture works without any compatibility risk. Full layout rewrite proceeds incrementally with continuous testing against real packages.

## Alternative Considered: GPU Compute Layout (Strategy 5)

Rejected. GPU compute shaders for text layout would push line breaking, glyph positioning, and wrapping to the GPU. However:

- **Line breaking is inherently sequential** — can't know where line N+1 starts until line N finishes. GPU parallelism doesn't help.
- **Bidi is impossible on GPU** — the Unicode Bidi Algorithm has deeply sequential state (embedding levels, bracket matching). Not expressible in WGSL.
- **Text shaping must stay on CPU** — HarfBuzz/rustybuzz needs CPU access to font tables for ligatures, kerning, mark attachment.
- **Branching kills GPU perf** — overlays, display properties, invisible text, variable-width fonts all require per-glyph branching. GPUs hate divergent branches.
- **No ecosystem** — no existing references for GPU text layout in editors.
- **The bottleneck doesn't exist** — a typical frame has ~3000-8000 visible glyphs. CPU layout for that is <1ms.

Strategy 4 (CPU Rust layout + GPU render) gives 95% of the performance benefit with 10% of the complexity. Every modern editor (VS Code, Zed, Lapce, Alacritty) uses this architecture.

## TUI Rendering Backend

A major benefit of owning layout in Rust: one layout engine, multiple renderers. The same `RustLayoutEngine` that produces glyph batches for wgpu can also output to a terminal grid, giving us a true TUI Emacs.

### Architecture

```
                              ┌─→ WgpuRenderer (GPU)
LayoutSnapshot → RustLayout ──┤
                              └─→ TuiRenderer (terminal)
```

The layout engine produces a backend-agnostic intermediate representation:

```rust
struct LayoutOutput {
    rows: Vec<LayoutRow>,
}

struct LayoutRow {
    glyphs: Vec<LayoutGlyph>,
    y: f32,
    height: f32,
}

struct LayoutGlyph {
    char: char,
    x: f32,
    width: f32,
    face_id: u32,
    is_cursor: bool,
    // ... other attributes
}
```

The **WgpuRenderer** consumes this as pixel-positioned glyph batches (current path). The **TuiRenderer** maps this to a cell grid:

```rust
struct TuiRenderer {
    terminal: crossterm::Terminal,   // or termwiz / ratatui backend
    grid: Vec<Vec<Cell>>,           // rows x cols cell grid
    prev_grid: Vec<Vec<Cell>>,      // previous frame for diffing
}

struct Cell {
    char: char,
    fg: Color,
    bg: Color,
    attrs: CellAttrs,  // bold, italic, underline, strikethrough
}
```

### How TUI Rendering Works

1. **Layout**: `RustLayoutEngine` produces `LayoutOutput` with pixel coordinates
2. **Quantize**: TuiRenderer maps pixel positions to cell grid (divide by cell width/height)
3. **Diff**: Compare current grid against previous grid
4. **Emit**: Output only changed cells via ANSI escape sequences

### Terminal Features

| Feature | GPU (wgpu) | TUI (terminal) |
|---------|-----------|----------------|
| Text rendering | Glyph atlas + shader | ANSI escape sequences |
| Colors | 32-bit RGBA linear | 256-color / 24-bit truecolor |
| Bold/italic | Font variant selection | SGR attributes |
| Underline | Custom pixel drawing | SGR underline (wavy if supported) |
| Images | GPU texture | Sixel / Kitty graphics protocol |
| Cursor | Animated, blinking | Terminal cursor escape |
| Smooth scroll | Pixel-level | Line-level (or pixel with Kitty) |
| Ligatures | Full OpenType | Not possible (cell grid) |
| Variable-width | Full support | Monospace only |
| Box drawing | SDF rounded rects | Unicode box characters |
| Video/WebKit | Inline rendering | Not supported |
| Mouse | Full pixel tracking | Cell-level tracking |
| DPI scaling | Automatic | Terminal handles it |
| Performance | 120fps GPU | 60fps terminal refresh |

### Crate Choices

- **crossterm** — Cross-platform terminal manipulation (input, output, raw mode). Mature, widely used.
- **ratatui** — TUI framework built on crossterm. Provides widget abstractions, but we may only need the backend layer since we have our own layout.
- **termwiz** — Alternative from wezterm project. Better Kitty graphics protocol support.

### TUI-Specific Considerations

**Cell grid quantization**: The layout engine works in pixel coordinates. For TUI, we quantize:
```rust
let col = (glyph.x / cell_width).floor() as usize;
let row = (glyph.y / cell_height).floor() as usize;
```

Wide characters (CJK) occupy 2 cells. The layout engine already knows character widths from cosmic-text; TUI renderer uses `unicode-width` crate to determine cell count.

**Color mapping**: Layout faces use 32-bit sRGB colors. TUI renderer maps to:
- 24-bit truecolor (most modern terminals)
- 256-color palette (fallback)
- 16-color ANSI (minimal fallback)

Detection via `COLORTERM=truecolor` environment variable or terminfo capabilities.

**Inline images**: Modern terminals support image protocols:
- **Kitty graphics protocol** — pixel-perfect, widely supported
- **Sixel** — older but broadly compatible
- **iTerm2 inline images** — macOS terminals

The TUI renderer can optionally support these for `IMAGE_GLYPH` layout items.

**Diffing for performance**: Unlike GPU (clear-and-rebuild each frame), terminals are slow to redraw. The TUI renderer must diff current vs previous grid and only emit changes. This is standard practice (ncurses, crossterm, ratatui all do this).

### Implementation Phase

TUI backend fits as an additional phase after Phase 1 (monospace ASCII):

**Phase 1.5: TUI Renderer**

- Implement `TuiRenderer` that consumes `LayoutOutput`
- Cell grid quantization from pixel coordinates
- ANSI escape sequence output via crossterm
- Grid diffing for incremental updates
- Basic face -> SGR attribute mapping
- Cursor display via terminal cursor

**Scope**: ~1200 Rust. **Difficulty**: Medium.

This phase can proceed in parallel with Phases 2-7 since TUI and GPU renderers consume the same layout output. Each layout feature (faces, display props, bidi) automatically works in both renderers once the layout engine supports it.

### Use Cases

- **SSH sessions** — Full Emacs over SSH without X11 forwarding or GPU
- **Containers / CI** — Emacs in Docker, headless servers
- **Low-resource machines** — No GPU required
- **Terminal multiplexers** — Works inside tmux, screen, zellij
- **Accessibility** — Screen readers work with terminal output
- **Testing** — Deterministic text output for layout regression tests
