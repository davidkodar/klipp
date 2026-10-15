//! Synthetic glowing path used by rendering parity and performance checks.
use klipp_timeline::*;

pub fn ring() -> TitleData {
    let points = [([0.86,0.5],[0.86,0.3012],[0.86,0.6988]),
        ([0.5,0.86],[0.6988,0.86],[0.3012,0.86]),
        ([0.14,0.5],[0.14,0.6988],[0.14,0.3012]),
        ([0.5,0.14],[0.3012,0.14],[0.6988,0.14])];
    let vertices: Vec<_> = points.into_iter().map(|(point,incoming,outgoing)| serde_json::json!({"point":point,"incoming":incoming,"outgoing":outgoing})).collect();
    let path = serde_json::from_value(serde_json::json!({
        "pose":{"contours":[{"closed":true,"vertices":vertices}],"fill":[0,0,0,0],
            "stroke":[0.5,0.8,0.7,0.65],"stroke_width":2,
            "stroke_glow":{"radius":8,"strength":0.25}},
        "trim":{"range":{"start":0,"end":1,"offset":0},"keyframes":[
            {"time_secs":0,"range":{"start":0,"end":0,"offset":0},"easing":"Linear"},
            {"time_secs":2,"range":{"start":0,"end":1,"offset":0},"easing":"OutCubic"},
            {"time_secs":7,"range":{"start":0,"end":1,"offset":0.2},"easing":"Linear"}]}
    })).unwrap();
    let mut title = TitleData::default();
    title.objects.clear();
    title.add_object(TitleObjectContent::Shape(Shape {kind:ShapeKind::Path(path), fill:None, gradient:None, stroke:None, fit:None}), ObjectTransform {
        size: klipp_timeline::TitlePosition {x:1.0,y:1.0}, ..klipp_timeline::default_text_transform()
    });
    title
}
