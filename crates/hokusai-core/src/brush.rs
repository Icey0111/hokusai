//! The [`Brush`] value: a full configuration consumed by the stroke engine.

use std::collections::BTreeMap;

use crate::input::BrushInput;
use crate::mapping::SettingValue;
use crate::setting::{BrushSetting, NUM_SETTINGS};

/// A setting hokusai doesn't (yet) recognise. Held as raw JSON so a parsed
/// brush can be re-serialised without losing data — important for working
/// with brush packs authored against newer libmypaint than this crate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UnknownSetting {
    pub base_value: f32,
    /// `(input_name, points)` — input names are kept as strings so we can
    /// passthrough inputs hokusai doesn't recognise either.
    pub inputs: BTreeMap<String, Vec<(f32, f32)>>,
}

#[derive(Debug, Clone)]
pub struct Brush {
    pub version: u32,
    pub group: Option<String>,
    pub parent_brush_name: Option<String>,
    pub comment: Option<String>,
    settings: Vec<SettingValue>,
    /// Settings hokusai didn't recognise on parse, kept by string key for
    /// lossless round-trip via `hokusai-brush`.
    pub unknown_settings: BTreeMap<String, UnknownSetting>,
}

impl Brush {
    pub fn new() -> Self {
        Self {
            version: 3,
            group: None,
            parent_brush_name: None,
            comment: None,
            settings: vec![SettingValue::default(); NUM_SETTINGS],
            unknown_settings: BTreeMap::new(),
        }
    }

    #[inline]
    pub fn get(&self, s: BrushSetting) -> &SettingValue {
        &self.settings[s.index()]
    }

    #[inline]
    pub fn get_mut(&mut self, s: BrushSetting) -> &mut SettingValue {
        &mut self.settings[s.index()]
    }

    pub fn set(&mut self, s: BrushSetting, v: SettingValue) {
        self.settings[s.index()] = v;
    }

    pub fn settings(&self) -> &[SettingValue] {
        &self.settings
    }

    // ---- libmypaint introspection API ---------------------------------
    //
    // Mirrors mypaint-brush.h:
    //   mypaint_brush_get/set_base_value
    //   mypaint_brush_is_constant
    //   mypaint_brush_get_inputs_used_n
    //   mypaint_brush_get/set_mapping_n
    //   mypaint_brush_get/set_mapping_point
    //
    // The C API returns void / asserts on bad indices; the Rust methods
    // return `bool` for mutating calls so callers can reject bad input
    // without aborting the process.

    #[inline]
    pub fn get_base_value(&self, setting: BrushSetting) -> f32 {
        self.get(setting).base_value
    }

    #[inline]
    pub fn set_base_value(&mut self, setting: BrushSetting, value: f32) {
        self.get_mut(setting).base_value = value;
    }

    /// `mypaint_brush_is_constant`: true when the setting uses no inputs.
    pub fn is_constant(&self, setting: BrushSetting) -> bool {
        self.get_inputs_used_n(setting) == 0
    }

    /// `mypaint_brush_get_inputs_used_n`: number of non-empty mappings.
    pub fn get_inputs_used_n(&self, setting: BrushSetting) -> usize {
        self.get(setting)
            .inputs
            .iter()
            .filter(|m| !m.points.is_empty())
            .count()
    }

    /// `mypaint_brush_get_mapping_n` for one input.
    pub fn get_mapping_n(&self, setting: BrushSetting, input: BrushInput) -> usize {
        self.get(setting)
            .inputs
            .iter()
            .find(|m| m.input == input)
            .map_or(0, |m| m.points.len())
    }

