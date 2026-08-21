use blit_gui::color::Color;
use scd::{
    Error,
    kdl::{FromKdl, FromKdlNode, Issue, KdlDocument, KdlEntry, KdlNode, issue, nodes, props},
};
use std::{fs, path::Path, path::PathBuf};

#[derive(Debug)]
pub struct Theme {
    pub font: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub colors: ThemeColors,
}

#[derive(Debug)]
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
    pub fn load(path: &Path) -> scd::Result<Self> {
        let source = fs::read_to_string(path)?;
        Self::parse(&source).map_err(|error| Error::message(format!("{}: {error}", path.display())))
    }

    fn parse(source: &str) -> scd::Result<Self> {
        let document = KdlDocument::parse(source)?;
        let (font, width, height, colors) = nodes!(
            &document,
            optional "font",
            "width" = 1280,
            "height" = 360,
            "colors" = ThemeColors::default(),
        )?;
        Ok(Self {
            font,
            width,
            height,
            colors,
        })
    }
}

impl FromKdlNode<'_> for ThemeColors {
    fn from_kdl_node(node: &KdlNode) -> std::result::Result<Self, Issue> {
        let default = ThemeColors::default();
        let (
            background,
            border,
            key,
            special,
            hover,
            pressed,
            pressed_foreground,
            foreground,
            muted,
            dim,
            hint_paddle,
            hint_control,
            shadow,
        ) = props!(
            node,
            "background" = default.background,
            "border" = default.border,
            "key" = default.key,
            "special" = default.special,
            "hover" = default.hover,
            "pressed" = default.pressed,
            "pressed-foreground" = default.pressed_foreground,
            "foreground" = default.foreground,
            "muted" = default.muted,
            "dim" = default.dim,
            "hint-paddle" = default.hint_paddle,
            "hint-control" = default.hint_control,
            "shadow" = default.shadow,
        )?;
        Ok(Self {
            background,
            border,
            key,
            special,
            hover,
            pressed,
            pressed_foreground,
            foreground,
            muted,
            dim,
            hint_paddle,
            hint_control,
            shadow,
        })
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            font: None,
            width: 1280,
            height: 360,
            colors: ThemeColors::default(),
        }
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
        Color::rgba(self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

impl FromKdl<'_> for ThemeColor {
    fn from_kdl(entry: &KdlEntry, field: &str) -> std::result::Result<Self, Issue> {
        let value = entry
            .value()
            .as_string()
            .ok_or_else(|| issue(entry, format!("{field} must be a string")))?;
        let value = value.strip_prefix('#').unwrap_or(value);
        if !matches!(value.len(), 6 | 8) {
            return Err(issue(
                entry,
                "color must contain six or eight hexadecimal digits",
            ));
        }
        let byte = |index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| issue(entry, "color must contain only hexadecimal digits"))
        };
        Ok(Self([
            byte(0)?,
            byte(2)?,
            byte(4)?,
            if value.len() == 8 { byte(6)? } else { 255 },
        ]))
    }
}
