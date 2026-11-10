//! Durability check — a small movable panel listing the player's worn items that
//! carry hit points in their OPL, each with its art, name and a coloured bar
//! (amber at the warn threshold, red at the danger one). Port of the web client's
//! Equipment Durability tracker onto the generic mod surface:
//!
//!   * worn items: `cuo:ent/equipment` on the player (serial per layer);
//!   * durability: `cuo:ent/properties` (the OPL; the host re-requests it on every
//!     0xDC revision bump, so a hit / repair arrives as a change);
//!   * the window: an HTML/CSS page (`durability.html` / `.css`) the mod fills, so the
//!     client's UI theme and the player's own copy style it;
//!   * icons: `art:` images on the page, tinted with the item hue;
//!   * hover: `cuo:ui/tooltip-serial` — the host's own OPL tooltip;
//!   * double-click a row: `actions::move_item` into the backpack;
//!   * size: `cuo:ui/resizable` — the host's bottom-right corner grip; the
//!     window's w/h are stored with its position;
//!   * threshold-cross alerts: a `cuo:chat/message` system line;
//!   * settings: the Options window's "Mods" page; position / lock / minimized in
//!     the mod's storage.

mod logic;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use cuo_mod_sdk::actions::{self, Destination};
use cuo_mod_sdk::host::{self, PageChange, Scope};
use cuo_mod_sdk::prelude::*;
use cuo_mod_sdk::types::Item as IsItem;
use logic::{Band, Reading, Row};
use serde::{Deserialize, Serialize};

const GAME_SCREEN: u8 = 6;
const LAYER_BACKPACK: usize = 0x15;
const DCLICK_MS: f32 = 400.0;
const HUE_WARN: u16 = 53;
const HUE_DANGER: u16 = 0x21;
const PAGE: &str = "durability";
const PAGE_HTML: &str = include_str!("durability.html");
const PAGE_CSS: &str = include_str!("durability.css");
// The body's font-size at 100% panel size: the sheet's sizes are em of it.
const FONT_PX: f32 = 16.0;

const OPT_ENABLED: &str = "enabled";
const OPT_WARN: &str = "warn";
const OPT_DANGER: &str = "danger";
const OPT_HIDE_ABOVE: &str = "hide_above";
const OPT_NOTIFY: &str = "notify";
const OPT_READOUT: &str = "readout";
const OPT_NAMES: &str = "names";
const OPT_SCALE: &str = "scale";

fn band_class(b: Band) -> &'static str {
    match b {
        Band::Ok => "ok",
        Band::Warn => "warn",
        Band::Danger => "danger",
    }
}

fn class(on: bool, name: &str) -> PageChange {
    if on { PageChange::AddClass(name.into()) } else { PageChange::RemoveClass(name.into()) }
}

