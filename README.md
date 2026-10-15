# durability-check-mod

The web client's **Equipment Durability** tracker as a ClassicUO mod: a small movable
panel that lists the worn items whose tooltip (OPL) carries hit points, each with its
art, name and a durability bar.

* **Bands** — teal while fine, amber at or below the warning threshold, red (bold name,
  tinted row) at or below the danger threshold.
* **Overall** — header shows the mean durability over every worn item, coloured by the
  worst item's band.
* **Order** — lowest percent first, so the most urgent item is always on top.
* **Alerts** — a journal line (`Broadsword at 23%`, hue 53 / 0x21) when an item drops
  into a heavier band (ok→warn, ok→danger, warn→danger). Recoveries stay quiet.
* **Hover** — the client's own item tooltip. **Double-click** a row — the item goes to
  the backpack.
* **`-` / `+`** — minimize to the single worst item. **`MOVE` / `LOCK`** — drag on/off.
* Live: a hit, a repair, equip / unequip refresh the panel (the client re-requests the
  OPL on every revision bump).

Settings live on **Options → Mods → Durability** (per profile): enable, warning (60%)
and danger (30%) thresholds, alerts, hide items above N% (100 = off; overall and alerts
still cover every item), panel size 100–250%, readout percent / values, item names.

Position, lock and minimized persist in `Data/Mods/durability-check/storage.json`.

## Build

Rust + the `wasm32-wasip2` target. The SDK is a pinned git dependency (`Cargo.toml`) —
keep it on the SDK revision of the client you install into.

```bash
rustup target add wasm32-wasip2
make build   # -> dist/durability-check/{mod.wasm,mod.json}; copy the folder into the client's Data/Mods/
make test    # OPL parsing / bands / order / crossing tests (src/logic.rs, host target)
```

## How it works

| need | surface |
|---|---|
| worn items | `cuo:ent/equipment` on the player (serial per layer) |
| durability | `cuo:ent/properties` — cliloc 1060639 / 1061645 `cur\tmax` |
| icons | `cuo:ui/custom` art (kind 3), item hue |
| hover tooltip | `cuo:ui/tooltip-serial` |
| move to backpack | `actions::move_item` |
| alerts | `cuo:chat/message`, kind 1 (system log) |
| settings | `cuo:options/schema` / `values` |
| window | `cuo:ui/*` nodes in the host's UiTheme palette; `cuo:ui/movable` |

The equipment query is not `Changed`-filtered: the host doesn't tick a live equip
(0x2E) for mods' `Changed<EquipmentSlotsDto>`, only login and unequip.
