//! Vector drawables, as an adaptive icon's layers and many launcher icons are made of: the
//! viewport, and each path with its fill, its stroke and the transform of the groups it is
//! in. The host draws them itself, so the install window shows such an icon before Android
//! runs.
//!
//! Colours that point at a colour resource are followed; a gradient or a colour state list
//! is drawn in its first colour. A colour that comes from the theme (`?attr/...`) cannot be
//! known without a theme, so such a fill is left out, as an unset fill is.

use super::arsc::ResourceTable;
use super::axml::{
    Attribute, Document, Element, Value, TYPE_FIRST_COLOR, TYPE_INT_DEC, TYPE_INT_HEX,
    TYPE_LAST_COLOR, TYPE_REFERENCE, TYPE_STRING,
};
use super::color;

const TYPE_FLOAT: u8 = 0x04;
const TYPE_DIMENSION: u8 = 0x05;
const TYPE_FRACTION: u8 = 0x06;

/// The framework's resource IDs of the attributes read here; each is also found by its
/// name, which aapt2 keeps.
mod attr {
    pub const VIEWPORT_WIDTH: u32 = 0x0101_0402;
    pub const VIEWPORT_HEIGHT: u32 = 0x0101_0403;
    pub const FILL_COLOR: u32 = 0x0101_0404;
    pub const PATH_DATA: u32 = 0x0101_0405;
    pub const STROKE_COLOR: u32 = 0x0101_0406;
    pub const STROKE_WIDTH: u32 = 0x0101_0407;
}

/// An affine transform `[a, b, c, d, e, f]`: x' = a x + c y + e, y' = b x + d y + f.
pub type Transform = [f32; 6];

pub const IDENTITY: Transform = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// One path of a vector drawable.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorPath {
    /// The path in the SVG path syntax Android uses.
    pub data: String,
    /// The fill as 0xAARRGGBB, its alpha already applied; None for no fill.
    pub fill: Option<u32>,
    pub stroke: Option<u32>,
    pub stroke_width: f32,
    pub even_odd: bool,
    /// From the path's coordinates to the viewport's.
    pub transform: Transform,
}

/// A vector drawable: its viewport, and how far it is inset in its bounds (a fraction of
/// each side, from an `<inset>` around it).
#[derive(Debug, Clone, PartialEq)]
pub struct Vector {
    pub width: f32,
    pub height: f32,
    pub inset: f32,
    pub paths: Vec<VectorPath>,
}

/// Reads compiled XML files of the APK, for the colour resources a path names.
pub type Open<'a> = dyn FnMut(&str) -> Option<Document> + 'a;

/// The attribute by its name, else by its framework ID.
pub(crate) fn find<'e>(element: &'e Element, id: u32, name: &str) -> Option<&'e Attribute> {
    element
        .attributes
        .iter()
        .find(|a| a.name == name)
        .or_else(|| {
            element
                .attributes
                .iter()
                .find(|a| id != 0 && a.resource_id == id)
        })
}

/// A complex value (a dimension or a fraction) as a plain number.
pub(crate) fn complex(data: u32) -> f32 {
    const RADIX: [f32; 4] = [
        1.0 / 256.0,
        1.0 / 256.0 / 128.0,
        1.0 / 256.0 / 32768.0,
        1.0 / 256.0 / 8_388_608.0,
    ];
    ((data & 0xffff_ff00) as i32) as f32 * RADIX[((data >> 4) & 3) as usize]
}

/// A number: a float, a dimension, a fraction, an integer, a reference to one, or its text.
pub(crate) fn number(table: &ResourceTable, attribute: &Attribute) -> Option<f32> {
    let mut value = attribute.value;
    for _ in 0..8 {
        match value.kind {
            TYPE_FLOAT => return Some(f32::from_bits(value.data)),
            TYPE_DIMENSION | TYPE_FRACTION => return Some(complex(value.data)),
            TYPE_INT_DEC | TYPE_INT_HEX => return Some(value.data as i32 as f32),
            TYPE_REFERENCE => value = table.values(value.data).first()?.1,
            _ => break,
        }
    }
    attribute.raw.as_deref()?.trim().parse().ok()
}

