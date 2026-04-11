# Direction Filter for Insight Message Table

Add a second filter row above the existing message-type filter so users can
filter recorded messages by direction (inbound / outbound) in addition to type.

## UI (`tools/insight/src/gui/message_table.rs`)

- New `show_direction_labels` method, rendered as the first row in the bottom
  filter panel (above `show_message_type_labels`).
- Two selectable-label chips: `"in(N)"` / `"out(N)"`, `"in"` first.
  - No leading `"Direction:"` label.
  - No `ui.separator()` between the new row and existing rows.
- `MessageTableViewModel` gains four flat fields (no new option struct):
  - `inbound_selected: bool`
  - `outbound_selected: bool`
  - `inbound_count: usize`
  - `outbound_count: usize`
- Both selected flags default to `false` on startup.
- Clicking a chip toggles its flag and calls a new
  `update_direction_filter` method on the view model.

## Filter model (`tools/insight/src/message_collection.rs`)

- `MessageFilter` gains two flat fields: `inbound: bool`, `outbound: bool`.
- New builder `with_directions(inbound: bool, outbound: bool) -> Self`.
- New predicate `include_direction`:
  - If `!inbound && !outbound`, return `true` (matches the "empty = all"
    convention of `include_message_type`).
  - Otherwise return
    `(inbound && msg.direction == Inbound) || (outbound && msg.direction == Outbound)`.
- `MessageFilter::include` chain extended to include `include_direction`.
- New `MessageCollection::filter_directions(inbound: bool, outbound: bool)`.

## Counts (`tools/insight/src/message_collection.rs`)

Replace the `message_counts: HashMap<MessageType, usize>` field and its
`message_counts()` getter with a struct-based shape:

```rust
pub(crate) struct FilterCounts {
    pub types: HashMap<MessageType, usize>,
    pub inbound: usize,
    pub outbound: usize,
}
```

- `MessageCollection` stores `counts: FilterCounts`, exposed via
  `counts() -> &FilterCounts`.
- Cross-filtered counting in `set_filter` and `add`:
  - **Type counts** are tallied when
    `include_channel && include_direction && include_message_content` — the
    new direction gate is what makes type counts reflect the current
    direction selection.
  - **Direction counts** are tallied when
    `include_channel && include_message_type && include_message_content`.

## Tests (`tools/insight/src/message_collection.rs`)

- One test: direction filter includes only matching messages, including the
  `(false, false) = show all` case.
- One test: cross-filter counts — selecting a direction shrinks type counts
  accordingly, and selecting a type shrinks direction counts accordingly.

## Callers to update

- `tools/insight/src/gui/message_table.rs` `update_message_counts`: consume
  the new `counts()` return shape, populate both the existing type `Vec` and
  the four new flat direction fields.
