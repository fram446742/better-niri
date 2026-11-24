//! Orientation values and their niri transform mapping.
//!
//! The six iio-sensor-proxy orientations map to niri's counter-clockwise
//! transforms. The mapping is fixed and documented; users who need a different
//! convention can send a raw transform with `rotate.set`.

use std::fmt;
use std::str::FromStr;

use niri_ipc::Transform;
use serde::{Deserialize, Serialize};

/// Accelerometer orientation as reported by iio-sensor-proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Orientation {
    Normal,
    LeftUp,
    RightUp,
    BottomUp,
    LeftDown,
    RightDown,
}

impl Orientation {
    /// Niri transform that keeps content upright for this orientation.
    pub fn transform(self) -> Transform {
        match self {
            Orientation::Normal => Transform::Normal,
            Orientation::LeftUp => Transform::_90,
            Orientation::RightUp => Transform::_270,
            Orientation::BottomUp => Transform::_180,
            Orientation::LeftDown => Transform::Flipped90,
            Orientation::RightDown => Transform::Flipped270,
        }
    }

    /// Orientation matching a transform, when the transform is a pure rotation.
    pub fn from_transform(transform: Transform) -> Option<Self> {
        match transform {
            Transform::Normal => Some(Orientation::Normal),
            Transform::_90 => Some(Orientation::LeftUp),
            Transform::_180 => Some(Orientation::BottomUp),
            Transform::_270 => Some(Orientation::RightUp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Orientation::Normal => "normal",
            Orientation::LeftUp => "left-up",
            Orientation::RightUp => "right-up",
            Orientation::BottomUp => "bottom-up",
            Orientation::LeftDown => "left-down",
            Orientation::RightDown => "right-down",
        }
    }

    /// Parses sensor output or transform aliases.
    ///
    /// Accepts the six iio names (case-insensitive, `_` or `-`), `normal`,
    /// `leftup`-style aliases and the rotation names `90`, `180`, `270`.
    pub fn parse(value: &str) -> Result<Self, String> {
        let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "normal" | "0" => Ok(Orientation::Normal),
            "left-up" | "leftup" | "90" => Ok(Orientation::LeftUp),
            "bottom-up" | "bottomup" | "upside-down" | "180" => Ok(Orientation::BottomUp),
            "right-up" | "rightup" | "270" => Ok(Orientation::RightUp),
            "left-down" | "leftdown" => Ok(Orientation::LeftDown),
            "right-down" | "rightdown" => Ok(Orientation::RightDown),
            other => Err(format!(
                "unknown orientation `{other}`; expected normal, left-up, right-up, bottom-up, left-down or right-down"
            )),
        }
    }
}

impl fmt::Display for Orientation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Orientation {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_orientation_to_a_transform() {
        assert_eq!(Orientation::Normal.transform(), Transform::Normal);
        assert_eq!(Orientation::LeftUp.transform(), Transform::_90);
        assert_eq!(Orientation::BottomUp.transform(), Transform::_180);
        assert_eq!(Orientation::RightUp.transform(), Transform::_270);
        assert_eq!(Orientation::LeftDown.transform(), Transform::Flipped90);
        assert_eq!(Orientation::RightDown.transform(), Transform::Flipped270);
    }

    #[test]
    fn round_trips_rotation_transforms() {
        for orientation in [
            Orientation::Normal,
            Orientation::LeftUp,
            Orientation::BottomUp,
            Orientation::RightUp,
        ] {
            assert_eq!(
                Orientation::from_transform(orientation.transform()),
                Some(orientation)
            );
        }
        assert_eq!(Orientation::from_transform(Transform::Flipped), None);
    }

    #[test]
    fn parses_sensor_and_transform_spellings() {
        assert_eq!(Orientation::parse("normal"), Ok(Orientation::Normal));
        assert_eq!(Orientation::parse("LEFT_UP"), Ok(Orientation::LeftUp));
        assert_eq!(Orientation::parse(" right-up\n"), Ok(Orientation::RightUp));
        assert_eq!(Orientation::parse("bottomup"), Ok(Orientation::BottomUp));
        assert_eq!(Orientation::parse("90"), Ok(Orientation::LeftUp));
        assert_eq!(Orientation::parse("180"), Ok(Orientation::BottomUp));
        assert_eq!(Orientation::parse("270"), Ok(Orientation::RightUp));
        assert!(Orientation::parse("sideways").is_err());
    }
}
