//! OFX parameter value types — shared between klipp-core (events) and klipp-plugin (host).

/// A typed OFX parameter value for effect parameters.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum OfxValue {
    Double(f64),
    Double2(f64, f64),
    Double3(f64, f64, f64),
    Int(i32),
    Int2(i32, i32),
    Int3(i32, i32, i32),
    Bool(bool),
    Choice(i32),
    String(String),
    Rgba(f64, f64, f64, f64),
    Rgb(f64, f64, f64),
}

impl OfxValue {
    pub fn as_double(&self) -> Option<f64> {
        match self {
            Self::Double(v) => Some(*v),
            Self::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i32> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Bool(v) => Some(if *v { 1 } else { 0 }),
            Self::Choice(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            Self::Int(v) => Some(*v != 0),
            _ => None,
        }
    }
}
