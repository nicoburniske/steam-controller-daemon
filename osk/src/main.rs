mod keyboard;
mod render;
mod theme;
mod wayland;

use clap::Parser;
use evdev::KeyCode;
use keyboard::Keyboard;
use render::KeyboardRenderer;
use scd::{ControllerButton, Error, OskPad, OskState, Result, ResultExt};
use std::{
    env, fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::mpsc,
};
use theme::Theme;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "/run/scd/control.sock")]
    socket: PathBuf,
    #[arg(long)]
    font: Option<PathBuf>,
    #[arg(long)]
    theme: Option<PathBuf>,
    #[arg(long)]
    preview: Option<PathBuf>,
    #[arg(long, default_value_t = 1920)]
    width: u32,
    #[arg(long, default_value_t = 360)]
    height: u32,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    let user_theme = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|config| config.join("scd/osk.toml"));
    let theme = if let Some(path) = args.theme.as_deref() {
        Theme::load(path)?
    } else if let Some(path) = user_theme.filter(|path| path.exists()) {
        Theme::load(&path)?
    } else {
        Theme::default()
    };
    let font_path = args
        .font
        .as_deref()
        .or(theme.font.as_deref())
        .or_else(|| option_env!("SCD_OSK_FONT").map(Path::new))
        .ok_or_else(|| Error::message("pass --font or build with SCD_OSK_FONT"))?;
    let font = fs::read(font_path).whence()?.into_boxed_slice();

    if let Some(preview) = args.preview {
        let mut keyboard = Keyboard::default();
        let (keys, _) = mpsc::channel();
        let mut state = OskState::default();
        state.set_visible(true);
        state.set_bindings([
            (ControllerButton::L4, KeyCode::KEY_LEFTMETA),
            (ControllerButton::L5, KeyCode::KEY_LEFTSHIFT),
            (ControllerButton::R4, KeyCode::KEY_LEFTCTRL),
            (ControllerButton::R5, KeyCode::KEY_LEFTALT),
            (ControllerButton::LeftTriggerClick, KeyCode::KEY_LEFTSHIFT),
            (ControllerButton::RightTriggerClick, KeyCode::KEY_ENTER),
            (ControllerButton::X, KeyCode::KEY_BACKSPACE),
            (ControllerButton::Y, KeyCode::KEY_SPACE),
        ]);
        state.set_active_bindings([ControllerButton::R4, ControllerButton::X]);
        state.left = OskPad {
            touched: true,
            pressed: true,
            position: [-0.15, -0.1],
        };
        state.right = OskPad {
            touched: true,
            pressed: false,
            position: [0.25, 0.15],
        };
        keyboard.update(state, &keys);
        let mut renderer = KeyboardRenderer::new(font, theme, args.width, args.height, 1)?;
        renderer.render(&keyboard);
        let mut output = BufWriter::new(fs::File::create(preview).whence()?);
        write!(output, "P6\n{} {}\n255\n", args.width, args.height).whence()?;
        for pixel in renderer.pixels() {
            output
                .write_all(&[
                    (pixel.raw() >> 16) as u8,
                    (pixel.raw() >> 8) as u8,
                    pixel.raw() as u8,
                ])
                .whence()?;
        }
        output.flush().whence()?;
        return Ok(());
    }

    wayland::run(args.socket, font, theme)
}
