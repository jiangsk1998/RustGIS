//! QGIS renderers (`<renderer-v2>`) as RustGIS ones: single symbol, categorized (Unique Values),
//! graduated (Graduated Colors), rule-based (Unique Values where every rule picks values of one
//! field), the null renderer, and the renderers that wrap another (point cluster and
//! displacement, inverted polygons, merged features) by the one inside.

use std::collections::BTreeMap;

use roxmltree::Node;

use super::symbol::{self, Converted};
use super::{QGIS_SCALE, Report, Style, child, expr, has_effect};
use crate::color::Ramp;
use crate::symbol_by::SymbolBy;
use crate::symbology::{ClassMethod, LineStroke, Renderer, Symbol};

/// The most classes a renderer is imported with: every feature is matched against them.
const MAX_CLASSES: usize = 4096;

/// Reads the renderer `r` into `style`: its renderer, a line layer's strokes, and its reference
/// scale and symbol levels.
pub(super) fn read(r: Node, layer: &str, style: &mut Style, report: &mut Report) {
    read_depth(r, layer, style, report, 0);
}

fn read_depth(r: Node, layer: &str, style: &mut Style, report: &mut Report, depth: usize) {
    // QGIS's reference scale (its own scale, at 96 dpi): kept for sizes in map units.
    let reference = r.attribute("referencescale").and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && *v > 0.0);
    if let Some(s) = reference {
        style.reference_scale = Some((s * QGIS_SCALE).clamp(1.0, 1e9));
    }
    if r.attribute("symbollevels") == Some("1") {
        style.stroke_levels = Some(true);
    }
    if r.attribute("enableorderby") == Some("1") {
        report.approximated(layer, "features drawn in the order QGIS's order-by sets, drawn in the data's order");
    }
    if has_effect(r) {
        report.skipped(layer, "draw effects (shadows, glows, blurs and the like)");
    }
    // The symbols the renderer names, by name.
    let symbols: BTreeMap<String, Node> = child(r, "symbols")
        .map(|s| {
            s.children()
                .filter(|c| c.has_tag_name("symbol"))
                .filter_map(|c| Some((c.attribute("name")?.to_string(), c)))
                .take(MAX_CLASSES * 2)
                .collect()
        })
        .unwrap_or_default();
    // The first symbol turned by a field turns the layer's markers.
    let rotation = std::cell::RefCell::new(None);
    let turned = symbols.values().any(|n| symbol::turns_by_field(*n));
    // Every symbol's data-defined expressions: the first that has any are the layer's.
    let by: std::cell::RefCell<Vec<SymbolBy>> = std::cell::RefCell::new(Vec::new());
    let read = |n: Node, report: &mut Report| {
        let c = symbol::read(n, reference, turned, layer, report);
        if let (Some(r), Ok(mut had)) = (&c.rotation, rotation.try_borrow_mut())
            && *had.get_or_insert_with(|| r.clone()) != *r
        {
            report.approximated(layer, "marker layers turned by different fields, all turned by the first");
        }
        if let Ok(mut all) = by.try_borrow_mut()
            && !all.contains(&c.by)
        {
            all.push(c.by.clone());
        }
        c
    };
    let sym = |name: Option<&str>, report: &mut Report| -> Option<Converted> { symbols.get(name?).map(|n| read(*n, report)) };
    let kind = r.attribute("type").unwrap_or_default();
    match kind {
        "singleSymbol" => {
            let Some(c) = sym(Some("0"), report).or_else(|| symbols.values().next().map(|n| read(*n, report))) else {
                return;
            };
            style.line_strokes = Some(if c.stacked() { c.strokes.clone() } else { Vec::new() });
            style.fill_layers = Some(c.fills.clone());
            style.renderer = Some(Renderer::Single(c.symbol));
        }
        "nullSymbol" => {
            style.renderer = Some(Renderer::Single(symbol::hidden()));
            style.line_strokes = Some(Vec::new());
            style.fill_layers = Some(Vec::new());
        }
        "categorizedSymbol" => categorized(r, layer, style, report, &sym),
        "graduatedSymbol" => graduated(r, layer, style, report, &sym),
        "RuleRenderer" => rules(r, layer, style, report, &sym),
        "pointCluster" | "pointDisplacement" | "invertedPolygonRenderer" | "mergedFeatureRenderer" | "25dRenderer" => {
            report.approximated(layer, format!("its {} renderer, drawn by the renderer inside it", wrapper_name(kind)));
            if let Some(inner) = child(r, "renderer-v2").filter(|_| depth < 8) {
                read_depth(inner, layer, style, report, depth + 1);
            }
        }
        "heatmapRenderer" => report.skipped(layer, "its heatmap renderer"),
        other => report.skipped(layer, format!("its {other} renderer")),
    }
    // A renderer read here (not by the one inside a wrapper) says whether its markers turn.
    let wrapper =
        matches!(kind, "pointCluster" | "pointDisplacement" | "invertedPolygonRenderer" | "mergedFeatureRenderer" | "25dRenderer");
    if style.renderer.is_some() && !wrapper {
        style.marker_rotation = Some(rotation.try_borrow().ok().and_then(|r| r.clone()));
        let mut all = by.try_borrow().map(|b| b.clone()).unwrap_or_default();
        // A switched-off class draws nothing: expressions applied to every feature would paint it.
        let off =
            r.descendants().any(|n| (n.has_tag_name("category") || n.has_tag_name("range")) && n.attribute("render") == Some("false"));
        if off && !all.iter().any(SymbolBy::is_empty) {
            all.push(SymbolBy::default());
        }
        // The expressions apply to every feature: kept only when every symbol has the same.
        let one = match all.as_slice() {
            [one] => one.clone(),
            _ => {
                if all.iter().any(|b| !b.is_empty()) {
                    report.skipped(layer, "data-defined settings that differ between its classes (or a class switched off)");
                }
                SymbolBy::default()
            }
        };
        style.symbol_by = Some(one);
    }
}

