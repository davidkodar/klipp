// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Klipp (#3236): spellbook's unit tests that need a real en_US
//! dictionary run when `KLIPP_SPELL_EN_US` names a folder holding
//! `en_US.aff` and `en_US.dic`; without it they are ignored, saying why.
//! Klipp does not ship that dictionary.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(klipp_en_us)");
    println!("cargo::rerun-if-env-changed=KLIPP_SPELL_EN_US");
    if std::env::var_os("KLIPP_SPELL_EN_US").is_some() {
        println!("cargo::rustc-cfg=klipp_en_us");
    }
}
