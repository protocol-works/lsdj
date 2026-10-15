# Media curation checklist — ratings, rename, tags, playlists

Manual verification of the curation features in the Media Explorer
(`docs/media-curation-plan.md`, ADR-0036). Registry round-trips, sorting,
the rename editor, the tag panel, and the playlist store are unit-tested; this
checklist covers the last hop — real window, real pointer, the FLX4 rotary, and
the drag-and-drop that jsdom cannot exercise.

## Setup

- [ ] `just tauri-dev`, app open, Media Explorer expanded on the Songs tab with
      a few generated songs present.

## Ratings

- [ ] Click the 4th star on a row: four stars light in the accent colour.
      Click it again: the row returns to unrated.
- [ ] Hover across a row's stars: the fill previews up to the hovered star and
      drops back on leave.
- [ ] Switch the accent theme (settings drawer): lit stars follow the accent and
      stay legible on every theme.
- [ ] Relaunch the app: ratings survive.
- [ ] Sort = Top rated: rated rows lead, highest first, unrated last; a
      composing (pending) row still sits at the very top.
- [ ] With Top rated active, turn the FLX4 browse rotary: the highlight walks
      the sorted order; LOAD loads the highlighted top-rated row.

## Rename

- [ ] Double-click a title: an inline editor opens prefilled. Enter commits;
      the row shows the new name and the `#id` tag is unchanged.
- [ ] Open the editor and press Escape: the old title is back, nothing saved.
- [ ] Open the editor, clear it, press Enter: nothing changes (empty is a
      cancel).
- [ ] Open the editor, then click a deck control: the rename commits on blur.
- [ ] Rename a track that is loaded on a deck: the deck keeps the label it was
      loaded with; reloading picks up the new name.
- [ ] "Open songs folder": the file keeps its original stem (by design).

## Tags

- [ ] ⋯ → Tags… on a row, type `Deep House`, Enter: the chip reads
      `#deep-house` (normalised shell-side) and the field clears.
- [ ] On a second row, open Tags…: `deep-house` is offered as a checkbox; tick
      it. Both rows now show the chip.
- [ ] Click a chip: the search box fills with `#deep-house` and the list narrows
      to those rows. Clear the search.
- [ ] Type `#deep-house #banger` (after tagging one row with both): only rows
      carrying both tags remain.

## Playlists

- [ ] "+ New playlist" in the source column, name it, Enter: it appears
      selected and empty with the hint text.
- [ ] Drag a row (anywhere on it) onto the playlist in the column: the target lights
      while hovering; on drop the count increments. Drop the same row again: no
      duplicate.
- [ ] Select the playlist: rows appear in playlist order with a ⋮⋮ grip at
      the left. Drag a row above
      another: a drop-indicator line shows between rows; the order updates and
      survives a relaunch.
- [ ] In the playlist, ✕ on a row removes it from the playlist only; the song is
      still in All songs.
- [ ] ⋯ → Delete from library on a song that is in a playlist: it leaves both
      the library and the playlist.
- [ ] Rename the playlist (double-click its name in the header above the list,
      or its ⋯ → Rename playlist); the file in `generated_songs/playlists/` is
      renamed too.
- [ ] Narrow the window below ~52rem: the source column becomes a select above
      the list; the same playlist is still selected.
- [ ] Rotary: with a playlist selected, the highlight walks the playlist order
      and LOAD loads the highlighted row.
- [ ] Finder → `generated_songs/playlists/<name>.m3u8` opens in Music.app or
      VLC and plays the songs in order.

## Set flow

- [ ] Load a song onto deck A: its row shows "On A"; load another onto B: "On B".
      Eject A: the badge clears and the row keeps a muted "played" state for the
      rest of the session.
