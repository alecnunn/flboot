//! In-process diffing via `objdiff-core`, replacing shell-outs to `objdiff-cli`.
//!
//! The commands that need a diff (`fl diff`, `fl dis`, `fl progress`) used to
//! spawn `objdiff-cli`, ask it for JSON, and index into `serde_json::Value` by
//! string key. This module calls the same engine directly, so the data stays
//! typed and no process boundary or JSON round trip is involved.
//!
//! Path derivation mirrors `codegen::write_objdiff` exactly: `objdiff.json` is
//! still generated for the objdiff GUI, but it is no longer an input to us, so
//! the two must agree on where a unit's target and base objects live.

use std::path::Path;

use anyhow::Result;
use objdiff_core::{
    diff::{
        self, DiffObjConfig, DiffObjsResult, DiffSide, InstructionDiffKind, InstructionDiffRow,
        MappingConfig, SymbolDiff,
        display::{DiffText, display_row},
    },
    obj::{self, Object},
};

use crate::model::Objects;

/// A unit's target (delinked original) and base (our build) objects, plus the
/// diff between them. Either side may be absent: `base` is `None` before the
/// unit has ever been compiled, and callers report that rather than crashing.
pub struct UnitDiff {
    pub target: Option<Object>,
    pub base: Option<Object>,
    pub result: DiffObjsResult,
    config: DiffObjConfig,
}

impl UnitDiff {
    /// The object and diff for one side, paired so a symbol index resolves
    /// against the object it came from.
    pub fn side(&self, side: DiffSide) -> Option<(&Object, &diff::ObjectDiff)> {
        let (obj, diff) = match side {
            DiffSide::Target => (self.target.as_ref(), self.result.left.as_ref()),
            DiffSide::Base => (self.base.as_ref(), self.result.right.as_ref()),
        };
        obj.zip(diff)
    }

    /// Builds rendered views of the unit's functions: one named `symbol`, or
    /// every function in the target when `symbol` is `None`.
    ///
    /// Sides are paired through objdiff's own symbol matching
    /// (`SymbolDiff::target_symbol`) rather than by re-matching names, so a
    /// renamed-but-matched symbol still lines up.
    pub fn function_views(&self, symbol: Option<&str>) -> Result<Vec<FnView>> {
        let want = symbol.map(crate::dev::symbol_key);

        // Prefer the target as the driving side; fall back to the base so a
        // unit with no delinked object still renders something useful.
        let (obj, obj_diff, side) = match self.side(DiffSide::Target) {
            Some((o, d)) => (o, d, DiffSide::Target),
            None => match self.side(DiffSide::Base) {
                Some((o, d)) => (o, d, DiffSide::Base),
                None => return Ok(Vec::new()),
            },
        };

        let mut views = Vec::new();
        for (idx, sym) in obj.symbols.iter().enumerate() {
            if sym.kind != obj::SymbolKind::Function {
                continue;
            }
            let key = crate::dev::symbol_key(&sym.name);
            if want.as_deref().is_some_and(|w| w != key) {
                continue;
            }

            let sym_diff = obj_diff
                .symbols
                .get(idx)
                .ok_or_else(|| anyhow::anyhow!("diff has no entry for symbol {}", sym.name))?;
            let near = SideView {
                match_percent: sym_diff.match_percent,
                rows: render_symbol(obj, idx, sym_diff, &self.config)?,
            };

            // The opposite side, if objdiff matched this symbol to one there.
            let far = match (sym_diff.target_symbol, self.side(other_side(side))) {
                (Some(far_idx), Some((far_obj, far_diff))) => {
                    let far_sym = far_diff.symbols.get(far_idx).ok_or_else(|| {
                        anyhow::anyhow!("diff has no entry for matched symbol {far_idx}")
                    })?;
                    Some(SideView {
                        match_percent: far_sym.match_percent,
                        rows: render_symbol(far_obj, far_idx, far_sym, &self.config)?,
                    })
                }
                _ => None,
            };

            let (target, base) = match side {
                DiffSide::Target => (Some(near), far),
                DiffSide::Base => (far, Some(near)),
            };
            views.push(FnView { name: sym.name.clone(), size: sym.size, target, base });
        }
        Ok(views)
    }
}

fn other_side(side: DiffSide) -> DiffSide {
    match side {
        DiffSide::Target => DiffSide::Base,
        DiffSide::Base => DiffSide::Target,
    }
}

