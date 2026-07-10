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
        display::{DiffText, DiffTextColor, display_row},
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

/// The diff configuration `objdiff-cli report generate` uses, which differs
/// from the one `objdiff-cli diff` uses.
///
/// `function_reloc_diffs: None` is the consequential one: a function whose only
/// difference is the *name* of a relocation target still counts as matched. Our
/// delinked objects and our build disagree on symbol decoration (for instance
/// `__delink_ida_const_start` against `___delink_ida_const_start`), and progress
/// deliberately does not penalize that. Using the default config here undercounts
/// matched functions.
pub fn report_config() -> DiffObjConfig {
    DiffObjConfig {
        function_reloc_diffs: diff::FunctionRelocDiffs::None,
        combine_data_sections: true,
        combine_text_sections: true,
        ppc_calculate_pool_relocations: false,
        ..Default::default()
    }
}

/// Diffs whichever of the two objects exist, under the default (side-by-side)
/// config. Absent files are not an error: callers decide what a missing target
/// or base means for them.
pub fn diff_paths(target_path: &str, base_path: &str) -> Result<UnitDiff> {
    diff_paths_with(target_path, base_path, DiffObjConfig::default())
}

/// Diffs whichever of the two objects exist, under an explicit config.
pub fn diff_paths_with(
    target_path: &str,
    base_path: &str,
    config: DiffObjConfig,
) -> Result<UnitDiff> {
    let target = read_side(target_path, DiffSide::Target, &config)?;
    let base = read_side(base_path, DiffSide::Base, &config)?;
    let result = diff::diff_objs(
        target.as_ref(),
        base.as_ref(),
        None,
        &config,
        &MappingConfig::default(),
    )?;
    Ok(UnitDiff { target, base, result, config })
}

/// Loads a unit's two objects and diffs them in-process.
pub fn diff_unit(config_id: &str, objects: &Objects, unit: &str) -> Result<UnitDiff> {
    let (target_path, base_path) = unit_paths(config_id, objects, unit)?;
    let unit_diff = diff_paths(&target_path, &base_path)?;
    if unit_diff.target.is_none() && unit_diff.base.is_none() {
        anyhow::bail!(
            "no objects for {unit}: neither {target_path} nor {base_path} exists (run `fl bootstrap`, then `fl build {unit}`)"
        );
    }
    Ok(unit_diff)
}

/// One rendered instruction line, with whether the diff considers it changed.
///
/// `text` and `colored` hold the same content; `colored` additionally carries
/// ANSI escapes. Both are kept because escapes inflate a string's byte length
/// without occupying columns, so padding must be measured against `text`.
pub struct Row {
    pub text: String,
    pub colored: String,
    pub changed: bool,
    /// When this row branches elsewhere: the destination row index and
    /// objdiff's branch index, which selects the arrow's color.
    pub branch_to: Option<(usize, u32)>,
}

impl Row {
    /// The rendered form for the given color mode.
    pub fn display(&self, color: bool) -> &str {
        if color { &self.colored } else { &self.text }
    }

    /// Printed column width, which the escapes in `colored` do not contribute to.
    pub fn width(&self) -> usize {
        self.text.chars().count()
    }
}

const RESET: &str = "\x1b[0m";

/// The color objdiff assigns to a branch, so a drawn arrow matches the hue the
/// engine would have given its own `~>` marker.
pub fn branch_color(branch_idx: u32) -> &'static str {
    ansi_for(DiffTextColor::Rotating(branch_idx as u8))
}

