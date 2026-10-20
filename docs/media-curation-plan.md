# Media Explorer curation plan — ratings, renaming, tags, playlists, sort, and row polish

Status: implemented on `media-curation` (2026-10-14), uncommitted, awaiting
the human checklist (`docs/media-curation-checklist.md`). Built in five
PR-sized phases (tags, then playlists, each their own PR). Nothing here touches audio; the only hardware-visible change is
the browse order the FLX4 rotary walks when a sort is active.

## What exists today

- Both libraries are a folder plus `registry.json`, reconciled against disk on
  every list (`src-tauri/src/songs.rs`, `samples.rs`, shared helpers in
  `library.rs`). The registry is the only place provenance lives, and the folder
  watcher deliberately ignores it, so a registry-only write never loops.
- A row's `title` is display only; its `file` is the registry identity, minted
  once from the title at save time (`library.rs` says it outright: "the file is
  just an identifier"). MCP `load_track`, the deck's `TrackSource`, and the
  explorer's re-list all key on `file`, never on the title.
- The Generate and Samples tabs render one row per entry: name + `#id` + prompt
  preview · engine · [Reuse settings] · 🎧 · →A →B · ✕. Lists are newest-first
  and filtered by the header search (`matchesSearch`, AND over fields).
- The FLX4 rotary moves a highlight through the visible tab; LOAD loads it. Only
  the Folder tab lets a mouse click set that highlight — Generate/Samples rows
  don't (a consistency gap this plan closes in phase 2).
- `list_songs` / `list_samples` over MCP return the registry rows verbatim, so a
  new registry field reaches the co-DJ for free.
- The deck knows the loaded track's label (`title #id`), bpm, and duration, but
  not its library file.
- `ui/TextField` forwards every native input prop (autoFocus, onKeyDown,
  onBlur) and has a `labelHidden` mode, so an inline editor is a configuration
  of it, not a new primitive.

Library today: 78 songs, 12 samples, all generated. Enough that a rating is
useless without a way to surface the keepers, so sort ships with the stars. The
samples side is where renaming earns its keep: freezes and pads auto-name
themselves, so "A freeze 3" has to become "vocal chop" somewhere.

## Decisions

1. **Rating lives in the registry** as `rating: Option<u8>` (1–5; absent =
   unrated) on `SongEntry` and `SampleEntry`, serialised with
   `skip_serializing_if = "Option::is_none"` so old rows and old registries stay
   readable. `reconcile` keeps whole entries, so a rating survives a scan; a
   hand-renamed file loses it exactly as it loses its prompt today (accepted).
   Rejected: a sidecar file (second source of truth) and webview localStorage
   (the shell owns library state, ADR-0020/0030, and MCP needs to see it).
2. **Rating and rename are per library row.** Songs and samples get them; the
   Folder tab (arbitrary external folders, no registry) and Presets (styles,
   not tracks) do not. A per-folder sidecar is a documented non-goal.
3. **Clearing is a click on the current star.** 0 means unrated; no separate
   clear button, no half stars, fixed max of 5.
4. **Sort is a per-user preference** (`mediaSort: 'newest' | 'rating' |
   'title'` in `AppSettings`, validated like `generationMode`). It applies to
   Generate and Samples; pending (composing) rows always pin to the top. The
   rotary walks the sorted, filtered order — top-rated first is exactly what
   you want on the controller.
5. **The star control is a design-system component** (`ui/Rating.tsx`), not a
   screen-local one-off (`.claude/rules/design-system.md`). Spec below; it is
   the only new primitive.
6. **Rename changes the title, never the file.** The file is the stable
   identity everything else holds: an MCP agent mid-set holding a `file`, a
   deck's `TrackSource`, the in-session row's id and in-memory `wav`, and the
   watcher's re-list (a file rename would read as delete + create and mint a
   fresh row, dropping the `#id` and the session bytes). Keeping the on-disk
   name means a rename is a registry write like a rating, with none of that
   churn. The cost is cosmetic: "Open songs folder" shows the original stem.
   Same rule the codebase already applies to a hand-renamed file.
7. **Validation happens shell-side.** The new title is untrusted webview input:
   trimmed, non-empty, control characters rejected, capped at 200 chars (a
   display label, so a looser cap than the 80-char file stem). The webview
   also refuses an empty commit, but the Rust command is the boundary.
