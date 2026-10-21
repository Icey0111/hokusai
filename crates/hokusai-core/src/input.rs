//! Brush inputs — drivers fed by the application each `stroke_to` call.
//!
//! String names match libmypaint's `brushsettings.json` `inputs` keys, so
//! `.myb` files round-trip without translation.

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum BrushInput {
    #[default]
    Pressure,
    Speed1,
    Speed2,
    Random,
    Stroke,
    Direction,
    Tilt,
    TiltDeclination,
    TiltAscension,
    DirectionAngle,
    AttackAngle,
    GridmapX,
    GridmapY,
    Custom,
    TiltDeclinationX,
    TiltDeclinationY,
    Viewzoom,
    BarrelRotation,
    BrushRadius,
}

impl BrushInput {
    pub const ALL: &'static [BrushInput] = &[
        Self::Pressure,
        Self::Speed1,
        Self::Speed2,
        Self::Random,
        Self::Stroke,
        Self::Direction,
        Self::Tilt,
        Self::TiltDeclination,
        Self::TiltAscension,
        Self::DirectionAngle,
        Self::AttackAngle,
        Self::GridmapX,
        Self::GridmapY,
        Self::Custom,
        Self::TiltDeclinationX,
        Self::TiltDeclinationY,
        Self::Viewzoom,
        Self::BarrelRotation,
        Self::BrushRadius,
    ];

    pub const fn cname(self) -> &'static str {
        match self {
            Self::Pressure => "pressure",
            Self::Speed1 => "speed1",
            Self::Speed2 => "speed2",
            Self::Random => "random",
            Self::Stroke => "stroke",
            Self::Direction => "direction",
            Self::Tilt => "tilt",
            Self::TiltDeclination => "tilt_declination",
            Self::TiltAscension => "tilt_ascension",
            Self::DirectionAngle => "direction_angle",
            Self::AttackAngle => "attack_angle",
            Self::GridmapX => "gridmap_x",
            Self::GridmapY => "gridmap_y",
            Self::Custom => "custom",
            Self::TiltDeclinationX => "tilt_declinationx",
            Self::TiltDeclinationY => "tilt_declinationy",
            Self::Viewzoom => "viewzoom",
            Self::BarrelRotation => "barrel_rotation",
            Self::BrushRadius => "brush_radius",
        }
    }

    pub fn from_cname(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|i| i.cname() == s)
    }

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Position in libmypaint's `MyPaintBrushInput` enum.
    ///
    /// The local enum is ordered for readability, but upstream's order is
    /// observable: `mypaint_mapping_calculate` sums a setting's mappings in
    /// enum order, and f32 addition is not associative.
    pub const fn libmypaint_index(self) -> usize {
        match self {
            Self::Pressure => 0,
            Self::Random => 1,
            Self::Stroke => 2,
            Self::Direction => 3,
            Self::TiltDeclination => 4,
            Self::TiltAscension => 5,
            Self::Speed1 => 6,
            Self::Speed2 => 7,
            Self::Custom => 8,
            Self::DirectionAngle => 9,
            Self::AttackAngle => 10,
            Self::TiltDeclinationX => 11,
            Self::TiltDeclinationY => 12,
            Self::GridmapX => 13,
            Self::GridmapY => 14,
            Self::Viewzoom => 15,
            Self::BrushRadius => 16,
            Self::BarrelRotation => 17,
            // Not a libmypaint input; sort after the 18 upstream slots.
            Self::Tilt => 18,
        }
    }
}

pub const NUM_INPUTS: usize = BrushInput::ALL.len();
