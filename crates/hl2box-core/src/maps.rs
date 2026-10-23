//! Map discovery for the launch page's start-map picker.
//!
//! Maps live in the mod's `maps/` folder and, for the base game, in the
//! engine's `hl2/maps/`. Only names the launch validator accepts are listed,
//! so a picked entry can never produce an invalid `+map` argument.

use std::collections::BTreeSet;
use std::path::Path;

use crate::launch::valid_map_name;
use crate::paths::Paths;

/// Map names (without `.bsp`), sorted and de-duplicated across both folders.
pub fn list_maps(paths: &Paths) -> Vec<String> {
    let mut set = BTreeSet::new();
    let dirs = [
        paths.mod_root().join("maps"),
        paths.engine_root.join("hl2").join("maps"),
    ];
    for dir in dirs {
        collect(&dir, &mut set);
    }
    set.into_iter().collect()
}

fn collect(dir: &Path, out: &mut BTreeSet<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let stem = name
            .strip_suffix(".bsp")
            .or_else(|| name.strip_suffix(".BSP"));
        if let Some(stem) = stem {
            if valid_map_name(stem) {
                out.insert(stem.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_both_folders_and_skips_bad_names() {
        let base = std::env::temp_dir().join("hl2box_maps_test");
        let _ = std::fs::remove_dir_all(&base);
        let paths = Paths::new(&base, "hl2sb");
        std::fs::create_dir_all(paths.mod_root().join("maps")).unwrap();
        std::fs::create_dir_all(base.join("hl2").join("maps")).unwrap();
        std::fs::write(paths.mod_root().join("maps").join("gm_a.bsp"), b"").unwrap();
        std::fs::write(paths.mod_root().join("maps").join("bad name.bsp"), b"").unwrap();
        std::fs::write(paths.mod_root().join("maps").join("notmap.txt"), b"").unwrap();
        std::fs::write(base.join("hl2").join("maps").join("d1_town.bsp"), b"").unwrap();
        std::fs::write(base.join("hl2").join("maps").join("gm_a.bsp"), b"").unwrap();
        assert_eq!(list_maps(&paths), vec!["d1_town", "gm_a"]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