/// Maps objdiff's semantic segment colors onto ANSI escapes. `Normal` is left
/// unstyled so ordinary instruction text keeps the terminal's own foreground
/// rather than being forced to grey.
fn ansi_for(color: DiffTextColor) -> &'static str {
    match color {
        DiffTextColor::Normal => "",
        DiffTextColor::Dim => "\x1b[2m",
        DiffTextColor::Bright => "\x1b[97m",
        DiffTextColor::DataFlow => "\x1b[96m",
        DiffTextColor::Replace => "\x1b[36m",
        DiffTextColor::Delete => "\x1b[31m",
        DiffTextColor::Insert => "\x1b[32m",
        // Branch arrows cycle through colors so nested branches stay tellable
        // apart; mirror that with the six standard hues.
        DiffTextColor::Rotating(i) => match i % 6 {
            0 => "\x1b[35m",
            1 => "\x1b[33m",
            2 => "\x1b[34m",
            3 => "\x1b[31m",
            4 => "\x1b[32m",
            _ => "\x1b[36m",
        },
    }
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
    let mut parts: Vec<(&'static str, String)> = Vec::new();
    display_row(obj, symbol_index, ins_row, config, |segment| {
        let rendered = match segment.text {
            DiffText::Basic(s) => s.to_string(),
            // Source line numbers come from our build's debug info; the delinked
            // target has none, so emitting them would shift the two columns out
            // of alignment against each other.
            DiffText::Line(_) => return Ok(()),
            DiffText::Address(addr) => format!("{addr:x}:"),
            DiffText::Opcode(mnemonic, _) => format!("{mnemonic} "),
            DiffText::Argument(arg) => arg.to_string(),
            DiffText::BranchDest(addr) => format!("{addr:x}"),
            // objdiff emits `~>` both before the mnemonic (this row is a branch
            // destination) and after the operands (this row branches away). We
            // draw a connected gutter instead, but keep the four columns so the
            // mnemonic stays aligned with rows that have no branch, where
            // objdiff emits Spacing(4).
            DiffText::BranchArrow(_) => "    ".to_string(),
            DiffText::Symbol(sym) => sym.demangled_name.as_ref().unwrap_or(&sym.name).clone(),
            DiffText::Addend(addend) => match addend.cmp(&0i64) {
                std::cmp::Ordering::Greater => format!("+{addend:#x}"),
                std::cmp::Ordering::Less => format!("-{:#x}", -addend),
                std::cmp::Ordering::Equal => String::new(),
            },
            DiffText::Spacing(n) => " ".repeat(n as usize),
            DiffText::Eol => return Ok(()),
        };

        // Padding is unstyled, and counts in columns rather than bytes.
        let pad = (segment.pad_to as usize).saturating_sub(rendered.chars().count());
        parts.push((ansi_for(segment.color), rendered));
        if pad > 0 {
            parts.push(("", " ".repeat(pad)));
        }
        Ok(())
    })?;

    let (text, colored) = assemble(parts);
    Ok(Row {
        text,
        colored,
        changed: ins_row.kind != InstructionDiffKind::None,
        branch_to: ins_row
            .branch_to
            .as_ref()
            .map(|b| (b.ins_idx as usize, b.branch_idx)),
    })
}

