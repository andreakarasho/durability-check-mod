//! SDK-free durability logic: OPL parsing, bands, row order, threshold crossings.
//! Kept apart from the UI so `cargo test` checks it natively.

use std::collections::HashMap;

/// 1060639 "Hit Points ~1_val~ / ~2_val~"; 1061645 "Hits ~1_val~ / ~2_val~" (some shards).
pub const HIT_POINTS_CLILOCS: [i32; 2] = [1060639, 1061645];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub current: u32,
    pub max: u32,
    pub percent: f32,
}

/// The hit-points line of an OPL, from its `(cliloc, args)` pairs. Args are the raw
/// cliloc arguments, tab-separated.
pub fn parse<'a>(lines: impl IntoIterator<Item = (i32, &'a str)>) -> Option<Reading> {
    lines.into_iter().filter(|(c, _)| HIT_POINTS_CLILOCS.contains(c)).find_map(|(_, args)| {
        let mut it = args.split('\t').map(|a| a.trim().parse::<u32>());
        let (Some(Ok(current)), Some(Ok(max))) = (it.next(), it.next()) else { return None };
        (max > 0).then(|| Reading { current, max, percent: current as f32 * 100.0 / max as f32 })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Band {
    Ok,
    Warn,
    Danger,
}

pub fn band(percent: f32, warn: i32, danger: i32) -> Band {
    if percent <= danger as f32 {
        Band::Danger
    } else if percent <= warn as f32 {
        Band::Warn
    } else {
        Band::Ok
    }
}

/// Worn layers that can carry an item with hit points (no hair / face / beard /
/// backpack / mount).
pub const LAYERS: &[usize] = &[
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0C, 0x0D, 0x0E, 0x11, 0x12, 0x13, 0x14, 0x16,
    0x17, 0x18,
];

/// Label for an item whose OPL has no name yet.
pub fn slot_label(layer: usize) -> &'static str {
    match layer {
        0x01 | 0x02 => "Weapon",
        0x03 => "Shoes",
        0x04 => "Pants",
        0x05 => "Shirt",
        0x06 => "Helm",
        0x07 => "Gloves",
        0x08 => "Ring",
        0x09 => "Talisman",
        0x0A => "Neck",
        0x0C => "Waist",
        0x0D | 0x11 => "Chest",
        0x0E => "Bracelet",
        0x12 => "Ears",
        0x13 => "Arms",
        0x14 => "Cloak",
        0x16 => "Robe",
        0x17 => "Skirt",
        0x18 => "Legs",
        _ => "Item",
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub serial: u32,
    pub graphic: u16,
    pub hue: u16,
    pub name: String,
    pub reading: Reading,
    pub band: Band,
}

/// Every worn item with durability, lowest percent first (the most urgent on top).
pub fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|a, b| a.reading.percent.total_cmp(&b.reading.percent).then(a.serial.cmp(&b.serial)));
}

/// Mean percent over every worn item (rounded); the headline's band is the worst item's.
pub fn overall(rows: &[Row]) -> Option<(i32, Band)> {
    let worst = rows.first()?;
    let avg = rows.iter().map(|r| r.reading.percent).sum::<f32>() / rows.len() as f32;
    Some((avg.round() as i32, worst.band))
}

/// Rows that got heavier since `prev` (ok→warn, ok→danger, warn→danger). Recoveries
/// and first sightings stay quiet. `prev` becomes the current bands.
pub fn crossings<'a>(rows: &'a [Row], prev: &mut HashMap<u32, Band>) -> Vec<&'a Row> {
    let out = rows.iter().filter(|r| prev.get(&r.serial).is_some_and(|p| r.band > *p)).collect();
    *prev = rows.iter().map(|r| (r.serial, r.band)).collect();
    out
}

/// ShaderHueTranslator.GetHueVector(hue, partial: false, alpha) for item art.
pub fn hue_vector(hue: u16) -> (f32, f32, f32) {
    let partial = hue & 0x8000 != 0;
    let hue = (hue & 0x7FFF) as i32;
    if hue == 0 {
        return (0.0, 0.0, 1.0);
    }
    if hue & 0x4000 != 0 {
        return (hue as f32, 7.0, 1.0); // SHADER_SPECTRAL
    }
    (hue as f32 - 1.0, if partial { 2.0 } else { 1.0 }, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(serial: u32, cur: u32, max: u32) -> Row {
        let percent = cur as f32 * 100.0 / max as f32;
        Row {
            serial,
            graphic: 0,
            hue: 0,
            name: String::new(),
            reading: Reading { current: cur, max, percent },
            band: band(percent, 60, 30),
        }
    }

    #[test]
    fn parses_hit_points() {
        let r = parse([(1050045, "x"), (1060639, "24\t50")]).unwrap();
        assert_eq!((r.current, r.max, r.percent), (24, 50, 48.0));
        assert_eq!(parse([(1061645, " 3 \t 4 ")]).unwrap().current, 3);
        assert!(parse([(1060639, "5\t0")]).is_none());
        assert!(parse([(1060639, "garbage")]).is_none());
        assert!(parse([(1050045, "24\t50")]).is_none());
    }

    #[test]
    fn bands_are_inclusive() {
        assert_eq!(band(60.0, 60, 30), Band::Warn);
        assert_eq!(band(60.1, 60, 30), Band::Ok);
        assert_eq!(band(30.0, 60, 30), Band::Danger);
    }

    #[test]
    fn sorted_overall_and_crossings() {
        let mut rows = vec![row(1, 50, 50), row(2, 10, 50), row(3, 25, 50)];
        sort_rows(&mut rows);
        assert_eq!(rows.iter().map(|r| r.serial).collect::<Vec<_>>(), [2, 3, 1]);
        assert_eq!(overall(&rows), Some((57, Band::Danger)));

        let mut prev = HashMap::new();
        assert!(crossings(&rows, &mut prev).is_empty(), "first sight is quiet");
        let mut next = vec![row(1, 25, 50), row(2, 40, 50), row(3, 10, 50)];
        sort_rows(&mut next);
        let hit: Vec<u32> = crossings(&next, &mut prev).iter().map(|r| r.serial).collect();
        assert_eq!(hit, [3, 1], "ok→warn and warn→danger ping; danger→warn recovery doesn't");
    }

}
