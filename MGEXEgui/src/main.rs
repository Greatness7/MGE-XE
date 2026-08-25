#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

rust_i18n::i18n!("locales", fallback = "en");

mod app;
mod config;
mod distant;
mod generate;
mod input;
mod job;
mod localization;
mod morrowind_profile;
mod platform;
mod plugins;
mod precheck;
mod shaders;
mod style;
mod ui;

use std::{path::PathBuf, sync::Arc};

use app::GuiApp;
use eframe::egui::{IconData, ViewportBuilder};
use rust_i18n::t;

fn main() {
    localization::initialize();
    if let Err(error) = run() {
        eprintln!("{}", t!("startup.failed", error = &error));
        if std::env::var_os("MGEGUI_NO_DIALOG").is_none() {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title(t!("startup.dialog_title").as_ref())
                .set_description(&error)
                .show();
        }
    }
}

fn run() -> Result<(), String> {
    let (root, no_mutex) = arguments()?;
    platform::validate_root(&root).map_err(|error| format!("{error:#}"))?;
    if platform::morrowind_is_running() {
        return Err(t!("startup.morrowind_running").into_owned());
    }
    std::env::set_current_dir(&root)
        .map_err(|error| t!("startup.working_directory", path = root.display(), error = error).into_owned())?;

    let _instance = if no_mutex {
        None
    } else {
        match platform::SingleInstance::acquire().map_err(|error| format!("{error:#}"))? {
            Some(instance) => Some(instance),
            None => return Err(t!("startup.already_open").into_owned()),
        }
    };

    let mut viewport = ViewportBuilder::default()
        .with_inner_size([724.0, 524.0])
        .with_min_inner_size([700.0, 480.0])
        .with_resizable(false);

    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        t!("application.title").as_ref(),
        options,
        Box::new(move |creation| {
            GuiApp::new(creation, root.clone())
                .map(|app| Box::new(app) as Box<dyn eframe::App>)
                .map_err(|error| error.into())
        }),
    )
    .map_err(|error| t!("startup.could_not_start", error = error).into_owned())
}

fn arguments() -> Result<(PathBuf, bool), String> {
    let mut root = None;
    let mut no_mutex = false;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "-nomutex" || argument == "--no-mutex" {
            no_mutex = true;
        } else if argument == "--morrowind-root" {
            let value = arguments
                .next()
                .ok_or_else(|| t!("startup.root_argument_missing").into_owned())?;
            root = Some(PathBuf::from(value));
        } else {
            return Err(t!("startup.unknown_argument", argument = argument.to_string_lossy()).into_owned());
        }
    }
    let root = match root {
        Some(root) => root,
        None => std::env::current_exe()
            .map_err(|error| t!("startup.locate_executable", error = error).into_owned())?
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| t!("startup.executable_parent_missing").into_owned())?,
    };
    Ok((root, no_mutex))
}

pub(crate) fn load_icon() -> Option<Arc<IconData>> {
    let image_bytes: &[u8] = include_bytes!("../AppIcon.ico");
    let image = image::load_from_memory_with_format(image_bytes, image::ImageFormat::Ico)
        .ok()?
        .into_rgba8();

    let (width, height) = image.dimensions();
    let rgba = image.into_raw();

    Some(Arc::new(IconData { rgba, width, height }))
}