#[derive(Clone, Copy, PartialEq)]
struct Opts {
    enabled: bool,
    warn: i32,
    danger: i32,
    hide_above: i32,
    notify: bool,
    values: bool,
    names: bool,
    scale: f32,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts { enabled: true, warn: 60, danger: 30, hide_above: 100, notify: true, values: false, names: true, scale: 1.0 }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
struct Saved {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    locked: bool,
    minimized: bool,
}

impl Default for Saved {
    fn default() -> Saved {
        Saved { x: 20.0, y: 120.0, w: 0.0, h: 0.0, locked: false, minimized: false }
    }
}

struct Ui {
    page: u64,
    // The page's #list element: a new one means the page (re)loaded and the rows are gone.
    list: u64,
}

impl Ui {
    fn root(&self) -> Entity {
        Entity::from_bits(self.page)
    }
}

#[derive(Default)]
struct State {
    in_game: bool,
    opts: Opts,
    saved: Saved,
    loaded: bool,
    save_pending: bool,
    equipped: Vec<(usize, u32)>, // (layer, serial)
    backpack: u32,
    art: HashMap<u32, (u16, u16)>, // serial -> (graphic, hue)
    props: HashMap<u32, (String, Option<Reading>)>,
    asked: HashSet<u32>,
    bands: HashMap<u32, Band>,
    ui: Option<Ui>,
    // What the window / list were last built from; rebuild only when it moves.
    built_opts: Option<Opts>,
    built_rows: Option<(Vec<Row>, bool)>,
    // The row last clicked (item serial) and when, for the double-click.
    last_click: Option<(u32, f32)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn setup(app: &mut App) {
    app.add_systems(Schedule::Startup, declare_options);
    app.add_systems(Schedule::Update, (snap.run_on_change(), feed.after(snap).run_on_change(), page_tick));
}

export_mod!(setup);

#[system]
fn declare_options(mut schema: ResMut<ModOptionsSchema>) {
    let opt = |key: &str, label: &str, kind: u8, default: i32, keywords: &str| ModOption {
        key: key.into(),
        label: label.into(),
        group: "Durability".into(),
        keywords: keywords.into(),
        kind,
        default,
        ..Default::default()
    };
    let slider = |key: &str, label: &str, default: i32, (min, max, step): (i32, i32, i32)| {
        let mut o = opt(key, label, 1, default, "durability threshold hit points");
        (o.min, o.max, o.step) = (min, max, step);
        o
    };
    let mut readout = opt(OPT_READOUT, "Durability readout", 3, 0, "durability percent values");
    readout.choices = vec!["Percent (60%)".into(), "Values (45 / 255)".into()];
    schema.options = vec![
        opt(OPT_ENABLED, "Enable durability panel", 0, 1, "durability equipment armor repair"),
        slider(OPT_WARN, "Warning threshold (%)", 60, (1, 99, 1)),
        slider(OPT_DANGER, "Danger threshold (%)", 30, (1, 98, 1)),
        opt(OPT_NOTIFY, "Notify on threshold crossing", 0, 1, "durability alert journal message"),
        slider(OPT_HIDE_ABOVE, "Hide items above (%, 100 = off)", 100, (2, 100, 1)),
        slider(OPT_SCALE, "Panel size (%)", 100, (100, 250, 10)),
        readout,
        opt(OPT_NAMES, "Show item names", 0, 1, "durability names"),
    ];
}

/// Game state, options, and the window's own position / size (stored once a drag ends).
#[allow(clippy::type_complexity)]
#[system]
fn snap(
    game: Option<Res<GameState>>,
    values: Option<Res<ModOptionsValues>>,
    mouse: Option<Res<MouseInput>>,
    moved: Query<&Node, (With<UiMovable>, Changed<Node>)>,
    resized: Query<&Node, (With<UiResizable>, Changed<Node>)>,
    mut cmds: Commands,
) {
    with(|s| {
        let mouse_down = mouse.as_ref().is_some_and(|m| m.left);
        s.in_game = game.is_some_and(|g| g.current == GAME_SCREEN);
        let get = |k: &str, d: i32| values.as_ref().and_then(|v| v.values.iter().find(|o| o.key == k).map(|o| o.value)).unwrap_or(d);
        let warn = get(OPT_WARN, 60).clamp(2, 99);
        s.opts = Opts {
            enabled: get(OPT_ENABLED, 1) != 0,
            warn,
            danger: get(OPT_DANGER, 30).clamp(1, warn - 1),
            hide_above: get(OPT_HIDE_ABOVE, 100),
            notify: get(OPT_NOTIFY, 1) != 0,
            values: get(OPT_READOUT, 0) == 1,
            names: get(OPT_NAMES, 1) != 0,
            scale: get(OPT_SCALE, 100).clamp(100, 250) as f32 / 100.0,
        };

        if let Some(n) = s.ui.as_ref().and_then(|u| moved.get(u.root())) {
            let (x, y) = (n.left.value, n.top.value);
            if (x, y) != (s.saved.x, s.saved.y) {
                (s.saved.x, s.saved.y) = (x, y);
                s.save_pending = true;
            }
        }
        // A resize drag writes width/height instead of left/top — the host only does
        // one or the other, so the two stores can't share a query.
        if let Some(n) = s.ui.as_ref().and_then(|u| resized.get(u.root())) {
            let (w, h) = (n.width.value, n.height.value);
            if (w, h) != (s.saved.w, s.saved.h) {
                (s.saved.w, s.saved.h) = (w, h);
                s.save_pending = true;
            }
        }
        // Drags write the node every frame: store once the button is up.
        if s.save_pending && !mouse_down {
            s.save_pending = false;
            storage::save(Scope::Global, &s.saved);
        }
        s.update(&mut cmds);
    });
}

/// The player's equipment and every item's art / OPL, as they change, cached by
/// serial: an item equipped from the backpack changes neither, so only an earlier
/// sighting knows them.
// ponytail: the art / OPL caches keep every item ever seen (a few bytes each); prune
// on item removal if a long session ever makes that matter.
#[allow(clippy::type_complexity)]
#[system]
fn feed(
    // Not Changed-filtered: a live equip (0x2E) doesn't reach a mod's
    // Changed<EquipmentSlots>, only login / unequip do. One row, so the per-frame
    // run is cheap.
    // ponytail: back to Changed<> once the host ticks equips for mods.
    equipment: Query<&EquipmentSlots, With<Player>>,
    graphics: Query<(&Serial, &Graphic, &Hue), (With<IsItem>, Changed<Graphic>)>,
    hues: Query<(&Serial, &Graphic, &Hue), (With<IsItem>, Changed<Hue>)>,
    props: Query<&EntityProperties, Changed<EntityProperties>>,
    mut cmds: Commands,
) {
    with(|s| {
        let mut dirty = !props.is_empty() || !graphics.is_empty() || !hues.is_empty();
        if let Some((_, e)) = equipment.single() {
            let at = |l: usize| e.serials.get(l).copied().unwrap_or(0);
            s.backpack = at(LAYER_BACKPACK);
            let equipped: Vec<(usize, u32)> = logic::LAYERS.iter().map(|&l| (l, at(l))).filter(|&(_, serial)| serial != 0).collect();
            if equipped != s.equipped {
                s.equipped = equipped;
                dirty = true;
            }
        }
        if !dirty {
            return;
        }
        for (_, (serial, g, h)) in graphics.iter().chain(hues.iter()) {
            s.art.insert(serial.value, (g.value, h.value));
        }
        for (_, p) in &props {
            let reading = logic::parse(p.lines.iter().map(|l| (l.cliloc, l.args.as_str())));
            s.props.insert(p.serial, (p.name.clone(), reading));
        }
        // Worn items the client never had an OPL for: ask once.
        for &(_, serial) in &s.equipped {
            if !s.props.contains_key(&serial) && s.asked.insert(serial) {
                actions::request_properties(serial);
            }
        }
        s.update(&mut cmds);
    });
}

/// The page's clicks, and its reloads: a saved page file makes new elements, without the rows.
#[system]
fn page_tick(time: Option<Res<Time>>, mut cmds: Commands) {
    let now = time.map_or(0.0, |t| t.total);
    with(|s| {
        let Some(ui) = s.ui.as_mut() else { return };
        let list = host::page_element(ui.page, "list").unwrap_or(0);
        let reloaded = list != ui.list;
        if reloaded {
            ui.list = list;
            s.built_rows = None;
        }
        let clicks = host::page_clicks(ui.page);
        for click in &clicks {
            s.click(click, now, &mut cmds);
        }
        if reloaded || !clicks.is_empty() {
            s.update(&mut cmds);
        }
    });
}

impl State {
    fn close(&mut self) {
        if let Some(ui) = self.ui.take() {
            host::page_close(ui.page);
        }
        self.built_opts = None;
        self.built_rows = None;
    }