fn number_of(table: &ResourceTable, element: &Element, id: u32, name: &str) -> Option<f32> {
    number(table, find(element, id, name)?)
}

/// A colour value, following references to colour resources, and to gradients and colour
/// state lists, of which the first colour is taken.
fn color_value(
    table: &ResourceTable,
    open: &mut Open<'_>,
    value: Value,
    depth: usize,
) -> Option<u32> {
    if depth > 8 {
        return None;
    }
    match value.kind {
        kind if (TYPE_FIRST_COLOR..=TYPE_LAST_COLOR).contains(&kind) => {
            Some(color(kind, value.data))
        }
        TYPE_REFERENCE => {
            let values = table.values(value.data);
            let (_, next) = values
                .iter()
                .find(|(config, _)| config.is_default_language())
                .or(values.first())?;
            color_value(table, open, *next, depth + 1)
        }
        TYPE_STRING => {
            let path = table.string(value.data)?.to_string();
            let document = open(&path)?;
            first_color(table, open, &document, depth + 1)
        }
        _ => None,
    }
}

fn first_color(
    table: &ResourceTable,
    open: &mut Open<'_>,
    document: &Document,
    depth: usize,
) -> Option<u32> {
    for element in &document.elements {
        for name in ["startColor", "color", "centerColor", "endColor"] {
            if let Some(attribute) = find(element, 0, name) {
                if let Some(argb) = color_value(table, open, attribute.value, depth) {
                    return Some(argb);
                }
            }
        }
    }
    None
}

fn color_of(
    table: &ResourceTable,
    open: &mut Open<'_>,
    element: &Element,
    id: u32,
    name: &str,
) -> Option<u32> {
    let attribute = find(element, id, name)?;
    color_value(table, open, attribute.value, 0).or_else(|| {
        let raw = attribute.raw.as_deref()?.trim().strip_prefix('#')?;
        let argb = u32::from_str_radix(raw, 16).ok()?;
        Some(match raw.len() {
            6 => 0xff00_0000 | argb,
            8 => argb,
            _ => return None,
        })
    })
}

/// The colour with `alpha` (0 to 1) applied to its own.
fn with_alpha(argb: u32, alpha: f32) -> u32 {
    let a = ((argb >> 24) as f32 * alpha.clamp(0.0, 1.0)).round() as u32;
    (a << 24) | (argb & 0x00ff_ffff)
}

/// `outer` after `inner`: the transform that applies `inner`, then `outer`.
pub fn multiply(outer: Transform, inner: Transform) -> Transform {
    let [pa, pb, pc, pd, pe, pf] = outer;
    let [la, lb, lc, ld, le, lf] = inner;
    [
        pa * la + pc * lb,
        pb * la + pd * lb,
        pa * lc + pc * ld,
        pb * lc + pd * ld,
        pa * le + pc * lf + pe,
        pb * le + pd * lf + pf,
    ]
}

/// A group's own transform, as VectorDrawable builds it: about its pivot, scale, then
/// rotate, then translate.
fn group(table: &ResourceTable, element: &Element) -> Transform {
    let get = |name: &str, default: f32| number_of(table, element, 0, name).unwrap_or(default);
    let (px, py) = (get("pivotX", 0.0), get("pivotY", 0.0));
    let (sx, sy) = (get("scaleX", 1.0), get("scaleY", 1.0));
    let (tx, ty) = (get("translateX", 0.0), get("translateY", 0.0));
    let (sin, cos) = get("rotation", 0.0).to_radians().sin_cos();
    let (a, b, c, d) = (cos * sx, sin * sx, -sin * sy, cos * sy);
    [
        a,
        b,
        c,
        d,
        px + tx - (a * px + c * py),
        py + ty - (b * px + d * py),
    ]
}

