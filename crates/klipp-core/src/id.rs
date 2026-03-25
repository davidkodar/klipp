use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

macro_rules! klipp_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn nil() -> Self {
                Self(Uuid::nil())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

klipp_id!(ClipId);
klipp_id!(TrackId);
klipp_id!(SequenceId);
klipp_id!(MediaId);
klipp_id!(MarkerId);
klipp_id!(TransitionId);
klipp_id!(GroupId);
klipp_id!(BinId);
