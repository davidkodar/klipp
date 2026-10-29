//! Shared graph paths and operation shapes for the socket tools.

use serde_json::{json, Value};

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
}

pub(crate) fn path_schema(read_only: bool) -> Value {
    let id = json!({"type":"string", "format":"uuid"});
    let root = json!({"type":"object","description":"One root: {clip:uuid}|{timeline_grade:uuid}|{group_stage:uuid,stage:pre|post}|{track:uuid,part:grade|effects}."});
    let item = if read_only { id.clone() } else { json!({"oneOf":[id,
        object(json!({"new":{"type":"integer","minimum":0}}), &["new"])
    ]}) };
    object(json!({"root":root,"nodes":{"type":"array","maxItems":32,"items":item}}), &["root","nodes"])
}

pub(crate) fn ops_schema() -> Value {
    let path = json!({"$ref":"#/properties/ops/$defs/path"});
    let id = json!({"oneOf":[{"type":"string","format":"uuid"},object(json!({"new":{"type":"integer","minimum":0}}), &["new"])]});
    let text = json!({"type":"string"});
    // Per-op shapes are enforced by GraphWireOp before a socket call and by
    // the server. Sharing the typed fields leaves room in the tool catalog.
    let fields = json!({
        "op":{"enum":["add","add_beside","remove","connect","disconnect","publish","unpublish","set","enable","mix","mask_channel","move_after","modify","keys"]},
        "path":path,"at":path,"after":{"oneOf":[path,id]},"type":text,"params":{"type":"object"},"label":text,
        "from":path,"from_port":text,"to":path,"to_port":text,"port":text,"key":text,"value":{},
        "enabled":{"type":"boolean"},"mix":{"type":["number","null"],"minimum":0,"maximum":1},
        "channel":{"enum":["red","luminance","alpha",null]},"modifiers":{"type":"array"},"keys":{"type":"array"}
    });
    json!({"type":"array","minItems":1,"maxItems":64,"$defs":{"path":path_schema(false)},
        "description":"add(after,type,params?,label?); add_beside(at,..); connect(from,from_port,to,to_port); disconnect(to,port); publish(path,from,from_port,key,label?); on a path: remove, unpublish(key), set(key,value), enable, mix, mask_channel, move_after, modify(key,modifiers), keys.",
        "items":object(fields,&["op"])
    })
}
