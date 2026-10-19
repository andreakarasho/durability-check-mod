//! Durability check — a small movable panel listing the player's worn items that
//! carry hit points in their OPL, each with its art, name and a coloured bar
//! (amber at the warn threshold, red at the danger one). Port of the web client's
//! Equipment Durability tracker onto the generic mod surface:
//!
//!   * worn items: `cuo:ent/equipment` on the player (serial per layer);
//!   * durability: `cuo:ent/properties` (the OPL; the host re-requests it on every
//!     0xDC revision bump, so a hit / repair arrives as a change);
//!   * icons: `cuo:ui/custom` art, tinted with the item hue;
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
use cuo_mod_sdk::host::{self, Scope};
use cuo_mod_sdk::prelude::*;
use cuo_mod_sdk::types::Item as IsItem;
use logic::{Band, Reading, Row};
use serde::{Deserialize, Serialize};

const GAME_SCREEN: u8 = 6;
const LAYER_BACKPACK: usize = 0x15;
const ART: u8 = 3; // UOCustomKind.Art
const DCLICK_MS: f32 = 400.0;
const HUE_WARN: u16 = 53;
const HUE_DANGER: u16 = 0x21;
const Z: i32 = 900;
const BAR_H: f32 = 10.0;

const OPT_ENABLED: &str = "enabled";
const OPT_WARN: &str = "warn";
const OPT_DANGER: &str = "danger";
const OPT_HIDE_ABOVE: &str = "hide_above";
const OPT_NOTIFY: &str = "notify";
const OPT_READOUT: &str = "readout";
const OPT_NAMES: &str = "names";
const OPT_SCALE: &str = "scale";

// The host's flat panel theme (ClassicUO.Ecs UiTheme.cs). A guest can't read the
// host palette; keep in step with UiTheme by hand. AMBER is the one colour the theme
// lacks — the warn band.
mod theme {
    pub type Rgb = (u8, u8, u8);
    pub const BG: Rgb = (9, 12, 17);
    pub const CARD: Rgb = (12, 16, 22);
    pub const FRAME: Rgb = (39, 45, 54);
    pub const ROW_HOVER: Rgb = (26, 33, 43);
    pub const CONTROL: Rgb = (28, 36, 47);
    pub const ACCENT: Rgb = (47, 191, 174);
    pub const WARN: Rgb = (240, 123, 123);
    pub const AMBER: Rgb = (240, 190, 90);
    pub const TEXT: Rgb = (232, 237, 242);
    pub const TEXT_MUTED: Rgb = (152, 162, 173);
    // FontCache face | UoFontRuntime.TtfFlag (0x100); sizes take UiTheme.SizeBoost.
    pub const REGULAR: u16 = 1 | 0x100;
    pub const BOLD: u16 = 2 | 0x100;
    pub const SIZE_BOOST: f32 = 2.0;
}

fn rgb(c: theme::Rgb, a: u8) -> Color {
    Color::rgba(c.0, c.1, c.2, a)
}