8. **Tags live in the registry too**, as `tags: Vec<String>` on both entry
   types (`skip_serializing_if = "Vec::is_empty"`), one shared vocabulary for
   songs and samples. A tag is a normalised word: trimmed, lowercased, a
   leading `#` dropped, inner whitespace to `-` (`deep house` → `deep-house`),
   deduped, at most 32 chars and 16 tags per row, control characters refused —
   all shell-side, with the row adopting the entry the command returns, so
   normalisation exists in one place. There is no tag entity: the vocabulary
   is the union of tags in use, derived in the webview from the two lists it
   already holds. Tags describe ("drone", "banger"); playlists sequence. A
   `#banger` search plus rating sort is the smart playlist, so none is built.

## Component spec — `ui/Rating`

- **Purpose:** show and set a 0–5 star rating inline in a dense list row.
- **API:** `value: number` (0–5), `onChange(value: number)`, `label: string`
  (the group's accessible name, e.g. "Rating for Dub #3"). No `max`, no
  `size` variants until a second caller needs them.
- **Markup:** a `<div role="radiogroup" aria-label={label}>` holding five
  native `<input type="radio">` (visually hidden) each with a `<label>`
  carrying the `★` glyph. Native radios give Tab-into-group and arrow-key
  selection for free; the only JS is the controlled `checked` and an `onClick`
  on the checked radio that emits 0 (a native radio can't un-check). Labels:
  `media.rating.star` with i18next plurals ("1 star", "{{count}} stars");
  group `title`: `media.rating.clearHint` ("Click the current star to clear").
  The component takes its labels as props, so the primitive stays
  namespace-free.
- **States:** off (`--color-text-silkscreen`), on (`--color-accent`, so stars
  follow the chosen theme hue — amber `--color-led-warn` is the alternative if
  you want them to read as "gold" regardless of theme), hover preview (fill up
  to the hovered star, pure CSS via `:has(~ label:hover)`, no JS), focus ring on
  the group via `:focus-visible` on the hidden input's label. Same glyph for on
  and off so the row never jitters.
- **Tokens:** `--text-s` glyph size, `--space-1` gap, colours above. No new
  tokens.
- **Where it sits in the row:** between the name cluster and the engine meta,
  `flex: none`, so it is scannable in a column and never shares a hit target
  with 🎧 / →A / →B / ✕. Pending rows render nothing there.

## Rename UX

- **Entry points:** a ✎ button per ready row (aria-label `media.rename.action`,
  "Rename {{name}}") — the keyboard- and screen-reader-reachable path — plus
  double-click on the name as the mouse shortcut. A rename affordance that only
  exists as a gesture would fail the accessibility floor, so the button is not
  optional. (An overflow "…" menu on `ui/AnchoredPanel` would de-clutter the
  row, but that is a second primitive for a row that still fits at desktop
  width; revisit if a sixth action lands.)
- **The editor:** the name cluster swaps for a `TextField` (`labelHidden`,
  label `media.rename.label` "New title for {{name}}", `autoFocus`, prefilled
  with the current title). Enter commits, Escape cancels, blur commits. The
  `#id` tag and prompt preview stay beside it so the row keeps its shape. One
  row edits at a time (`editingId`, like `expandedId`).
- **Commit:** trim; unchanged or empty → cancel silently. Otherwise set the row
  title optimistically, invoke the rename command, and on failure show
  `media.rename.failed` and refresh the library to resync. A session-only take
  with no file (outside Tauri, or a failed save) just updates the row, the same
  branch `removeTrack` takes.
- **What does not change:** the file on disk, the `#id`, the prompt, the
  rating, the recipe. A track already loaded on a deck keeps the label it was
  loaded with until it is reloaded — the deck label is a snapshot by design
  (`TrackIdentitySnap`), and hot cues key on that snapshot, so leaving it alone
  is the safe choice. Search matches the new title immediately, since it reads
  the row.

## Phase 1 — stars, rename, sort (one PR)

Rust (`src-tauri/src/`):
- `library.rs`: one generic `update_entry<T>(dir, file, apply)` that loads the
  registry, finds the row by `file` (error if absent), applies the closure,
  saves, and returns the row. Both libraries' rate and rename become
  three-line methods under their own lock, instead of four copies of the
  read-modify-write `remove` already does.
- `songs.rs`, `samples.rs`: add `rating: Option<u8>` to the entry structs and
  the `reconcile` hand-added defaults; `rate(name, Option<u8>)` validates
  `1..=5`; `rename(name, title)` validates per decision 7. No path resolution
  is needed — the name is matched against registry rows, never the filesystem.
- `commands.rs`: `rate_generated_song` / `rate_generated_sample(name, rating)`
  and `rename_generated_song` / `rename_generated_sample(name, title)` next to
  the delete commands, each returning the updated entry; register all four in
  `lib.rs`.
- `mcp.rs`: amend the `list_songs` / `list_samples` descriptions: "`rating`
  1–5 when the DJ has rated it; prefer higher-rated material". No new tool yet.
- Tests: rate and rename round-trip through a fresh library instance; rate
  rejects 0/6 and an unknown file; rename rejects empty, whitespace-only, and
  over-long titles and leaves file/prompt/model/recipe/rating untouched;
  reconcile keeps a rating; an old row without `rating` deserialises.

Frontend (`frontend/src/`):
- `ui/Rating.tsx` + styles in `ui/ui.css`.
- `media/MediaExplorer.tsx`: `rating: number` on the ready variants of
  `GeneratedTrack` / `GeneratedSample` (0 when absent); `rateTrack` /
  `rateSample` and `renameTrack` / `renameSample` set the row optimistically,
  invoke the command, and on failure surface the error and call the library's
  refresh to resync. **Fix the re-list to rebuild row fields from the registry,
  keeping only the prior row's `id` and in-memory `wav`** — today
  `reListLibrary` reuses the whole old row by filename, which would keep a
  stale rating or title after an out-of-band write (an MCP rate in phase 2, or
  a resync after a failed command).
- Rename editor and ✎ button per the UX section; i18n keys under
  `media.rename.*` and `media.rating.*`.
- Sort: a `Select` in the header actions (Generate/Samples only) with
  `media.sort.newest` / `media.sort.rating` / `media.sort.title`; `sortRows`
  is a stable sort (rating desc, or title case-insensitive) with pending rows
  first; persisted via `updateAppSettings`. Title sort is three lines and is
  what makes renamed libraries browsable, so it rides along.
- Tests: Rating (click sets, click current clears, five labelled radios,
  arrow keys); MediaExplorer (rows show the registry rating, a click invokes
  the rate command with `{ name, rating }`; ✎ opens an input prefilled with
  the title, Enter invokes the rename command with `{ name, title }` and the
  row relabels, Escape restores, an unchanged or empty commit sends nothing;
  rating and title sorts order rows and keep pending on top; a failed command
  resyncs); persistence (`mediaSort` validates and defaults to newest).

Docs: a short `docs/media-curation-checklist.md` — rotary walks the sorted
order, LOAD loads the highlighted top-rated row, stars are legible under each
accent theme and at the ≤52rem breakpoint where the row wraps, a rename
commits on blur when you click away to the deck.

## Phase 2 — act on ratings without the mouse

- **Click-to-highlight in Generate/Samples**, matching the Folder tab: the name
  becomes the same `media__name--button`. This removes the one place the three
  tabs disagree, and gives the keyboard a target. Double-click still opens the
  rename editor (first click highlights, which is harmless).
- **Keys 1–5 rate the highlighted row, 0 clears**, handled in the explorer's
  existing bus/keyboard path (not `shortcuts.ts`, which is focus-only), ignored
  while typing — including inside the rename editor. Audition with 🎧, press a
  number: the fastest curation loop.
- **MCP `rate_song` / `rate_sample(file, rating)`** mirroring `load_track`'s
  argument shape; after a write the tool emits `library://changed` itself
  (the watcher ignores `registry.json` on purpose) so the open tab re-lists.
  Add a line to `.agents/skills/lsdj/SKILL.md`: the co-DJ may record a rating
  the human states out loud and should bias picks toward rated material.

## Phase 3 — context in the row (optional)

- **"On A" / "On B" badge** on the row loaded on a deck. Key it on the row id
  the explorer captured at load time (it is the caller of `onLoadTrack`), not
  on a title match against the interface store — titles now change under a
  rename, and the deck keeps the old label anyway. Clear the badge when the
  store reports the deck's track as null. Cheap, and it answers "which one is
  playing?" while you curate. Rating or renaming the playing track from the
  deck itself would need the library file in `TrackIdentitySnap`; not worth
  it while the badge plus a row click covers the flow.
- **Length in the meta column** (`recipe.seconds` for a versioned recipe,
  blank otherwise) — picking between takes is easier with a duration.

## Phase 4 — row menu and tags (its own PR)

### Row actions grow up: a menu

With rename, rating, tags, and (next) playlists the row would carry eight or
nine controls. Instead: visible are ★, 🎧, →A, →B; everything else moves into a
⋯ menu — Rename, Tags…, Reuse settings, Delete from library, and in phase 5
Add to playlist…, Move up / Move down. Fewer controls, more actions, and the
destructive one is no longer a bare ✕ beside the load buttons (phase 5 brings
a ✕ back inside a playlist view, where it only removes from the playlist). Double-click still renames; keys 1–5 still rate. The ✎ button from
phase 1 folds into the menu here (or the menu is pulled into phase 1 — open
question 4).

New primitive `ui/ActionMenu`: a trigger `Button` plus `AnchoredPanel` holding
a vertical list of items `{ label, onSelect, disabled?, danger? }` with
`role="menu"`/`menuitem`, arrow-key movement, Escape to close, focus return to
the trigger (AnchoredPanel already does the last two). No nested submenus:
"Tags…" and "Add to playlist…" close the menu and open their panel on the same
anchor. Tokens: existing panel surface/border, `--color-led-danger` text for a
`danger` item.

### Tag UX

- **In the row:** tags render as small `#drone #banger` chips after the prompt
  preview, in the muted silkscreen style of the engine column, `flex: none`
  so the prompt preview is what shrinks. Clicking a chip puts `#drone` in the
  search box — the search syntax teaches itself the first time it is used.
- **Searching:** zero new search code. Each tag is passed to `matchesSearch`
  as `#drone`, so `#drone` matches tags only, `drone` matches tags and prompt
  text alike, and `#drone #banger` is an AND, as every search already is.
  `#banger` plus the rating sort is "the best bangers" with nothing else built.
- **Editing:** ⋯ → "Tags…" opens a panel anchored to the row: the known
  vocabulary as checkboxes (alphabetical, so a tag typed once is a click
  thereafter and spellings stay consistent) above an "Add a tag…" field that
  commits on Enter. Every toggle sends the row's whole tag list to the shell;
  the row adopts what comes back. The panel is the keyboard and screen-reader
  path; there is no gesture-only way to tag.
- **Fast path:** in phase 2's key handling, `t` on the highlighted row opens
  the panel with the field focused — audition, rate with a number, tag with a
  word, next.
- **Samples** get the same chips and panel; "riser", "vocal-chop", "kick" is
  where a shared vocabulary pays off most.
- Not built: tag colours, hierarchies, a tag-management screen (renaming or
  deleting a tag everywhere is a one-off maintenance command when someone
  needs it), tags inside the title as hashtags (they would pollute the deck
  label), and a tag facet bar above the list (search covers it; a "Tags"
  group in the phase 5 source column is the fallback if browsing by tag
  without typing proves necessary — open question 7).

New primitive `ui/ChecklistPanel`: `AnchoredPanel` holding a labelled list of
checkbox options `{ value, label, checked }` with `onToggle(value)`, plus an
optional add field `onAdd(text)` with its own label. Tags use it with the
vocabulary; phase 5's "Add to playlist…" uses it with the playlists and "New
playlist…". Two callers, one component, built here.

### Shell and MCP

- `songs.rs`, `samples.rs`: `tags: Vec<String>` on the entries and the
  `reconcile` defaults; `tag(name, tags) -> Entry` normalises per decision 8
  through the same `update_entry` helper as rate and rename.
- `commands.rs`: `tag_generated_song` / `tag_generated_sample(name, tags)`,
  registered in `lib.rs`. Three field commands per library, all the same shape.
- `mcp.rs`: `list_songs` / `list_samples` carry `tags` for free; amend the
  descriptions ("`tags` are the DJ's own words for the material — honour them
  when asked for a vibe"). A `tag_song(file, tags)` tool rides with the phase 2
  `rate_*` tools if the co-DJ should be allowed to tag; off by default.

### Tests

- Rust: normalisation (case, `#`, whitespace, dedupe, caps, control
  characters refused); round-trip through a fresh library; an old row without
  `tags` deserialises; reconcile keeps tags.
- Frontend: chips render from the registry; a chip click sets the search to
  `#tag`; `#tag` search matches tags and not an unrelated prompt; the panel
  toggle and add paths invoke the command with the full list and the row
  adopts the returned entry; ActionMenu (arrow keys, Escape, focus return,
  danger item) and ChecklistPanel (toggle, add on Enter) unit tests.

## Phase 5 — playlists (its own PR)

### What a playlist is here

An ordered list of songs with a name. Not samples (the pads are their home; a
mixed list would need two load paths and two row shapes in one view), not
Folder-tab files (no identity to reference — "Import to library", below, closes
that gap instead). No colours, descriptions, or smart rules in v1: a rating sort
already answers "the good ones", and a saved search can become a smart playlist
later without changing this model.

### The UX model: a source column, not a fifth tab

- The Generate tab gets a two-column body under its compose form: a narrow
  **source column** on the left (All songs, then each playlist, then "+ New
  playlist") and the existing row list on the right. Selecting a source
  filters the list. Search, sort, rating, rename, 🎧, →A/→B, the rotary and
  LOAD all keep working on that list exactly as before — nothing is relearned,
  and there is still one row renderer.
- Why not a tab: it would clone the row list, split "library" from
  "playlist" into two places, and add a fifth stop to the rotary-press tab
  cycle that every user pays for. Every DJ tool puts the tree on the left and
  the tracks on the right for the same reason.
- The tray is wide and short, so a ~12rem column costs little. Below the
  existing 52rem breakpoint it collapses to a `Select` above the list — same
  choice, one control.
- Rename the tab from "Generate" to "Songs" (a one-string change to
  `media.tabs.generate`): it is the song library, and "Generate" describes only
  its top row. Samples keeps its name.
- The selected source persists in `AppSettings` (`mediaSource`), validated
  against the playlist list on load and falling back to All songs.

### Getting songs in

- **Drag** a row by its name onto a playlist in the column (native HTML5 drag
  and drop; the target lights with the accent inset border; drop appends,
  idempotently). Requires `"dragDropEnabled": false` on the main window in
  `tauri.conf.json` — Tauri's file-drop interception otherwise swallows in-page
  drags on macOS, and nothing in the app uses file drops (the Folder tab uses
  the native picker).
- **Keyboard / screen reader path:** ⋯ → "Add to playlist…" opens the phase 4
  `ChecklistPanel` with the playlists as checkboxes (membership toggles, so
  the same panel removes) plus "New playlist…". This is the required path;
  drag is the shortcut.
- Multi-select (shift-click a range, then add) is deferred; drag plus the panel
  covers v1.

### Inside a playlist

- The same rows in **playlist order**. The sort select gains "Playlist order"
  as its default here; rating/title sorts stay available for scanning but are
  view-only — the stored order changes only by reordering.
- **Reorder** by dragging rows (a drop-indicator line between rows), or from the
  menu: Move up / Move down.
- **In a playlist view a visible ✕ returns, and it means "Remove from
  {{playlist}}"** — never delete. After phase 4 the library delete lives only
  in the ⋯ menu ("Delete from library", a confirmation-free Trash like today),
  so the only bare ✕ anywhere in the explorer is the non-destructive one. The
  two must be impossible to confuse; this is the single most important detail
  in the feature.
- Header: the playlist name (double-click or ✎ to rename, the same inline
  editor as a track), the count, and the total length once the length column
  exists. Empty state: "Drag songs here, or use ⋯ → Add to playlist."
- Deleting a song from the library (⋯ → Delete from library, or MCP) also
  removes it from every playlist, so a playlist never shows a ghost.

### Set-flow touches

- Rows loaded onto a deck **this session** take a muted "played" state, and the
  row currently on a deck shows the phase-3 "On A"/"On B" badge. Both come from
  one session map the explorer already has the data for (it is the caller of
  `onLoadTrack`). A playlist then reads like a set sheet: done, playing, next.
- The rotary walks the playlist in order, so the hardware walks the set.
- Deferred: auto-load the next track, hardware playlist switching (Shift +
  rotary press), persisting played state across launches.

### Store: one `.m3u8` per playlist

- `generated_songs/playlists/<name>.m3u8`: `#EXTM3U`, then per song an
  `#EXTINF:-1,<title>` line (readability only; the registry stays
  authoritative) and a relative path (`../Dub.wav`). A standard, hand-editable,
  Finder-visible format that other players and DJ software import; "Open
  songs folder" already reveals it, and copying the folder carries the sets.
- Name = file stem via `safe_stem`; rename = file rename (nothing holds a
  playlist by identity across IPC except the UI's selection, which the rename
  command returns). Collisions refused, not suffixed.
- Reconcile on list: lines whose file is missing are dropped (the folder is the
  truth, as for the registry); duplicates keep the first occurrence.
- Commands: `list_playlists() -> Vec<{ name, files }>` (tiny; one call),
  `save_playlist(name, files)` for create, add, remove, and reorder alike — the
  webview sends the whole new list and the shell writes it under the songs
  lock, so there is one write path and no per-operation races —
  `rename_playlist(from, to)`, `delete_playlist(name)` (Trash).
- The watcher ignores `playlists/` (not audio). The webview re-lists after its
  own writes; MCP writes emit `library://changed` with `library: "playlists"`
  (`subscribeLibraryChanged` gains the third kind).
- Alternative: a single `playlists.json`. Slightly less code, nothing else
  gained. Pick m3u8 unless interop is unwanted.

### MCP

`list_playlists` and `save_playlist(name, files)`: the co-DJ can build a set
("five of the best-rated dub tracks, warm-up to peak") and the human watches it
appear in the column — "the best-rated `#banger`s" is one `list_songs` call
away once tags exist. `load_track` is unchanged. Second PR of the phase, with
a line in `.agents/skills/lsdj/SKILL.md`.

### Tests and checklist

- Rust: m3u8 round-trip (titles with commas and `#`, non-ASCII names);
  reconcile drops missing files and dedupes; rename collision refused; song
  delete strips the file from every playlist.
- Frontend: pure helpers (insert, move) unit-tested; the source column filters
  the list; ✕ in a playlist calls `save_playlist` without the file and never
  the delete command; menu paths (add, remove, move up/down); a `#tag` search
  inside a playlist narrows it (tags and playlists compose); the narrow-width
  `Select` fallback; a persisted source whose playlist is gone falls back to
  All songs. Drag and drop is not testable in jsdom and goes on the checklist.
- Checklist: drag a row to a playlist, reorder with the indicator line, rotary
  walks playlist order, the `.m3u8` opens from Finder in Music.app or VLC.

### Fallout worth doing

- **"Import to library"** on Folder-tab rows: copy the file into
  `generated_songs` (scoped read, `unique_wav_path`-style naming keeping the
  extension) so it becomes a first-class row with rating, rename, and
  playlists. One command instead of sidecar stores for external folders.

## Rejected / deferred, and why

- Renaming the file on disk alongside the title: see decision 6. If a Finder
  view that matches the titles ever matters, the right shape is a one-shot
  "rename files to match titles" maintenance command that also rewrites
  the registry's `file` keys under the lock, not a per-row behaviour.
- An MCP `rename_*` tool: `generate_track` already takes a `title`, and the
  co-DJ has no reason to rename what the human curates. Add only if asked.
- Bulk "trash everything under 2 stars": destructive, and ✕ already trashes
  recoverably one at a time. Revisit only if the library grows past what a
  sorted list handles.
- A minimum-rating filter: with ~80 rows, rating sort already puts the keepers
  on the first screen. Add a "4★+" toggle only if sort proves not enough.
- Hardware rating from the FLX4: every pad and button is already mapped
  (`docs/midi-ddj-flx4.md`); keys 1–5 cover it.
- Searching by typing `★★★★`: free to add (`'★'.repeat(rating)` into
  `matchesSearch`) but undiscoverable; the sort select is the honest control.
- Smart playlists: a `#tag` search plus a sort already is one; "save this
  search" is the whole feature if anyone asks for it.
- Mixed song + sample playlists: two load paths and two row shapes in one
  view; pad banks are the samples' set-prep story.

## Open questions

1. Star colour: theme accent (proposed) or fixed amber?
2. Is "top rated" sort enough for phase 1, or do you want the "4★+" filter
   toggle in the same PR?
3. Phase 2's number keys: should they also work with no highlight by rating
   the track loaded on deck A? (Would pull the phase-3 badge plumbing forward.)
4. Row menu timing: build `ActionMenu` in phase 1 so ✎ never ships as a loose
   button, or let phase 1 ship ✎ and fold it into the menu in phase 4?
5. Playlist store: `.m3u8` per playlist (proposed, for interop) or one
   `playlists.json`?
6. Rename the Generate tab to "Songs" when playlists land?
7. Tags: search-only (proposed), or also a "Tags" group in the source column
   for browsing without typing?
8. Multi-word tags: fold to `deep-house` (proposed, so a tag is one search
   term), or keep spaces?

## Implementation contract (resolved defaults + IPC)

Open questions resolved for the first implementation (change by editing this
section): accent-coloured stars; sort only, no "4★+" filter; number keys act
on the highlighted row only; phase 1 ships the ✎ button and phase 4 folds it
into the menu; `.m3u8` playlists; the Generate tab becomes "Songs" when
playlists land; tags are search-only; multi-word tags fold to `deep-house`.

Tauri commands (JS `invoke` args are camelCase objects; Rust params are
snake_case; every mutation returns the updated row so the webview adopts the
shell's normalised values):

- `rate_generated_song({ name, rating })` → `SongEntry`. `rating` is 1–5 or
  `null` to clear; out of range or unknown `name` → error string.
- `rate_generated_sample({ name, rating })` → `SampleEntry`.
- `rename_generated_song({ name, title })` → `SongEntry`. Title trimmed,
  non-empty, no control characters, ≤ 200 chars.
- `rename_generated_sample({ name, title })` → `SampleEntry`.
- `tag_generated_song({ name, tags })` → `SongEntry`. Tags normalised per
  decision 8; the returned entry carries the normalised list.
- `tag_generated_sample({ name, tags })` → `SampleEntry`.
- `list_playlists()` → `Playlist[]`, `Playlist = { name: string, files:
  string[] }`, files reconciled against the songs folder, deduped, in file
  order.
- `save_playlist({ name, files })` → `Playlist`. Create or replace; the name
  goes through `safe_stem`; files not in the songs folder are dropped; dedupe
  keeps the first occurrence.
- `rename_playlist({ from, to })` → `Playlist`; a collision is refused.
- `delete_playlist({ name })` → void (Trash).
- `delete_generated_song` additionally removes the file from every playlist.

Registry rows: `SongEntry` / `SampleEntry` gain `rating?: number` (1–5, absent
when unrated) and `tags?: string[]` (absent when empty); both default on read.

Events: `library://changed` payload `{ library: 'songs' | 'samples' |
'playlists' }`. Shell-side writers that the watcher cannot see (MCP tools
writing a registry field or a playlist) emit it themselves.

MCP tools: `rate_song(file, rating)`, `rate_sample(file, rating)`,
`list_playlists()`, `save_playlist(name, files)`; the `list_songs` /
`list_samples` descriptions mention `rating` and `tags`.

Persistence (`frontend/src/persistence.ts`): `AppSettings.mediaSort:
'newest' | 'rating' | 'title'` (default `newest`); `AppSettings.mediaSource:
'all' | string` (a playlist name; default `'all'`; validated against the
playlist list at use time, falling back to `'all'`).

Config: `src-tauri/tauri.conf.json` main window `"dragDropEnabled": false`.

Shell as built (2026-10-14), where it refines the contract above:
- `save_playlist` / `rename_playlist` refuse a blank name rather than falling
  back to a file called `playlist`; a new name is sanitised through
  `safe_stem` and the returned `name` is the sanitised one (`Warm/up` →
  `Warm-up`). Saving under an existing name replaces that playlist.
- `rename_playlist` collision checks ignore case (macOS volumes usually do); a
  case-only change of a playlist's own name is allowed.
- `list_playlists` returns playlists sorted by name ignoring case; each
  playlist's `files` keep m3u8 order, reconciled against the songs folder and
  deduped on read without rewriting the file (a song restored from the Trash
  before the next save reappears).
- MCP `rate_song` / `rate_sample`: omitting `rating` clears it.
- `watcher::emit_library_changed(app, library)` is the one emitter for
  `library://changed`; the watch is non-recursive and audio-only, so
  `playlists/` writes never trigger it.