fn path_data(table: &ResourceTable, document: &Document, element: &Element) -> Option<String> {
    let attribute = find(element, attr::PATH_DATA, "pathData")?;
    if let Some(raw) = &attribute.raw {
        return Some(raw.clone());
    }
    match attribute.value.kind {
        TYPE_STRING => document.pool.get(attribute.value.data).map(str::to_string),
        TYPE_REFERENCE => table.text(attribute.value.data),
        _ => None,
    }
}

/// Reads a compiled `<vector>` document; None when it is something else or draws nothing.
pub fn parse(table: &ResourceTable, open: &mut Open<'_>, document: &Document) -> Option<Vector> {
    let root = document.elements.first()?;
    if root.name != "vector" {
        return None;
    }
    let width = number_of(table, root, attr::VIEWPORT_WIDTH, "viewportWidth")?;
    let height = number_of(table, root, attr::VIEWPORT_HEIGHT, "viewportHeight")?;
    if !(width > 0.0 && height > 0.0) {
        return None;
    }
    let root_alpha = number_of(table, root, 0, "alpha").unwrap_or(1.0);
    // The groups the element being read is in, with their depths.
    let mut groups: Vec<(usize, Transform)> = vec![(root.depth, IDENTITY)];
    let mut paths = Vec::new();
    for element in &document.elements[1..] {
        while groups.len() > 1
            && groups
                .last()
                .is_some_and(|(depth, _)| *depth >= element.depth)
        {
            groups.pop();
        }
        let parent = groups.last().map_or(IDENTITY, |(_, t)| *t);
        match element.name.as_str() {
            "group" => groups.push((element.depth, multiply(parent, group(table, element)))),
            "path" => {
                let Some(data) = path_data(table, document, element) else {
                    continue;
                };
                let alpha =
                    |name: &str| number_of(table, element, 0, name).unwrap_or(1.0) * root_alpha;
                let fill = color_of(table, open, element, attr::FILL_COLOR, "fillColor")
                    .map(|c| with_alpha(c, alpha("fillAlpha")));
                let stroke = color_of(table, open, element, attr::STROKE_COLOR, "strokeColor")
                    .map(|c| with_alpha(c, alpha("strokeAlpha")));
                let stroke_width =
                    number_of(table, element, attr::STROKE_WIDTH, "strokeWidth").unwrap_or(0.0);
                let even_odd = find(element, 0, "fillType").is_some_and(|a| {
                    a.raw
                        .as_deref()
                        .map_or(a.value.data == 1, |raw| raw.eq_ignore_ascii_case("evenOdd"))
                });
                if fill.is_none() && (stroke.is_none() || stroke_width <= 0.0) {
                    continue;
                }
                paths.push(VectorPath {
                    data,
                    fill,
                    stroke,
                    stroke_width,
                    even_odd,
                    transform: parent,
                });
            }
            _ => {}
        }
    }
    (!paths.is_empty()).then_some(Vector {
        width,
        height,
        inset: 0.0,
        paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr11_a_group_turns_and_moves_its_paths_about_its_pivot() {
        let table = ResourceTable::default();
        let element = Element {
            name: "group".into(),
            depth: 1,
            attributes: vec![
                Attribute {
                    name: "rotation".into(),
                    resource_id: 0,
                    raw: Some("90".into()),
                    value: Value { kind: 0, data: 0 },
                },
                Attribute {
                    name: "pivotX".into(),
                    resource_id: 0,
                    raw: Some("10".into()),
                    value: Value { kind: 0, data: 0 },
                },
            ],
        };
        let [a, b, c, d, e, f] = group(&table, &element);
        // (20, 0) turns a quarter about (10, 0) to (10, 10).
        let (x, y) = (a * 20.0 + c * 0.0 + e, b * 20.0 + d * 0.0 + f);
        assert!(
            (x - 10.0).abs() < 1e-4 && (y - 10.0).abs() < 1e-4,
            "({x}, {y})"
        );
    }

    #[test]
    fn fr11_dimensions_and_fractions_read_as_numbers() {
        // 18 with radix 23p0, and 0.5 with radix 0p23.
        assert_eq!(complex(18 << 8), 18.0);
        assert!((complex((0x40_0000 << 8) | (3 << 4)) - 0.5).abs() < 1e-6);
    }
}