fn band_color(b: Band) -> theme::Rgb {
    match b {
        Band::Ok => theme::ACCENT,
        Band::Warn => theme::AMBER,
        Band::Danger => theme::WARN,
    }
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
    root: Entity,
    overall: Entity,
    min_btn: Entity,
    lock_btn: Entity,
    list: Option<Entity>,
    // Row entity -> item serial, for the double-click.
    rows: Vec<(Entity, u32)>,
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
    // The width the rows' names were fitted to; a finished resize drag refits them.
    rows_w: f32,
    pending_rows: bool,
    last_click: Option<(Entity, f32)>,}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn setup(app: &mut App) {
    app.add_systems(Schedule::Startup, declare_options);
    app.add_systems(Schedule::Update, (snap.run_on_change(), feed.after(snap).run_on_change()));
    app.add_observer(on_click);
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

        if let Some(n) = s.ui.as_ref().and_then(|u| moved.get(u.root)) {
            let (x, y) = (n.left.value, n.top.value);
            if (x, y) != (s.saved.x, s.saved.y) {
                (s.saved.x, s.saved.y) = (x, y);
                s.save_pending = true;
            }
        }
        // A resize drag writes width/height instead of left/top — the host only does
        // one or the other, so the two stores can't share a query.
        if let Some(n) = s.ui.as_ref().and_then(|u| resized.get(u.root)) {
            let (w, h) = (n.width.value, n.height.value);
            if (w, h) != (s.saved.w, s.saved.h) {
                (s.saved.w, s.saved.h) = (w, h);
                s.save_pending = true;
            }
            // The name column is a fixed fraction of the width: a width change changes
            // how many characters fit, so the rows must be refitted when the drag ends.
            if w != s.rows_w {
                s.pending_rows = true;
            }
        }
        if s.pending_rows && !mouse_down {
            s.pending_rows = false;
            s.built_rows = None;
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

#[system]
fn on_click(click: On<Event, UiClick>, time: Option<Res<Time>>, mut cmds: Commands) {
    let now = time.map_or(0.0, |t| t.total);
    with(|s| s.click(click.entity(), now, &mut cmds));
}

impl State {
    fn update(&mut self, cmds: &mut Commands) {
        if !self.loaded {
            self.loaded = true;
            self.saved = storage::load(Scope::Global).unwrap_or_default();
        }
        if !(self.in_game && self.opts.enabled) {
            if let Some(ui) = self.ui.take() {
                cmds.entity(ui.root).despawn();
            }
            self.built_opts = None;
            self.built_rows = None;
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

        if self.built_opts != Some(self.opts) {
            if let Some(ui) = self.ui.take() {
                cmds.entity(ui.root).despawn();
            }
            self.spawn(cmds);
            self.built_opts = Some(self.opts);
            self.built_rows = None;
        }
        let key = (rows, self.saved.minimized);
        if self.built_rows.as_ref() != Some(&key) {
            self.build_list(cmds, &key.0);
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

    fn px(&self, v: f32) -> Val {
        Val::px(v * self.opts.scale)
    }

    fn font(&self, face: u16, size: f32) -> TextFont {
        TextFont { font_id: face, size: ((size + theme::SIZE_BOOST) * self.opts.scale).round() as u16 }
    }

    #[allow(clippy::too_many_arguments)]
    fn label(&self, parent: Entity, text: &str, face: u16, size: f32, color: theme::Rgb, node: Node, cmds: &mut Commands) -> Entity {
        cmds.spawn((
            node,
            Text { value: text.into() },
            self.font(face, size),
            TextColor { value: rgb(color, 255) },
            TextWrap { kind: text_wrap_kind::NONE },
            ChildOf::new(parent),
        ))
        .id()
    }

    fn button(&self, parent: Entity, text: &str, cmds: &mut Commands) -> Entity {
        let n = Node {
            height: self.px(18.0),
            padding: UiRect { left: self.px(4.0), right: self.px(4.0), top: Val::px(0.0), bottom: Val::px(0.0) },
            justify_content: justify_content::CENTER,
            align_items: align_items::CENTER,
            ..Node::base()
        };
        cmds.spawn((
            n,
            BackgroundColor { value: rgb(theme::CONTROL, 255) },
            BorderRadius::all(3.0),
            Text { value: text.into() },
            self.font(theme::BOLD, 14.0),
            TextColor { value: rgb(theme::TEXT_MUTED, 255) },
            TextWrap { kind: text_wrap_kind::NONE },
            Interaction::NONE,
            UiNoWindowDrag {},
            ChildOf::new(parent),
        ))
        .id()
    }

    fn width(&self) -> f32 {
        150.0 + if self.opts.names { 100.0 } else { 0.0 }
    }

    /// The root's current declared width, in px (the persisted one, or the
    /// default for the options).
    fn root_w(&self) -> f32 {
        if self.saved.w > 0.0 {
            self.saved.w
        } else {
            self.px(self.width()).value
        }
    }

    /// The longest prefix of `name` that fits `avail` px — the full name when it
    /// does. `measure-text` takes the rendered TTF size in the font id's high bits
    /// (`font & 0x1FF`, size << 9), so no px-scale guessing — the host measures the
    /// very glyphs the row draws.
    fn fit(&self, name: &str, face: u16, avail: f32) -> String {
        let font = (face & 0x1FF) | ((16.0 * self.opts.scale).round() as u16) << 9;
        let w = |t: &str| host::measure_text(font, t) as f32;
        if w(name) <= avail {
            return name.to_string();
        }
        let chars: Vec<char> = name.chars().collect();
        // Binary-search the largest prefix whose `prefix…` fits.
        let mut lo = 0usize;
        let mut hi = chars.len();
        while lo + 2 < hi {
            let mid = (lo + hi) / 2;
            let mut t: String = chars[..mid].iter().collect();
            t.push('…');
            if w(&t) <= avail {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let mut t: String = chars[..lo].iter().collect();
        t.push('…');
        t
    }

    fn spawn(&mut self, cmds: &mut Commands) {
        let mut n = Node::abs(self.saved.x, self.saved.y, 0.0, 0.0);
        n.width = if self.saved.w > 0.0 { Val::px(self.saved.w) } else { self.px(self.width()) };
        // Concrete height, not auto: the host's resize grip anchors on the node's
        // declared bottom-right, and an auto-height node declares no bottom. The
        // default fits the full equipment list; shrink and the list clips, grow and
        // it pads.
        n.height = if self.saved.h > 0.0 { Val::px(self.saved.h) } else { self.px(250.0) };
        n.overflow = overflow::CLIP;
        n.flex_direction = flex_direction::COLUMN;
        n.padding = UiRect { left: self.px(8.0), right: self.px(8.0), top: self.px(4.0), bottom: self.px(6.0) };
        n.border = UiRect::splat(Val::px(1.0));
        n.gap = self.px(2.0);
        let root = cmds
            .spawn((
                n,
                BackgroundColor { value: rgb(theme::BG, 250) },
                BorderColor { r: theme::FRAME.0 as f32, g: theme::FRAME.1 as f32, b: theme::FRAME.2 as f32, a: 255.0 },
                BorderRadius::all(4.0),
                GlobalZIndex { value: Z },
                UiNoRightClickClose {},
                UiContainsByBounds {},
                // Bottom-right grip: drag the window's size. Minus the row / readout
                // columns the width can't shrink much before the list just clips.
                UiResizable { min_w: self.opts.scale * 140.0, min_h: self.opts.scale * 90.0, whole_pixels: true, ..Default::default() },
                UiName { value: "durability.root".into() },
            ))
            .id();
        if !self.saved.locked {
            cmds.entity(root).insert(UiMovable {});
        }

        let header = Node {
            width: Val::percent(100.0),
            align_items: align_items::CENTER,
            gap: self.px(6.0),
            padding: UiRect { left: Val::px(0.0), right: Val::px(0.0), top: self.px(2.0), bottom: self.px(4.0) },
            ..Node::base()
        };
        let header = cmds.spawn((header, ChildOf::new(root))).id();
        let title = if self.opts.names { "Equipment Durability" } else { "Equipment" };
        let grow = Node { width: Val { type_: val_type::GROW, value: 0.0, is_auto: false }, ..Node::base() };
        self.label(header, title, theme::REGULAR, 14.0, theme::TEXT_MUTED, grow, cmds);
        let overall = self.label(header, "—", theme::BOLD, 14.0, theme::TEXT, Node::base(), cmds);
        let min_btn = self.button(header, if self.saved.minimized { "+" } else { "-" }, cmds);
        let lock_btn = self.button(header, if self.saved.locked { "LOCK" } else { "MOVE" }, cmds);
        // Separator under the header.
        let sep = Node { width: Val::percent(100.0), height: Val::px(1.0), ..Node::base() };
        cmds.spawn((sep, BackgroundColor { value: rgb(theme::FRAME, 255) }, ChildOf::new(root)));

        self.ui = Some(Ui { root, overall, min_btn, lock_btn, list: None, rows: vec![] });
    }

    fn build_list(&mut self, cmds: &mut Commands, rows: &[Row]) {
        self.rows_w = self.root_w();
        let Some(ui) = self.ui.as_mut() else { return };
        if let Some(list) = ui.list.take() {
            cmds.entity(list).despawn();
        }
        ui.rows.clear();
        let (root, overall) = (ui.root, ui.overall);

        let (text, color) = match logic::overall(rows) {
            Some((avg, band)) => (format!("{avg}%"), band_color(band)),
            None => ("—".into(), theme::TEXT),
        };
        cmds.entity(overall).insert((Text { value: text }, TextColor { value: rgb(color, 255) }));

        let n = Node { width: Val::percent(100.0), flex_direction: flex_direction::COLUMN, gap: self.px(2.0), ..Node::base() };
        let list = cmds.spawn((n, ChildOf::new(root))).id();

        let shown: Vec<&Row> = if self.saved.minimized {
            rows.iter().take(1).collect()
        } else {
            rows.iter().filter(|r| r.reading.percent <= self.opts.hide_above as f32).collect()
        };
        if shown.is_empty() && !self.saved.minimized {
            let msg = if rows.is_empty() { "Equip an item with durability to start tracking." } else { "All items above the hide threshold." };
            let n = Node { width: Val::percent(100.0), padding: UiRect { left: Val::px(0.0), right: Val::px(0.0), top: self.px(4.0), bottom: self.px(4.0) }, ..Node::base() };
            let e = self.label(list, msg, theme::REGULAR, 14.0, theme::TEXT_MUTED, n, cmds);
            cmds.entity(e).insert(TextWrap { kind: text_wrap_kind::WORDS });
        }
        let mut entities = Vec::with_capacity(shown.len());
        for r in shown {
            entities.push((self.row(list, r, cmds), r.serial));
        }
        let ui = self.ui.as_mut().expect("ui");
        ui.list = Some(list);
        ui.rows = entities;
    }

    fn row(&self, list: Entity, r: &Row, cmds: &mut Commands) -> Entity {
        let n = Node {
            width: Val::percent(100.0),
            height: self.px(24.0),
            align_items: align_items::CENTER,
            gap: self.px(8.0),
            padding: UiRect { left: self.px(4.0), right: self.px(4.0), top: Val::px(0.0), bottom: Val::px(0.0) },
            ..Node::base()
        };
        let base = if r.band == Band::Danger { rgb(theme::WARN, 26) } else { rgb(theme::BG, 0) };
        let row = cmds
            .spawn((
                n,
                BackgroundColor { value: base.clone() },
                UiHoverTint { normal: base, hover: rgb(theme::ROW_HOVER, 255) },
                BorderRadius::all(3.0),
                UiTooltipSerial { serial: r.serial },
                Interaction::NONE,
                UiNoWindowDrag {},
                ChildOf::new(list),
            ))
            .id();

        let icon = Node {
            width: self.px(22.0),
            height: self.px(22.0),
            border: UiRect::splat(Val::px(1.0)),
            justify_content: justify_content::CENTER,
            align_items: align_items::CENTER,
            overflow: overflow::CLIP,
            ..Node::base()
        };
        let icon = cmds
            .spawn((
                icon,
                BackgroundColor { value: rgb(theme::CARD, 255) },
                BorderColor { r: theme::FRAME.0 as f32, g: theme::FRAME.1 as f32, b: theme::FRAME.2 as f32, a: 255.0 },
                BorderRadius::all(2.0),
                ChildOf::new(row),
            ))
            .id();
        let (hx, hy, hz) = logic::hue_vector(r.hue);
        let art = UiCustom { kind: ART, asset_id: r.graphic as u32, hue_x: hx, hue_y: hy, hue_z: hz, stacked: false };
        cmds.spawn((Node { width: self.px(20.0), height: self.px(20.0), ..Node::base() }, art, ChildOf::new(icon)));

        let grow = || Node { width: Val { type_: val_type::GROW, value: 0.0, is_auto: false }, overflow: overflow::CLIP, ..Node::base() };
        if self.opts.names {
            let (face, color) = if r.band == Band::Danger { (theme::BOLD, theme::WARN) } else { (theme::REGULAR, theme::TEXT) };
            let s = self.opts.scale;
            // The row's non-name columns: padding + border, icon, the gaps, bar, readout.
            let fixed = 2.0 + (16.0 + 8.0 + 22.0 + 24.0 + 56.0 + if self.opts.values { 44.0 } else { 28.0 }) * s;
            let name = self.fit(&r.name, face, self.root_w() - fixed);
            self.label(row, &name, face, 14.0, color, grow(), cmds);
        }

        // The bar: fill + empty track. The readout sits beside it — mod nodes below
        // the root can't be absolute, so there's no overlaying it on the bar.
        let bar_w = if self.opts.names { Node { width: self.px(56.0), ..Node::base() } } else { grow() };
        // No border: the host draws a rounded outline AFTER the children, so it rings the fill.
        let bar = Node { height: self.px(BAR_H), ..bar_w };
        let bar = cmds
            .spawn((
                bar,
                BackgroundColor { value: rgb(theme::CONTROL, 255) },
                BorderRadius::all(BAR_H * self.opts.scale),
                ChildOf::new(row),
            ))
            .id();
        // Pill ends (the host clamps the radius to half the box); min width = height so a
        // low reading is a short capsule, not a dot. Own radius: a parent clip is a
        // rectangular scissor, it doesn't round its children.
        let fill = Node {
            width: Val::percent(r.reading.percent.clamp(0.0, 100.0)),
            min_width: self.px(BAR_H),
            height: Val::percent(100.0),
            ..Node::base()
        };
        cmds.spawn((fill, BackgroundColor { value: rgb(band_color(r.band), 255) }, BorderRadius::all(BAR_H * self.opts.scale), ChildOf::new(bar)));

        let readout = if self.opts.values {
            format!("{}/{}", r.reading.current, r.reading.max)
        } else {
            format!("{}%", r.reading.percent.round())
        };
        let n = Node { width: self.px(if self.opts.values { 44.0 } else { 28.0 }), justify_content: justify_content::END, ..Node::base() };
        self.label(row, &readout, theme::BOLD, 14.0, band_color(r.band), n, cmds);
        row
    }

    fn click(&mut self, e: Entity, now: f32, cmds: &mut Commands) {
        let Some((root, min_btn, lock_btn)) = self.ui.as_ref().map(|u| (u.root, u.min_btn, u.lock_btn)) else { return };
        if e == min_btn {
            self.saved.minimized = !self.saved.minimized;
            cmds.entity(e).insert(Text { value: if self.saved.minimized { "+" } else { "-" }.into() });
            storage::save(Scope::Global, &self.saved);
            self.update(cmds);
            return;
        }
        if e == lock_btn {
            self.saved.locked = !self.saved.locked;
            if self.saved.locked {
                cmds.entity(root).remove::<UiMovable>();
            } else {
                cmds.entity(root).insert(UiMovable {});
            }
            cmds.entity(e).insert(Text { value: if self.saved.locked { "LOCK" } else { "MOVE" }.into() });
            storage::save(Scope::Global, &self.saved);
            return;
        }
        let Some(&(_, serial)) = self.ui.as_ref().and_then(|u| u.rows.iter().find(|(r, _)| *r == e)) else { return };
        // Double-click a row: the item goes to the backpack.
        match self.last_click {
            Some((prev, at)) if prev == e && now - at <= DCLICK_MS => {
                self.last_click = None;
                if self.backpack != 0 {
                    actions::move_item(serial, 0, Destination::Container(self.backpack));
                }
            }
            _ => self.last_click = Some((e, now)),
        }
    }
}