/// A renderer's classifying attribute as a field: `name` or `"name"`; `None` for an expression.
fn field_of(attr: &str, layer: &str, report: &mut Report) -> Option<String> {
    let a = attr.trim();
    let unquoted = a.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(a);
    if !unquoted.is_empty() && !unquoted.contains('"') && (a.starts_with('"') || unquoted.chars().all(|c| c.is_alphanumeric() || c == '_'))
    {
        return Some(unquoted.to_string());
    }
    report.skipped(layer, format!("classes by the expression `{a}` (only by a field)"));
    None
}

/// A switched-off class (`render="false"`): it draws nothing.
fn switched_off() -> Converted {
    Converted { symbol: symbol::hidden(), strokes: Vec::new(), rotation: None, by: Default::default(), fills: Vec::new() }
}

type SymbolOf<'a> = dyn Fn(Option<&str>, &mut Report) -> Option<Converted> + 'a;

fn categorized(r: Node, layer: &str, style: &mut Style, report: &mut Report, sym: &SymbolOf) {
    let cats: Vec<Node> = child(r, "categories").map(|c| c.children().filter(|n| n.has_tag_name("category")).collect()).unwrap_or_default();
    let Some(field) = field_of(r.attribute("attr").unwrap_or_default(), layer, report) else {
        // Drawn as one: the first category's symbol.
        if let Some(c) = cats.first().and_then(|c| sym(c.attribute("symbol"), report)) {
            style.fill_layers = Some(c.fills.clone());
            style.renderer = Some(Renderer::Single(c.symbol));
        }
        return;
    };
    let mut classes: Vec<(String, Symbol)> = Vec::new();
    let (mut class_labels, mut class_strokes) = (BTreeMap::new(), BTreeMap::new());
    let mut other = symbol::hidden();
    let mut other_strokes = Vec::new();
    let (mut class_fills, mut other_fills) = (BTreeMap::new(), Vec::new());
    for cat in &cats {
        // A switched-off class draws nothing; its symbol isn't read (its field would turn the rest).
        let c = if cat.attribute("render") == Some("false") {
            switched_off()
        } else {
            let Some(c) = sym(cat.attribute("symbol"), report) else { continue };
            c
        };
        // A category of several values (QGIS's merged categories) lists them as <val>s, or as a
        // list <Option>.
        let mut values: Vec<String> = cat
            .descendants()
            .filter(|v| v.has_tag_name("val") || (v.has_tag_name("Option") && v.attribute("type") == Some("QString")))
            .filter_map(|v| v.attribute("value").map(str::to_string))
            .collect();
        if values.is_empty() {
            values.push(cat.attribute("value").unwrap_or_default().to_string());
        }
        for v in values {
            // An empty value is QGIS's "all other values".
            if v.is_empty() || v == "NULL" {
                other = c.symbol;
                other_strokes = if c.stacked() { c.strokes.clone() } else { Vec::new() };
                other_fills.clone_from(&c.fills);
                continue;
            }
            if classes.len() >= MAX_CLASSES {
                report.skipped(layer, format!("categories past the first {MAX_CLASSES}"));
                break;
            }
            if let Some(label) = cat.attribute("label").filter(|l| *l != v && !l.is_empty()) {
                class_labels.insert(v.clone(), label.to_string());
            }
            if c.stacked() {
                class_strokes.insert(v.clone(), c.strokes.clone());
            }
            if !c.fills.is_empty() {
                class_fills.insert(v.clone(), c.fills.clone());
            }
            classes.push((v, c.symbol));
        }
    }
    style.line_strokes = Some(Vec::new());
    style.fill_layers = Some(Vec::new());
    style.renderer = Some(Renderer::Unique {
        field,
        classes,
        other,
        ramp: Ramp::default(),
        class_labels,
        other_label: None,
        class_strokes,
        other_strokes,
        class_fills,
        other_fills,
    });
}

