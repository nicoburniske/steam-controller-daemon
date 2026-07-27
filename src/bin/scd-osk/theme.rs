use blit::color::Color;
use scd::{Result, ResultExt};
use serde::{Deserialize, Deserializer};
use std::{fs, path::Path, path::PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub font: Option<PathBuf>,
    pub colors: ThemeColors,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ThemeColors {
    pub background: ThemeColor,
    pub border: ThemeColor,
    pub key: ThemeColor,
    pub special: ThemeColor,
    pub hover: ThemeColor,
    pub pressed: ThemeColor,
    pub pressed_foreground: ThemeColor,
    pub foreground: ThemeColor,
    pub muted: ThemeColor,
    pub dim: ThemeColor,
    pub hint_paddle: ThemeColor,
    pub hint_control: ThemeColor,
    pub shadow: ThemeColor,
}

#[derive(Debug, Clone, Copy)]
pub struct ThemeColor([u8; 4]);

impl Theme {
    pub fn load(path: &Path) -> Result<Self> {
        toml::from_str(&fs::read_to_string(path).whence()?).whence()
    }
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            background: ThemeColor::rgb(35, 38, 46),
            border: ThemeColor::rgb(77, 82, 88),
            key: ThemeColor::rgb(14, 20, 27),
            special: ThemeColor::rgb(0, 0, 0),
            hover: ThemeColor::rgb(255, 255, 255),
            pressed: ThemeColor::rgb(26, 159, 255),
            pressed_foreground: ThemeColor::rgb(255, 255, 255),
            foreground: ThemeColor::rgb(255, 255, 255),
            muted: ThemeColor::rgb(139, 146, 154),
            dim: ThemeColor::rgb(77, 82, 88),
            hint_paddle: ThemeColor::rgb(83, 91, 104),
            hint_control: ThemeColor::rgb(54, 60, 70),
            shadow: ThemeColor::rgb(0, 0, 0),
        }
    }
}

impl ThemeColor {
    const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self([red, green, blue, 255])
    }

    pub const fn color(self) -> Color {
        Color::from_rgba8(self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let value = value.strip_prefix('#').unwrap_or(&value);
        if !matches!(value.len(), 6 | 8) {
            return Err(serde::de::Error::custom(
                "color must contain six or eight hexadecimal digits",
            ));
        }
        let byte = |index| {
            u8::from_str_radix(&value[index..index + 2], 16).map_err(serde::de::Error::custom)
        };
        Ok(Self([
            byte(0)?,
            byte(2)?,
            byte(4)?,
            if value.len() == 8 { byte(6)? } else { 255 },
        ]))
    }
}
