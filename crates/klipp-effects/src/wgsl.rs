//! Just enough WGSL, in Rust, to run a shader body on the CPU as written:
//! `vec2/3/4<f32>` with their operators and swizzles, the built-ins the
//! effect shaders use, and WGSL's numeric conversions. Code transcribed
//! from a shader then reads like the shader, which is what keeps the two
//! the same computation.

#![allow(dead_code)]

use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct V2 {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct V4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl V4 {
    pub fn xyz(self) -> V3 {
        V3 { x: self.x, y: self.y, z: self.z }
    }
    pub fn rgb(self) -> V3 {
        self.xyz()
    }
}

/// `vec2<f32>(x, y)` / `vec2<f32>(s)`.
macro_rules! vec2 {
    ($s:expr) => {{ let s: f32 = $s; $crate::wgsl::V2 { x: s, y: s } }};
    ($x:expr, $y:expr $(,)?) => { $crate::wgsl::V2 { x: $x, y: $y } };
}

/// `vec3<f32>(x, y, z)` / `vec3<f32>(s)`.
macro_rules! vec3 {
    ($s:expr) => {{ let s: f32 = $s; $crate::wgsl::V3 { x: s, y: s, z: s } }};
    ($x:expr, $y:expr, $z:expr $(,)?) => { $crate::wgsl::V3 { x: $x, y: $y, z: $z } };
}

/// `vec4<f32>(x, y, z, w)`, `vec4<f32>(v3, w)` / `vec4<f32>(s)`.
macro_rules! vec4 {
    ($s:expr) => {{ let s: f32 = $s; $crate::wgsl::V4 { x: s, y: s, z: s, w: s } }};
    ($v:expr, $w:expr $(,)?) => {{ let v: $crate::wgsl::V3 = $v; $crate::wgsl::V4 { x: v.x, y: v.y, z: v.z, w: $w } }};
    ($x:expr, $y:expr, $z:expr, $w:expr $(,)?) => { $crate::wgsl::V4 { x: $x, y: $y, z: $z, w: $w } };
}

pub(crate) use {vec2, vec3, vec4};

macro_rules! vector_ops {
    ($t:ident, $($f:ident),+) => {
        impl Add for $t { type Output = $t; fn add(self, o: $t) -> $t { $t { $($f: self.$f + o.$f),+ } } }
        impl Sub for $t { type Output = $t; fn sub(self, o: $t) -> $t { $t { $($f: self.$f - o.$f),+ } } }
        impl Mul for $t { type Output = $t; fn mul(self, o: $t) -> $t { $t { $($f: self.$f * o.$f),+ } } }
        impl Div for $t { type Output = $t; fn div(self, o: $t) -> $t { $t { $($f: self.$f / o.$f),+ } } }
        impl Add<f32> for $t { type Output = $t; fn add(self, o: f32) -> $t { $t { $($f: self.$f + o),+ } } }
        impl Sub<f32> for $t { type Output = $t; fn sub(self, o: f32) -> $t { $t { $($f: self.$f - o),+ } } }
        impl Mul<f32> for $t { type Output = $t; fn mul(self, o: f32) -> $t { $t { $($f: self.$f * o),+ } } }
        impl Div<f32> for $t { type Output = $t; fn div(self, o: f32) -> $t { $t { $($f: self.$f / o),+ } } }
        impl Add<$t> for f32 { type Output = $t; fn add(self, o: $t) -> $t { $t { $($f: self + o.$f),+ } } }
        impl Sub<$t> for f32 { type Output = $t; fn sub(self, o: $t) -> $t { $t { $($f: self - o.$f),+ } } }
        impl Mul<$t> for f32 { type Output = $t; fn mul(self, o: $t) -> $t { $t { $($f: self * o.$f),+ } } }
        impl Div<$t> for f32 { type Output = $t; fn div(self, o: $t) -> $t { $t { $($f: self / o.$f),+ } } }
        impl Neg for $t { type Output = $t; fn neg(self) -> $t { $t { $($f: -self.$f),+ } } }
        impl AddAssign for $t { fn add_assign(&mut self, o: $t) { $(self.$f += o.$f;)+ } }
        impl SubAssign for $t { fn sub_assign(&mut self, o: $t) { $(self.$f -= o.$f;)+ } }
        impl MulAssign<f32> for $t { fn mul_assign(&mut self, o: f32) { $(self.$f *= o;)+ } }
        impl DivAssign<f32> for $t { fn div_assign(&mut self, o: f32) { $(self.$f /= o;)+ } }
        impl Num for $t {
            fn map(self, f: impl Fn(f32) -> f32) -> $t { $t { $($f: f(self.$f)),+ } }
            fn zip(self, o: $t, f: impl Fn(f32, f32) -> f32) -> $t { $t { $($f: f(self.$f, o.$f)),+ } }
        }
    };
}

/// Component-wise application, so one built-in serves scalars and vectors.
pub trait Num: Copy {
    fn map(self, f: impl Fn(f32) -> f32) -> Self;
    fn zip(self, o: Self, f: impl Fn(f32, f32) -> f32) -> Self;
}

impl Num for f32 {
    fn map(self, f: impl Fn(f32) -> f32) -> f32 {
        f(self)
    }
    fn zip(self, o: f32, f: impl Fn(f32, f32) -> f32) -> f32 {
        f(self, o)
    }
}

vector_ops!(V2, x, y);
vector_ops!(V3, x, y, z);
vector_ops!(V4, x, y, z, w);

/// `max` / `min` / `clamp` for floats, integers and float vectors.
pub trait Ord3: Copy {
    fn max_(self, o: Self) -> Self;
    fn min_(self, o: Self) -> Self;
}

impl Ord3 for i32 {
    fn max_(self, o: i32) -> i32 { self.max(o) }
    fn min_(self, o: i32) -> i32 { self.min(o) }
}

impl Ord3 for u32 {
    fn max_(self, o: u32) -> u32 { self.max(o) }
    fn min_(self, o: u32) -> u32 { self.min(o) }
}

impl<T: Num> Ord3 for T {
    fn max_(self, o: T) -> T { self.zip(o, f32::max) }
    fn min_(self, o: T) -> T { self.zip(o, f32::min) }
}

pub fn max<T: Ord3>(a: T, b: T) -> T { a.max_(b) }
pub fn min<T: Ord3>(a: T, b: T) -> T { a.min_(b) }
/// WGSL `clamp`: `min(max(v, lo), hi)`.
pub fn clamp<T: Ord3>(v: T, lo: T, hi: T) -> T { v.max_(lo).min_(hi) }

/// WGSL `mix`, the weight scalar.
pub fn mix<T: Num + Add<Output = T> + Sub<Output = T> + Mul<f32, Output = T>>(a: T, b: T, t: f32) -> T {
    a + (b - a) * t
}

pub fn abs<T: Num>(v: T) -> T { v.map(f32::abs) }
pub fn floor<T: Num>(v: T) -> T { v.map(f32::floor) }
pub fn ceil<T: Num>(v: T) -> T { v.map(f32::ceil) }
pub fn fract<T: Num>(v: T) -> T { v.map(|x| x - x.floor()) }
pub fn sqrt<T: Num>(v: T) -> T { v.map(f32::sqrt) }
pub fn sin<T: Num>(v: T) -> T { v.map(f32::sin) }
pub fn cos<T: Num>(v: T) -> T { v.map(f32::cos) }
pub fn exp<T: Num>(v: T) -> T { v.map(f32::exp) }
pub fn sign<T: Num>(v: T) -> T { v.map(|x| if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }) }
/// WGSL `round`: ties to even.
pub fn round<T: Num>(v: T) -> T { v.map(f32::round_ties_even) }
pub fn pow<T: Num>(a: T, b: T) -> T { a.zip(b, f32::powf) }
pub fn step<T: Num>(edge: T, x: T) -> T { edge.zip(x, |e, x| if x < e { 0.0 } else { 1.0 }) }
pub fn atan2(y: f32, x: f32) -> f32 { y.atan2(x) }
pub fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// WGSL `select(f, t, cond)`.
pub fn select<T>(f: T, t: T, cond: bool) -> T { if cond { t } else { f } }

