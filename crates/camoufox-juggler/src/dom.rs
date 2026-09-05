//! DOM helpers: element geometry, node description and adoption.
//!
//! Typed surface over `Page.getContentQuads`, `Page.describeNode`,
//! `Page.scrollIntoViewIfNeeded` and `Page.adoptNode`. Element object ids
//! come from [`crate::page::JugglerPage::query_object_id`]
//! (`document.querySelector` with `returnByValue: false`).

use serde_json::Value;

use crate::error::{JugglerError, Result};

/// A 2D point.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Point {
    /// X coordinate (CSS pixels).
    pub x: f64,
    /// Y coordinate (CSS pixels).
    pub y: f64,
}

/// A content quad: four points describing an element's layout box
/// (possibly transformed/rotated).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Quad {
    /// First corner.
    pub p1: Point,
    /// Second corner.
    pub p2: Point,
    /// Third corner.
    pub p3: Point,
    /// Fourth corner.
    pub p4: Point,
}

impl Quad {
    /// Axis-aligned bounding box as `(x, y, width, height)`.
    pub fn bounding_box(&self) -> (f64, f64, f64, f64) {
        let xs = [self.p1.x, self.p2.x, self.p3.x, self.p4.x];
        let ys = [self.p1.y, self.p2.y, self.p3.y, self.p4.y];
        let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let min_y = ys.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_y = ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (min_x, min_y, max_x - min_x, max_y - min_y)
    }

    /// The element's center point (useful for click targeting).
    pub fn center(&self) -> Point {
        let (x, y, width, height) = self.bounding_box();
        Point {
            x: x + width / 2.0,
            y: y + height / 2.0,
        }
    }
}

/// `Page.describeNode` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct NodeDescription {
    /// Frame id when the node is an iframe document.
    pub content_frame_id: Option<String>,
    /// Frame the node belongs to.
    pub owner_frame_id: Option<String>,
}

/// Decodes `Page.getContentQuads` results.
pub(crate) fn decode_quads(result: &Value) -> Result<Vec<Quad>> {
    let array = result
        .get("quads")
        .and_then(Value::as_array)
        .ok_or_else(|| JugglerError::Protocol("getContentQuads without quads".into()))?;
    let mut quads = Vec::with_capacity(array.len());
    for quad in array {
        let p1 = decode_point(quad.get("p1"));
        let p2 = decode_point(quad.get("p2"));
        let p3 = decode_point(quad.get("p3"));
        let p4 = decode_point(quad.get("p4"));
        match (p1, p2, p3, p4) {
            (Some(p1), Some(p2), Some(p3), Some(p4)) => quads.push(Quad { p1, p2, p3, p4 }),
            _ => {
                return Err(JugglerError::Protocol(
                    "getContentQuads returned a malformed quad".into(),
                ))
            }
        }
    }
    Ok(quads)
}

fn decode_point(value: Option<&Value>) -> Option<Point> {
    let value = value?;
    Some(Point {
        x: value.get("x").and_then(Value::as_f64)?,
        y: value.get("y").and_then(Value::as_f64)?,
    })
}

/// Decodes `Page.describeNode` results.
pub(crate) fn decode_node_description(result: &Value) -> NodeDescription {
    let field = |name: &str| {
        result
            .get(name)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    };
    NodeDescription {
        content_frame_id: field("contentFrameId"),
        owner_frame_id: field("ownerFrameId"),
    }
}

/// Extracts a remote object id from a `Runtime.evaluate` result
/// (`returnByValue: false`).
pub(crate) fn object_id_of(result: &Value) -> Result<Option<String>> {
    if let Some(details) = result.get("exceptionDetails") {
        let text = details
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("exception");
        return Err(JugglerError::Protocol(format!("evaluation failed: {text}")));
    }
    Ok(result
        .pointer("/result/objectId")
        .and_then(Value::as_str)
        .map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_quads() {
        let result = json!({"quads": [
            {"p1": {"x": 0.0, "y": 0.0}, "p2": {"x": 10.0, "y": 0.0},
             "p3": {"x": 10.0, "y": 20.0}, "p4": {"x": 0.0, "y": 20.0}},
        ]});
        let quads = decode_quads(&result).unwrap();
        assert_eq!(quads.len(), 1);
        let (x, y, width, height) = quads[0].bounding_box();
        assert_eq!((x, y, width, height), (0.0, 0.0, 10.0, 20.0));
        let center = quads[0].center();
        assert_eq!((center.x, center.y), (5.0, 10.0));
    }

    #[test]
    fn rejects_malformed_quads() {
        assert!(decode_quads(&json!({})).is_err());
        assert!(decode_quads(&json!({"quads": [{"p1": {"x": 1.0}}]})).is_err());
    }

    #[test]
    fn decodes_node_descriptions() {
        let description = decode_node_description(&json!({
            "contentFrameId": "frame-2",
            "ownerFrameId": "frame-1",
        }));
        assert_eq!(description.content_frame_id.as_deref(), Some("frame-2"));
        assert_eq!(description.owner_frame_id.as_deref(), Some("frame-1"));

        // Absent/empty fields stay None.
        let description = decode_node_description(&json!({"ownerFrameId": ""}));
        assert!(description.content_frame_id.is_none());
        assert!(description.owner_frame_id.is_none());
    }

    #[test]
    fn extracts_object_ids() {
        assert_eq!(
            object_id_of(
                &json!({"result": {"type": "object", "subtype": "node", "objectId": "obj-1"}})
            )
            .unwrap(),
            Some("obj-1".to_string())
        );
        // No objectId (e.g. null result).
        assert_eq!(
            object_id_of(&json!({"result": {"type": "undefined"}})).unwrap(),
            None
        );
        // Exceptions surface as errors.
        assert!(object_id_of(&json!({"exceptionDetails": {"text": "boom"}})).is_err());
    }
}