    fn update(&mut self, cmds: &mut Commands) {
        if !self.loaded {
            self.loaded = true;
            self.saved = storage::load(Scope::Global).unwrap_or_default();
        }
        if !(self.in_game && self.opts.enabled) {
            self.close();
            return;
        }

        let rows = self.rows();
        if self.opts.notify {
            for r in logic::crossings(&rows, &mut self.bands) {
                let mut m = ChatMessage::system(format!("{} at {}%", r.name, r.reading.percent.round()));
                m.hue = if r.band == Band::Danger { HUE_DANGER } else { HUE_WARN };
                m.kind = 1;
                cmds.send(m);
            }
        } else {
            logic::crossings(&rows, &mut self.bands);
        }

        // The options size and fill the whole window: a change opens it anew. So does a window the client closed.
        if self.built_opts != Some(self.opts) || self.ui.as_ref().is_none_or(|u| !host::page_is_open(u.page)) {
            self.close();
            self.spawn(cmds);
            self.built_opts = Some(self.opts);
        }
        let key = (rows, self.saved.minimized);
        if self.built_rows.as_ref() != Some(&key) && self.build_list(cmds, &key.0) {
            self.built_rows = Some(key);
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .equipped
            .iter()
            .filter_map(|&(layer, serial)| {
                let (name, reading) = self.props.get(&serial)?;
                let reading = (*reading)?;
                let (graphic, hue) = self.art.get(&serial).copied().unwrap_or_default();
                let name = if name.trim().is_empty() { logic::slot_label(layer).to_string() } else { name.trim().to_string() };
                let band = logic::band(reading.percent, self.opts.warn, self.opts.danger);
                Some(Row { serial, graphic, hue, name, reading, band })
            })
            .collect();
        logic::sort_rows(&mut rows);
        rows
    }

    fn spawn(&mut self, cmds: &mut Commands) {
        let (o, s) = (self.opts, &self.saved);
        let Some(page) = host::page_spawn(PAGE, PAGE_HTML, PAGE_CSS, s.x as i32, s.y as i32) else { return };
        // The stored size, else the default for the options: it fits the full equipment list.
        let size = |saved: f32, default: f32| if saved > 0.0 { saved } else { default * o.scale };
        let mut n = Node::abs(s.x, s.y, size(s.w, if o.names { 250.0 } else { 150.0 }), size(s.h, 250.0));
        n.display = display::FLEX;
        let root = Entity::from_bits(page);
        // Bottom-right grip: drag the window's size. Smaller than this and the list just clips.
        cmds.entity(root).insert((n, UiResizable { min_w: o.scale * 140.0, min_h: o.scale * 90.0, whole_pixels: true, ..Default::default() }));
        if s.locked {
            cmds.entity(root).remove::<UiMovable>();
        }
        host::page_edit(page, "panel", &PageChange::Style(("font-size".into(), format!("{}px", FONT_PX * o.scale))));
        host::page_edit(page, "title", &PageChange::Text(if o.names { "Equipment Durability" } else { "Equipment" }.into()));
        host::page_edit(page, "list", &class(!o.names, "no-names"));
        host::page_edit(page, "list", &class(o.values, "values"));
        self.ui = Some(Ui { page, list: 0 });
        self.label_buttons(page);
    }

    fn label_buttons(&self, page: u64) {
        host::page_edit(page, "min", &PageChange::Text(if self.saved.minimized { "+" } else { "-" }.into()));
        host::page_edit(page, "lock", &PageChange::Text(if self.saved.locked { "LOCK" } else { "MOVE" }.into()));
    }

    /// Fills the header's overall and the rows. False while the page hasn't loaded.
    fn build_list(&self, cmds: &mut Commands, rows: &[Row]) -> bool {
        let Some(page) = self.ui.as_ref().filter(|u| u.list != 0).map(|u| u.page) else { return false };
        let overall = logic::overall(rows);
        host::page_edit(page, "overall", &PageChange::Text(overall.map_or("—".into(), |(avg, _)| format!("{avg}%"))));
        for b in [Band::Ok, Band::Warn, Band::Danger] {
            host::page_edit(page, "overall", &class(overall.is_some_and(|(_, worst)| worst == b), band_class(b)));
        }

        let shown: Vec<&Row> = if self.saved.minimized {
            rows.iter().take(1).collect()
        } else {
            rows.iter().filter(|r| r.reading.percent <= self.opts.hide_above as f32).collect()
        };
        let empty = shown.is_empty() && !self.saved.minimized;
        host::page_edit(page, "no-items", &class(empty && rows.is_empty(), "on"));
        host::page_edit(page, "all-hidden", &class(empty && !rows.is_empty(), "on"));

        host::page_clear(page, "list");
        for r in shown {
            let Some(row) = host::page_clone(page, "list", "row") else { return false };
            let part = |name: &str, change: PageChange| {
                if let Some(e) = host::page_part(page, row, name) {
                    host::page_edit_element(page, e, &change);
                }
            };
            host::page_edit_element(page, row, &PageChange::Attr(("data-arg".into(), format!("row:{}", r.serial))));
            host::page_edit_element(page, row, &PageChange::AddClass(band_class(r.band).into()));
            part("icon", PageChange::Attr(("src".into(), format!("art:{}?hue={}", r.graphic, r.hue & 0x7FFF))));
            part("name", PageChange::Text(r.name.clone()));
            part("fill", PageChange::Style(("width".into(), format!("{}%", r.reading.percent.clamp(0.0, 100.0)))));
            let readout = if self.opts.values {
                format!("{}/{}", r.reading.current, r.reading.max)
            } else {
                format!("{}%", r.reading.percent.round())
            };
            part("readout", PageChange::Text(readout));
            // The host's own item tooltip; a drag on a row doesn't move the window.
            cmds.entity(Entity::from_bits(row)).insert((UiTooltipSerial { serial: r.serial }, Interaction::NONE, UiNoWindowDrag {}));
        }
        true
    }

    /// The `data-arg` of a clicked element of the page.
    fn click(&mut self, arg: &str, now: f32, cmds: &mut Commands) {
        let Some((page, root)) = self.ui.as_ref().map(|u| (u.page, u.root())) else { return };
        match arg {
            "min" => self.saved.minimized = !self.saved.minimized,
            "lock" => {
                self.saved.locked = !self.saved.locked;
                if self.saved.locked {
                    cmds.entity(root).remove::<UiMovable>();
                } else {
                    cmds.entity(root).insert(UiMovable {});
                }
            }
            _ => {
                let Some(serial) = arg.strip_prefix("row:").and_then(|s| s.parse::<u32>().ok()) else { return };
                // Double-click a row: the item goes to the backpack.
                match self.last_click {
                    Some((prev, at)) if prev == serial && now - at <= DCLICK_MS => {
                        self.last_click = None;
                        if self.backpack != 0 {
                            actions::move_item(serial, 0, Destination::Container(self.backpack));
                        }
                    }
                    _ => self.last_click = Some((serial, now)),
                }
                return;
            }
        }
        self.label_buttons(page);
        storage::save(Scope::Global, &self.saved);
    }
}