/// One side of a function, rendered and scored.
pub struct SideView {
    pub match_percent: Option<f32>,
    pub rows: Vec<Row>,
}

/// A function as seen from both sides. Either side may be absent: the function
/// exists only in the target (not yet written), or only in our build (not in
/// the original).
pub struct FnView {
    pub name: String,
    pub size: u64,
    pub target: Option<SideView>,
    pub base: Option<SideView>,
}

/// Resolves a unit's target and base object paths the same way
/// `codegen::write_objdiff` writes them into `objdiff.json`.
///
/// A library may declare several objects; `objdiff.json` emits one unit per
/// source file, named after the source with its extension stripped. `fl diff`
/// used to select among them by passing `-u src/<unit>`, so we match on that
/// same name and fall back to the library's first object.
pub fn unit_paths(config_id: &str, objects: &Objects, unit: &str) -> Result<(String, String)> {
    let lib = objects
        .get(unit)
        .ok_or_else(|| anyhow::anyhow!("unknown unit: {unit}"))?;
    let delink_name = lib.delink.clone().unwrap_or_else(|| unit.to_string());
    let want = format!("src/{unit}");

    let (src, target) = lib
        .objects
        .iter()
        .find(|(src, _)| {
            crate::codegen::strip_last_extension(&crate::codegen::to_forward_path(src)) == want
        })
        .or_else(|| lib.objects.iter().next())
        .ok_or_else(|| anyhow::anyhow!("unit {unit} declares no objects"))?;

    let target_name = match target {
        Some(t) if !t.is_empty() => t.clone(),
        _ => src.clone(),
    };

    Ok((
        crate::codegen::get_delink_path(config_id, &delink_name, &target_name),
        crate::codegen::get_target_path(config_id, unit, src),
    ))
}

/// Reads an object, treating a missing file as absence rather than an error.
///
/// A unit that has never been built has no base object; `fl progress` and
/// `fl diff` both want to say so plainly instead of failing to open a path.
fn read_side(path: &str, side: DiffSide, config: &DiffObjConfig) -> Result<Option<Object>> {
    let path = Path::new(path);
    if !path.exists() {
        return Ok(None);
    }
    obj::read::read(path, config, side)
        .map(Some)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))
}

/// Loads a unit's two objects and diffs them in-process.
pub fn diff_unit(config_id: &str, objects: &Objects, unit: &str) -> Result<UnitDiff> {
    let (target_path, base_path) = unit_paths(config_id, objects, unit)?;
    let config = DiffObjConfig::default();

    let target = read_side(&target_path, DiffSide::Target, &config)?;
    let base = read_side(&base_path, DiffSide::Base, &config)?;
    if target.is_none() && base.is_none() {
        anyhow::bail!(
            "no objects for {unit}: neither {target_path} nor {base_path} exists (run `fl bootstrap`, then `fl build {unit}`)"
        );
    }

    let result = diff::diff_objs(
        target.as_ref(),
        base.as_ref(),
        None,
        &config,
        &MappingConfig::default(),
    )?;
    Ok(UnitDiff { target, base, result, config })
}

/// One rendered instruction line, with whether the diff considers it changed.
pub struct Row {
    pub text: String,
    pub changed: bool,
}

/// Renders one instruction row to text.
///
/// The segment-to-text mapping mirrors `objdiff-cli`'s own TUI renderer so the
/// mnemonics, arguments and addends read the way objdiff prints them. Note the
/// address is symbol-relative (objdiff subtracts the symbol's address), where
/// the old JSON path carried an absolute section offset.
pub fn render_row(
    obj: &Object,
    symbol_index: usize,
    ins_row: &InstructionDiffRow,
    config: &DiffObjConfig,
) -> Result<Row> {
    let mut text = String::new();
    display_row(obj, symbol_index, ins_row, config, |segment| {
        let rendered = match segment.text {
            DiffText::Basic(s) => s.to_string(),
            DiffText::Line(num) => format!("{num} "),
            DiffText::Address(addr) => format!("{addr:x}:"),
            DiffText::Opcode(mnemonic, _) => format!("{mnemonic} "),
            DiffText::Argument(arg) => arg.to_string(),
            DiffText::BranchDest(addr) => format!("{addr:x}"),
            DiffText::BranchArrow(_) => " ~> ".to_string(),
            DiffText::Symbol(sym) => sym.demangled_name.as_ref().unwrap_or(&sym.name).clone(),
            DiffText::Addend(addend) => match addend.cmp(&0i64) {
                std::cmp::Ordering::Greater => format!("+{addend:#x}"),
                std::cmp::Ordering::Less => format!("-{:#x}", -addend),
                std::cmp::Ordering::Equal => String::new(),
            },
            DiffText::Spacing(n) => " ".repeat(n as usize),
            DiffText::Eol => return Ok(()),
        };
        text.push_str(&rendered);
        for _ in rendered.len()..segment.pad_to as usize {
            text.push(' ');
        }
        Ok(())
    })?;

    Ok(Row {
        text: text.trim_end().to_string(),
        changed: ins_row.kind != InstructionDiffKind::None,
    })
}

