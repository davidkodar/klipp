// Frozen value types from the v1 reader at 7d309e9f.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LogFormat {
    #[default]
    Srgb,
    Linear,
    LogC3,
    LogC4,
    SLog2,
    SLog3,
    CanonLog,
    CanonLog3,
    VLog,
    NLog,
    FLog,
    DJIDLog,
    GoProFlat,
    AppleLog,
    LeicaLLog,
    RedLog3G10,
    BmdFilmGen5,
    HLG,
    PQ,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum WorkingColorspace {
    #[default]
    Rec709Linear,
    AcesCG,
    Bt2020Linear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GradeEncoding {
    #[default]
    Bt1886,
    AcesCct,
    AcesCc,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Frei0rValue {
    Bool(bool),
    Double(f64),
    Color([f32; 3]),
    Position([f64; 2]),
    String(String),
}

pub mod assets;
pub mod color_grade;
pub mod effects;
pub mod graph;
pub mod keyframe;
pub mod kinds;
pub mod modifier;
pub mod ofx;
pub mod params;
pub use color_grade::*;
pub use effects::*;
pub use graph::*;
pub use keyframe::*;
pub use kinds::*;
pub use modifier::*;
pub use ofx::*;
pub use params::*;

pub mod stores;
pub use stores::*;

pub mod json_check;