fn graduated(r: Node, layer: &str, style: &mut Style, report: &mut Report, sym: &SymbolOf) {
    let mut ranges: Vec<(f64, f64, Converted, Option<String>)> = Vec::new();
    for rg in child(r, "ranges").map(|c| c.children().filter(|n| n.has_tag_name("range")).collect::<Vec<_>>()).unwrap_or_default() {
        let num = |k: &str| rg.attribute(k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
        let (Some(lower), Some(upper)) = (num("lower"), num("upper")) else { continue };
        let c = if rg.attribute("render") == Some("false") {
            switched_off()
        } else {
            let Some(c) = sym(rg.attribute("symbol"), report) else { continue };
            c
        };
        ranges.push((lower, upper, c, rg.attribute("label").map(str::to_string)));
        if ranges.len() >= MAX_CLASSES {
            report.skipped(layer, format!("ranges past the first {MAX_CLASSES}"));
            break;
        }
    }
    let Some(field) = field_of(r.attribute("attr").unwrap_or_default(), layer, report) else {
        if let Some((.., c, _)) = ranges.first() {
            style.fill_layers = Some(c.fills.clone());
            style.renderer = Some(Renderer::Single(c.symbol));
        }
        return;
    };
    if ranges.is_empty() {
        return;
    }
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    if ranges.windows(2).any(|w| w.first().map(|a| a.1) != w.get(1).map(|b| b.0)) {
        report.approximated(layer, "ranges with gaps or overlaps between them, drawn as if each began where the one before ends");
    }
    let min = ranges.first().map_or(0.0, |r| r.0);
    // QGIS draws nothing for a value outside every range; a RustGIS graduated renderer gives it the
    // first or last class. The import doesn't read the data, so it can't tell whether any value
    // falls outside: the note is always given, on purpose.
    report.approximated(layer, "values below the first range or above the last, drawn in the first or last class (QGIS leaves them out)");
    let mut class_labels = BTreeMap::new();
    let mut class_strokes: BTreeMap<usize, Vec<LineStroke>> = BTreeMap::new();
    let mut class_fills = BTreeMap::new();
    for (i, (.., c, label)) in ranges.iter().enumerate() {
        if let Some(l) = label.as_ref().filter(|l| !l.is_empty()) {
            class_labels.insert(i, l.clone());
        }
        if c.stacked() {
            class_strokes.insert(i, c.strokes.clone());
        }
        if !c.fills.is_empty() {
            class_fills.insert(i, c.fills.clone());
        }
    }
    style.line_strokes = Some(Vec::new());
    style.fill_layers = Some(Vec::new());
    style.renderer = Some(Renderer::Graduated {
        field,
        breaks: ranges.iter().map(|r| r.1).collect(),
        symbols: ranges.iter().map(|r| r.2.symbol).collect(),
        ramp: Ramp::default(),
        method: ClassMethod::default(),
        min,
        classes: ranges.len(),
        class_labels,
        class_strokes,
        class_fills,
    });
}

/// A rule of a rule-based renderer that draws: its filter and its parents' (`ELSE` for an else
/// rule), and whether a scale range applies along the way.
struct Rule<'a, 'i> {
    filters: Vec<String>,
    node: Node<'a, 'i>,
    scaled: bool,
}

fn rules(r: Node, layer: &str, style: &mut Style, report: &mut Report, sym: &SymbolOf) {
    let mut leaves = Vec::new();
    if let Some(root) = child(r, "rules") {
        flatten(root, Vec::new(), false, &mut leaves, 0);
    }
    // Rules that draw: those with a symbol and not switched off.
    let drawn: Vec<&Rule> =
        leaves.iter().filter(|l| l.node.attribute("symbol").is_some() && l.node.attribute("active") != Some("0")).collect();
    if drawn.iter().any(|l| l.scaled) {
        report.approximated(layer, "rules shown only at some scales, drawn at every scale");
    }
    // Every rule picks values of one field (an else rule for the rest): Unique Values.
    let mut field: Option<String> = None;
    let mut classes: Vec<(String, Converted, Option<String>)> = Vec::new();
    let mut other: Option<Converted> = None;
    let mut simple = !drawn.is_empty();
    for rule in &drawn {
        let Some(c) = sym(rule.node.attribute("symbol"), report) else { continue };
        let label = rule.node.attribute("label").map(str::to_string);
        if rule.filters.iter().any(|f| f.trim().eq_ignore_ascii_case("ELSE")) {
            if rule.filters.len() == 1 {
                other = Some(c);
                continue;
            }
            simple = false;
            break;
        }
        let parts: Option<Vec<(String, Vec<String>)>> = rule.filters.iter().map(|f| expr::equality(f)).collect();
        match parts.as_deref() {
            Some([(f, values)]) if field.as_ref().is_none_or(|g| g.eq_ignore_ascii_case(f)) => {
                field = Some(f.clone());
                for v in values {
                    if classes.len() < MAX_CLASSES {
                        classes.push((v.clone(), c.clone(), label.clone()));
                    }
                }
            }
            _ => {
                simple = false;
                break;
            }
        }
    }
    if simple && let Some(field) = field {
        let mut class_strokes = BTreeMap::new();
        let mut class_fills = BTreeMap::new();
        let mut class_labels = BTreeMap::new();
        for (v, c, label) in &classes {
            if c.stacked() {
                class_strokes.insert(v.clone(), c.strokes.clone());
            }
            if !c.fills.is_empty() {
                class_fills.insert(v.clone(), c.fills.clone());
            }
            if let Some(l) = label.as_ref().filter(|l| !l.is_empty() && *l != v) {
                class_labels.insert(v.clone(), l.clone());
            }
        }
        let (other_symbol, other_strokes, other_fills) = match &other {
            Some(c) => (c.symbol, if c.stacked() { c.strokes.clone() } else { Vec::new() }, c.fills.clone()),
            None => (symbol::hidden(), Vec::new(), Vec::new()),
        };
        style.line_strokes = Some(Vec::new());
        style.fill_layers = Some(Vec::new());
        style.renderer = Some(Renderer::Unique {
            field,
            classes: classes.into_iter().map(|(v, c, _)| (v, c.symbol)).collect(),
            other: other_symbol,
            ramp: Ramp::default(),
            class_labels,
            other_label: None,
            class_strokes,
            other_strokes,
            class_fills,
            other_fills,
        });
        return;
    }
    // One rule (or none but an else): its symbol for every feature.
    if drawn.len() == 1 && drawn.first().is_some_and(|r| r.filters.is_empty()) || drawn.is_empty() {
        if let Some(c) = drawn.first().and_then(|r| sym(r.node.attribute("symbol"), report)) {
            style.line_strokes = Some(if c.stacked() { c.strokes.clone() } else { Vec::new() });
            style.fill_layers = Some(c.fills.clone());
            style.renderer = Some(Renderer::Single(c.symbol));
        }
        return;
    }
    report
        .approximated(layer, format!("its {} rules (filters beyond one field's values), drawn with the first rule's symbol", drawn.len()));
    if let Some(c) = drawn.first().and_then(|r| sym(r.node.attribute("symbol"), report)) {
        style.line_strokes = Some(if c.stacked() { c.strokes.clone() } else { Vec::new() });
        style.fill_layers = Some(c.fills.clone());
        style.renderer = Some(Renderer::Single(c.symbol));
    }
}

/// The leaf rules under `node`, each with its own filter and its parents', and whether a scale
/// range applies along the way. Bounded in depth and number.
fn flatten<'a, 'i>(node: Node<'a, 'i>, filters: Vec<String>, scaled: bool, out: &mut Vec<Rule<'a, 'i>>, depth: usize) {
    if depth > 32 || out.len() >= MAX_CLASSES {
        return;
    }
    for rule in node.children().filter(|c| c.has_tag_name("rule")) {
        let mut f = filters.clone();
        if let Some(filter) = rule.attribute("filter").map(str::trim).filter(|f| !f.is_empty()) {
            f.push(filter.to_string());
        }
        let scaled = scaled || rule.attribute("scalemindenom").is_some() || rule.attribute("scalemaxdenom").is_some();
        let children = rule.children().any(|c| c.has_tag_name("rule"));
        if rule.attribute("symbol").is_some() || !children {
            out.push(Rule { filters: f.clone(), node: rule, scaled });
        }
        if children {
            flatten(rule, f, scaled, out, depth + 1);
        }
    }
}

fn wrapper_name(kind: &str) -> &'static str {
    match kind {
        "pointCluster" => "point cluster",
        "pointDisplacement" => "point displacement",
        "invertedPolygonRenderer" => "inverted polygon",
        "mergedFeatureRenderer" => "merged feature",
        _ => "2.5D",
    }
}