pub fn dot3(a: V3, b: V3) -> f32 { a.x * b.x + a.y * b.y + a.z * b.z }
pub fn dot2(a: V2, b: V2) -> f32 { a.x * b.x + a.y * b.y }

pub trait Length: Copy {
    fn len(self) -> f32;
}
impl Length for V2 { fn len(self) -> f32 { (self.x * self.x + self.y * self.y).sqrt() } }
impl Length for V3 { fn len(self) -> f32 { (self.x * self.x + self.y * self.y + self.z * self.z).sqrt() } }
pub fn length<T: Length>(v: T) -> f32 { v.len() }
pub fn distance<T: Length + Sub<Output = T>>(a: T, b: T) -> f32 { (a - b).len() }

/// WGSL value conversions. Float → integer saturates, as Rust `as` does.
pub trait Conv: Copy {
    fn f(self) -> f32;
    fn u(self) -> u32;
    fn i(self) -> i32;
}
impl Conv for f32 {
    fn f(self) -> f32 { self }
    fn u(self) -> u32 { self as u32 }
    fn i(self) -> i32 { self as i32 }
}
impl Conv for u32 {
    fn f(self) -> f32 { self as f32 }
    fn u(self) -> u32 { self }
    fn i(self) -> i32 { self as i32 }
}
impl Conv for i32 {
    fn f(self) -> f32 { self as f32 }
    fn u(self) -> u32 { self as u32 }
    fn i(self) -> i32 { self }
}
pub fn f32c<T: Conv>(v: T) -> f32 { v.f() }
pub fn u32c<T: Conv>(v: T) -> u32 { v.u() }
pub fn i32c<T: Conv>(v: T) -> i32 { v.i() }