/// Builds a row's plain and colored forms from styled parts, trimming trailing
/// whitespace *before* escapes are woven in.
///
/// Trimming the finished colored string does not work: a segment like the
/// branch arrow renders as `" ~> "`, so its trailing space ends up inside the
/// escape wrap and `trim_end` sees the reset sequence as the last characters.
/// The colored row would then be one column wider than the plain one and the
/// side-by-side separator would drift.
fn assemble(mut parts: Vec<(&'static str, String)>) -> (String, String) {
    while let Some((_, last)) = parts.last_mut() {
        let trimmed = last.trim_end();
        if trimmed.len() == last.len() {
            break;
        }
        last.truncate(trimmed.len());
        if last.is_empty() {
            parts.pop();
        } else {
            break;
        }
    }

    let mut text = String::new();
    let mut colored = String::new();
    for (escape, part) in &parts {
        text.push_str(part);
        if escape.is_empty() {
            colored.push_str(part);
        } else {
            colored.push_str(escape);
            colored.push_str(part);
            colored.push_str(RESET);
        }
    }
    (text, colored)
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

/// Match statistics for one unit, or for a whole project once summed.
///
/// Ported from `objdiff-cli`'s `report_object`, which lives in the CLI rather
/// than in `objdiff-core` -- core only holds the protobuf `Measures` type,
/// behind the `bindings` feature. Reimplementing the arithmetic avoids pulling
/// a protobuf toolchain in to populate a struct we would fill ourselves anyway.
///
/// `fuzzy_match_percent` is size-weighted while accumulating and only becomes a
/// percentage after `calc_fuzzy_match_percent`, exactly as upstream does it, so
/// summing units and then finalizing yields objdiff's own overall number.
#[derive(Default, Clone, Copy)]
pub struct Measures {
    pub fuzzy_match_percent: f32,
    pub total_code: u64,
    pub matched_code: u64,
    pub matched_code_percent: f32,
    pub total_data: u64,
    pub matched_data: u64,
    pub matched_data_percent: f32,
    pub total_functions: u32,
    pub matched_functions: u32,
    pub matched_functions_percent: f32,
    pub total_units: u32,
}

impl std::ops::AddAssign for Measures {
    fn add_assign(&mut self, other: Self) {
        // Undo the other's averaging so the sum stays size-weighted.
        self.fuzzy_match_percent += other.fuzzy_match_percent * other.total_code as f32;
        self.total_code += other.total_code;
        self.matched_code += other.matched_code;
        self.total_data += other.total_data;
        self.matched_data += other.matched_data;
        self.total_functions += other.total_functions;
        self.matched_functions += other.matched_functions;
        self.total_units += other.total_units;
    }
}

impl Measures {
    /// Averages the size-weighted fuzzy percentage over total code bytes.
    pub fn calc_fuzzy_match_percent(&mut self) {
        self.fuzzy_match_percent = if self.total_code == 0 {
            100.0
        } else {
            self.fuzzy_match_percent / self.total_code as f32
        };
    }

    pub fn calc_matched_percent(&mut self) {
        let pct = |m: u64, t: u64| if t == 0 { 100.0 } else { m as f32 / t as f32 * 100.0 };
        self.matched_code_percent = pct(self.matched_code, self.total_code);
        self.matched_data_percent = pct(self.matched_data, self.total_data);
        self.matched_functions_percent =
            pct(self.matched_functions as u64, self.total_functions as u64);
    }

    /// Sums per-unit measures and finalizes the derived percentages.
    pub fn total(units: impl IntoIterator<Item = Measures>) -> Measures {
        let mut m = Measures::default();
        for u in units {
            m += u;
        }
        m.calc_fuzzy_match_percent();
        m.calc_matched_percent();
        m
    }
}

/// One function's entry in a progress report.
pub struct FnItem {
    pub name: String,
    pub size: u64,
    pub match_percent: f32,
    pub address: Option<u64>,
}

/// Computes a unit's measures and its per-function breakdown.
///
/// Mirrors `report_object`: sections of unknown kind are skipped, data and BSS
/// contribute to the data totals, and only sized, non-hidden, non-ignored
/// symbols in code sections count as functions. A symbol objdiff could not
/// score counts as 0% -- our `objdiff.json` never marks a unit `complete`, so
/// upstream's "assume 100% when complete" branch cannot apply.
pub fn unit_measures(unit: &UnitDiff) -> (Measures, Vec<FnItem>) {
    let mut measures = Measures { total_units: 1, ..Default::default() };
    let mut functions = Vec::new();

    let (Some(obj), Some(obj_diff)) = (
        unit.target.as_ref().or(unit.base.as_ref()),
        unit.result.left.as_ref().or(unit.result.right.as_ref()),
    ) else {
        return (measures, functions);
    };

    for ((section_idx, section), section_diff) in
        obj.sections.iter().enumerate().zip(&obj_diff.sections)
    {
        if section.kind == obj::SectionKind::Unknown {
            continue;
        }
        let section_match_percent = section_diff.match_percent.unwrap_or(0.0);

        if matches!(section.kind, obj::SectionKind::Data | obj::SectionKind::Bss) {
            measures.total_data += section.size;
            if section_match_percent == 100.0 {
                measures.matched_data += section.size;
            }
            continue;
        }

        for (symbol, symbol_diff) in obj.symbols.iter().zip(&obj_diff.symbols) {
            if symbol.section != Some(section_idx)
                || symbol.size == 0
                || symbol.flags.contains(obj::SymbolFlag::Hidden)
                || symbol.flags.contains(obj::SymbolFlag::Ignored)
                || symbol.kind == obj::SymbolKind::Section
            {
                continue;
            }
            let match_percent = symbol_diff.match_percent.unwrap_or(0.0);
            measures.fuzzy_match_percent += match_percent * symbol.size as f32;
            measures.total_code += symbol.size;
            measures.total_functions += 1;
            if match_percent == 100.0 {
                measures.matched_code += symbol.size;
                measures.matched_functions += 1;
            }
            functions.push(FnItem {
                name: symbol.name.clone(),
                size: symbol.size,
                match_percent,
                address: symbol.address.checked_sub(section.address),
            });
        }
    }

    functions.sort_by(|a, b| {
        a.address
            .unwrap_or(u64::MAX)
            .cmp(&b.address.unwrap_or(u64::MAX))
            .then_with(|| a.size.cmp(&b.size))
    });

    measures.calc_fuzzy_match_percent();
    measures.calc_matched_percent();
    (measures, functions)
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

    /// An empty unit is vacuously complete, matching upstream's guard against
    /// dividing by zero code bytes.
    #[test]
    fn measures_of_empty_unit_are_fully_matched() {
        let mut m = Measures::default();
        m.calc_fuzzy_match_percent();
        m.calc_matched_percent();
        assert_eq!(m.fuzzy_match_percent, 100.0);
        assert_eq!(m.matched_code_percent, 100.0);
        assert_eq!(m.matched_data_percent, 100.0);
        assert_eq!(m.matched_functions_percent, 100.0);
    }

    #[test]
    fn calc_fuzzy_match_percent_averages_over_code_bytes() {
        // Two functions: 100 bytes at 100%, 100 bytes at 50%.
        let mut m = Measures { fuzzy_match_percent: 100.0 * 100.0 + 50.0 * 100.0, total_code: 200, ..Default::default() };
        m.calc_fuzzy_match_percent();
        assert_eq!(m.fuzzy_match_percent, 75.0);
    }

    #[test]
    fn calc_matched_percent_is_a_ratio_of_bytes_and_counts() {
        let mut m = Measures {
            total_code: 400,
            matched_code: 100,
            total_functions: 8,
            matched_functions: 2,
            ..Default::default()
        };
        m.calc_matched_percent();
        assert_eq!(m.matched_code_percent, 25.0);
        assert_eq!(m.matched_functions_percent, 25.0);
    }

    /// Summing units must re-weight each unit's averaged percentage by its own
    /// code size, or a tiny fully-matched unit would drag the total up as much
    /// as a huge one.
    #[test]
    fn total_reweights_unit_percentages_by_size() {
        let mut small = Measures { fuzzy_match_percent: 100.0 * 10.0, total_code: 10, matched_code: 10, total_units: 1, total_functions: 1, matched_functions: 1, ..Default::default() };
        small.calc_fuzzy_match_percent();
        assert_eq!(small.fuzzy_match_percent, 100.0);

        let mut big = Measures { fuzzy_match_percent: 0.0, total_code: 990, total_units: 1, total_functions: 99, ..Default::default() };
        big.calc_fuzzy_match_percent();
        assert_eq!(big.fuzzy_match_percent, 0.0);

        let total = Measures::total([small, big]);
        assert_eq!(total.total_code, 1000);
        assert_eq!(total.total_units, 2);
        assert_eq!(total.total_functions, 100);
        assert_eq!(total.matched_functions, 1);
        // 10 of 1000 bytes fully matched, so 1% -- not the 50% a naive mean gives.
        assert_eq!(total.fuzzy_match_percent, 1.0);
        assert_eq!(total.matched_code_percent, 1.0);
    }

    /// The report config must relax relocation-name differences, or progress
    /// undercounts every function that references a differently-decorated
    /// symbol. See the `__delink` vs `___delink` mismatch in the real project.
    #[test]
    fn report_config_ignores_relocation_name_differences() {
        let cfg = report_config();
        assert_eq!(cfg.function_reloc_diffs, diff::FunctionRelocDiffs::None);
        assert!(cfg.combine_data_sections);
        assert!(cfg.combine_text_sections);
        // The side-by-side default must NOT relax them, so `fl diff` keeps
        // showing the mismatch that `fl progress` forgives.
        assert_ne!(DiffObjConfig::default().function_reloc_diffs, diff::FunctionRelocDiffs::None);
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Regression: a styled part ending in a space (objdiff renders the branch
    /// arrow as `" ~> "`) put that space inside the escape wrap, so trimming
    /// the finished colored string was a no-op and the colored row came out one
    /// column wider than the plain one.
    #[test]
    fn assemble_trims_trailing_space_inside_a_styled_part() {
        let (text, colored) = assemble(vec![
            ("", "jl short 5".to_string()),
            ("\x1b[35m", " ~> ".to_string()),
        ]);
        assert_eq!(text, "jl short 5 ~>");
        assert_eq!(colored, "jl short 5\x1b[35m ~>\x1b[0m");
        assert_eq!(strip_ansi(&colored), text);
    }

    /// Trailing pad parts are dropped entirely, not left as empty escapes.
    #[test]
    fn assemble_drops_trailing_padding_parts() {
        let (text, colored) = assemble(vec![
            ("\x1b[2m", "0:".to_string()),
            ("", "   ".to_string()),
        ]);
        assert_eq!(text, "0:");
        assert_eq!(colored, "\x1b[2m0:\x1b[0m");
    }

    /// Stripping escapes must always recover the plain form, whatever the mix.
    #[test]
    fn assemble_colored_strips_back_to_plain() {
        let (text, colored) = assemble(vec![
            ("\x1b[2m", "7:".to_string()),
            ("", "  ".to_string()),
            ("", "or ".to_string()),
            ("\x1b[35m", "ch".to_string()),
            ("", ", ".to_string()),
            ("\x1b[33m", "0xfc".to_string()),
        ]);
        assert_eq!(strip_ansi(&colored), text);
        assert_eq!(text, "7:  or ch, 0xfc");
    }

    #[test]
    fn assemble_handles_only_whitespace() {
        let (text, colored) = assemble(vec![("", "   ".to_string())]);
        assert_eq!(text, "");
        assert_eq!(colored, "");
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