/// Renders every instruction row of a symbol.
fn render_symbol(
    obj: &Object,
    symbol_index: usize,
    symbol_diff: &SymbolDiff,
    config: &DiffObjConfig,
) -> Result<Vec<Row>> {
    symbol_diff
        .instruction_rows
        .iter()
        .map(|row| render_row(obj, symbol_index, row, config))
        .collect()
}

/// Formats a match percentage the way the old JSON path did, or `-` when the
/// side is absent or unscored.
pub fn fmt_pct(match_percent: Option<f32>) -> String {
    match_percent
        .map(|p| format!("{p:.2}"))
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn objects(json: &str) -> Objects {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn unit_paths_match_objdiff_json_derivation() {
        let objs = objects(
            r#"{ "x86math.dll": {
                "progress_category": "x", "cflags": "x", "idapro": "x",
                "objects": { "src/x86math.dll.cpp": "x86math.dll.obj" }
            } }"#,
        );
        let (target, base) = unit_paths("cfgid", &objs, "x86math.dll").unwrap();
        assert_eq!(target, "build/cfgid/delink/x86math.dll/x86math.dll.obj");
        assert_eq!(base, "build/cfgid/obj/x86math.dll/src/x86math.dll.obj");
    }

    /// `delink` renames the delink output directory; the base path is unaffected.
    #[test]
    fn unit_paths_honor_delink_override() {
        let objs = objects(
            r#"{ "Common.dll": {
                "progress_category": "x", "cflags": "x", "idapro": "x", "delink": "common",
                "objects": { "src/Common.dll.cpp": "Common.dll.obj" }
            } }"#,
        );
        let (target, base) = unit_paths("cfgid", &objs, "Common.dll").unwrap();
        assert_eq!(target, "build/cfgid/delink/common/Common.dll.obj");
        assert_eq!(base, "build/cfgid/obj/Common.dll/src/Common.dll.obj");
    }

    /// A null/empty target falls back to the source name, matching write_objdiff.
    #[test]
    fn unit_paths_fall_back_to_source_name() {
        let objs = objects(
            r#"{ "A.dll": {
                "progress_category": "x", "cflags": "x", "idapro": "x",
                "objects": { "src/A.dll.obj": null }
            } }"#,
        );
        let (target, _) = unit_paths("cfgid", &objs, "A.dll").unwrap();
        assert_eq!(target, "build/cfgid/delink/A.dll/src/A.dll.obj");
    }

    #[test]
    fn unit_paths_rejects_unknown_unit() {
        let objs = objects(r#"{}"#);
        assert!(unit_paths("cfgid", &objs, "nope.dll").is_err());
    }

    /// A library with several objects selects the one whose objdiff unit name
    /// matches `src/<unit>`, not merely the first declared.
    #[test]
    fn unit_paths_select_matching_object_among_several() {
        let objs = objects(
            r#"{ "Multi.dll": {
                "progress_category": "x", "cflags": "x", "idapro": "x",
                "objects": {
                    "src/other.cpp": "other.obj",
                    "src/Multi.dll.cpp": "Multi.dll.obj"
                }
            } }"#,
        );
        let (target, base) = unit_paths("cfgid", &objs, "Multi.dll").unwrap();
        assert_eq!(target, "build/cfgid/delink/Multi.dll/Multi.dll.obj");
        assert_eq!(base, "build/cfgid/obj/Multi.dll/src/Multi.dll.obj");
    }
}