    /// `mypaint_brush_set_mapping_n`: resize an input's control points.
    ///
    /// Upstream asserts `0 <= n <= 64` and `n != 1`; violating either
    /// returns `false` here. Newly created points are `(0.0, 0.0)`.
    pub fn set_mapping_n(&mut self, setting: BrushSetting, input: BrushInput, n: usize) -> bool {
        if n > 64 || n == 1 {
            return false;
        }
        let sv = self.get_mut(setting);
        if n == 0 {
            sv.inputs.retain(|m| m.input != input);
            return true;
        }
        if let Some(m) = sv.inputs.iter_mut().find(|m| m.input == input) {
            m.points.resize(n, (0.0, 0.0));
            return true;
        }
        sv.inputs.push(crate::mapping::InputMapping {
            input,
            points: vec![(0.0, 0.0); n],
        });
        true
    }

    /// `mypaint_brush_get_mapping_point`; `None` if out of range.
    pub fn get_mapping_point(
        &self,
        setting: BrushSetting,
        input: BrushInput,
        index: usize,
    ) -> Option<(f32, f32)> {
        self.get(setting)
            .inputs
            .iter()
            .find(|m| m.input == input)
            .and_then(|m| m.points.get(index).copied())
    }

    /// `mypaint_brush_set_mapping_point`; returns `false` for an
    /// out-of-range index or an x value that would break the required
    /// ascending order.
    pub fn set_mapping_point(
        &mut self,
        setting: BrushSetting,
        input: BrushInput,
        index: usize,
        x: f32,
        y: f32,
    ) -> bool {
        let sv = self.get_mut(setting);
        let Some(m) = sv.inputs.iter_mut().find(|m| m.input == input) else {
            return false;
        };
        if index >= m.points.len() {
            return false;
        }
        if index > 0 && x < m.points[index - 1].0 {
            return false;
        }
        m.points[index] = (x, y);
        true
    }
}

impl Default for Brush {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::BrushInput;

    #[test]
    fn base_value_and_constant_introspection() {
        let mut b = Brush::new();
        assert!(b.is_constant(BrushSetting::Radius));
        assert_eq!(b.get_base_value(BrushSetting::Radius), 0.0);
        b.set_base_value(BrushSetting::Radius, 2.5);
        assert_eq!(b.get_base_value(BrushSetting::Radius), 2.5);
        assert_eq!(b.get_inputs_used_n(BrushSetting::Radius), 0);
    }

    #[test]
    fn mapping_api_roundtrip_and_guards() {
        let mut b = Brush::new();
        assert!(!b.set_mapping_n(BrushSetting::Radius, BrushInput::Pressure, 1));
        assert!(!b.set_mapping_n(BrushSetting::Radius, BrushInput::Pressure, 65));
        assert!(b.set_mapping_n(BrushSetting::Radius, BrushInput::Pressure, 2));
        assert_eq!(
            b.get_mapping_n(BrushSetting::Radius, BrushInput::Pressure),
            2
        );
        assert!(!b.is_constant(BrushSetting::Radius));
        assert_eq!(b.get_inputs_used_n(BrushSetting::Radius), 1);

        assert!(b.set_mapping_point(BrushSetting::Radius, BrushInput::Pressure, 0, 0.0, 0.0));
        assert!(b.set_mapping_point(BrushSetting::Radius, BrushInput::Pressure, 1, 1.0, 1.0));
        assert_eq!(
            b.get_mapping_point(BrushSetting::Radius, BrushInput::Pressure, 1),
            Some((1.0, 1.0))
        );
        // x must stay ascending
        assert!(!b.set_mapping_point(BrushSetting::Radius, BrushInput::Pressure, 1, -1.0, 1.0));
        assert_eq!(
            b.get_mapping_point(BrushSetting::Radius, BrushInput::Pressure, 1),
            Some((1.0, 1.0))
        );

        assert!(b.set_mapping_n(BrushSetting::Radius, BrushInput::Pressure, 0));
        assert!(b.is_constant(BrushSetting::Radius));
        assert_eq!(
            b.get_mapping_n(BrushSetting::Radius, BrushInput::Pressure),
            0
        );
    }
}
