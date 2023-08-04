#![feature(iterator_try_collect)]
#![feature(result_option_inspect)]
#![allow(dead_code)]

use anyhow::Context;
use cache::EntryCache;
use clap::{Parser, Args, Subcommand};
use cs2::{CS2Handle, Module, CS2Offsets, EntitySystem, CS2Model, BoneFlags, Globals};
use imgui::{Condition, ImColor32};
use obfstr::obfstr;
use settings::{AppSettings, load_app_settings};
use view::ViewController;
use visuals::{BombState, PlayerInfo};
use windows::Win32::System::Console::GetConsoleProcessList;
use std::{
    cell::{RefCell, RefMut},
    fmt::Debug, sync::Arc, rc::Rc, io::BufWriter, fs::File, path::PathBuf,
};

use crate::settings::save_app_settings;

mod view;
mod settings;
mod cache;
mod visuals;

pub struct Application {
    pub cs2: Arc<CS2Handle>,
    pub cs2_offsets: Arc<CS2Offsets>,
    pub cs2_entities: EntitySystem,
    pub cs2_globals: Globals,

    pub settings_visible: bool,
    pub model_cache: EntryCache<u64, CS2Model>,
    pub class_name_cache: EntryCache<u64, Option<String>>,

    pub players: Vec<PlayerInfo>,
    pub view_controller: ViewController,

    pub bomb_state: BombState,
    
    pub frame_read_calls: usize,
    pub last_total_read_calls: usize,

    pub settings: RefCell<AppSettings>,
    pub settings_dirty: bool,
}

impl Application {
    pub fn settings(&self) -> std::cell::Ref<'_, AppSettings> {
        self.settings.borrow()
    }

    pub fn settings_mut(&self) -> RefMut<'_, AppSettings> {
        self.settings.borrow_mut()
    }

    pub fn pre_update(&mut self, context: &mut imgui::Context) -> anyhow::Result<()> {
        if self.settings_dirty {
            self.settings_dirty = false;
            let mut settings = self.settings.borrow_mut();

            let mut imgui_settings = String::new();
            context.save_ini_settings(&mut imgui_settings);
            settings.imgui = Some(imgui_settings);

            if let Err(error) = save_app_settings(&*settings) {
                log::warn!("Failed to save user settings: {}", error);
            };
        }
        Ok(())
    }

    pub fn update(&mut self, ui: &imgui::Ui) -> anyhow::Result<()> {
        if ui.is_key_pressed_no_repeat(imgui::Key::Keypad0) {
            log::debug!("Toogle settings");
            self.settings_visible = !self.settings_visible;
            
            if !self.settings_visible {
                /* overlay has just been closed */
                self.settings_dirty = true;
            }
        }

        self.view_controller.update_screen_bounds(mint::Vector2::from_slice(&ui.io().display_size));
        self.view_controller
            .update_view_matrix(&self.cs2)?;


        self.cs2_globals = self.cs2.read::<Globals>(Module::Client, &[ self.cs2_offsets.globals, 0 ])
            .with_context(|| obfstr!("failed to read globals").to_string())?;
       
        visuals::read_player_info(self)?;
        
        if self.settings().bomb_timer {
            self.bomb_state = visuals::read_bomb_state(self)?;
        }

        let read_calls = self.cs2.ke_interface.total_read_calls();
        self.frame_read_calls = read_calls - self.last_total_read_calls;
        self.last_total_read_calls = read_calls;

        Ok(())
    }

    pub fn render(&self, ui: &imgui::Ui) {
        ui.window("overlay")
            .draw_background(false)
            .no_decoration()
            .no_inputs()
            .size(ui.io().display_size, Condition::Always)
            .position([0.0, 0.0], Condition::Always)
            .build(|| self.render_overlay(ui));

        if self.settings_visible {
            self.render_settings(ui);
        }
    }

    fn render_overlay(&self, ui: &imgui::Ui) {
        let settings = self.settings.borrow();

        {
            let text_buf;
            let text = obfstr!(text_buf = "Valthrun Overlay");
            
            ui.set_cursor_pos([
                ui.window_size()[0] - ui.calc_text_size(text)[0] - 10.0,
                10.0,
            ]);
            ui.text(text);
        }
        {
            let text = format!("{:.2} FPS", ui.io().framerate);
            ui.set_cursor_pos([
                ui.window_size()[0] - ui.calc_text_size(&text)[0] - 10.0,
                24.0,
            ]);
            ui.text(text)
        }
        {
            let text = format!("{} Reads", self.frame_read_calls);
            ui.set_cursor_pos([
                ui.window_size()[0] - ui.calc_text_size(&text)[0] - 10.0,
                38.0,
            ]);
            ui.text(text)
        }

        if settings.player_list {
            ui.set_cursor_pos([10.0, 300.0]);
            ui.text(format!("{} players alive", self.players.len()));
            for entry in self.players.iter() {
                ui.text(format!(
                    "{} ({}) | {:?}",
                    entry.player_name, entry.player_health, entry.position
                ));
            }
        }

        let draw = ui.get_window_draw_list();
        for entry in self.players.iter() {
            if entry.local {
                continue;
            }

            let position = entry.position;

            if settings.player_pos_dot {
                if let Some(mut screen_position) =
                    self.view_controller.world_to_screen(&position, false)
                {
                    draw.add_circle(screen_position, 8.0, ImColor32::from_rgb(255, 0, 0))
                        .filled(true)
                        .build();

                    screen_position.y -= 10.0;
                    draw.add_text(
                        screen_position,
                        ImColor32::from_rgb(0, 255, 0),
                        &entry.debug_text,
                    );
                }
            }

            if settings.esp_skeleton {
                let bones = entry.model.bones.iter()
                    .zip(entry.bone_states.iter());

                for (bone, state) in bones {
                    if (bone.flags & BoneFlags::FlagHitbox as u32) == 0 {
                        continue;
                    }

                    let parent_index = if let Some(parent) = bone.parent {
                        parent
                    } else {
                        continue;
                    };

                    let parent_position = match self
                        .view_controller
                        .world_to_screen(&entry.bone_states[parent_index].position, true)
                    {
                        Some(position) => position,
                        None => continue,
                    };
                    let bone_position =
                        match self.view_controller.world_to_screen(&state.position, true) {
                            Some(position) => position,
                            None => continue,
                        };

                    draw.add_line(
                        parent_position,
                        bone_position,
                        ImColor32::from_rgb(0, 255, 255),
                    )
                    .build();
                }
            }

            if settings.esp_boxes {
                self.view_controller.draw_box_3d(
                    &draw,
                    &(entry.model.vhull_min + entry.position),
                    &(entry.model.vhull_max + entry.position),
                    ImColor32::from_rgb(255, 0, 255),
                    7.0
                );
            }

            if settings.bomb_timer {
                let group = ui.begin_group();

                let line_height = ui.text_line_height_with_spacing();
                ui.set_cursor_pos([ 10.0, ui.window_size()[1] * 0.95 - line_height * 5.0 ]); // ui.frame_height() - line_height * 5.0 

                match &self.bomb_state {
                    BombState::Unset => {},
                    BombState::Active { bomb_site, time_detonation, defuse } => {
                        ui.text(&format!("Bomb planted on {}", if *bomb_site == 0 { "A" } else { "B" }));
                        ui.text(&format!("Damage:"));
                        ui.same_line();
                        ui.text_colored([ 0.0, 0.0, 0.0, 0.0 ], "???");
                        ui.text(&format!("Time: {:.3}", time_detonation));
                        if let Some(defuse) = defuse.as_ref() {
                            let color = if defuse.time_remaining > *time_detonation {
                                [ 0.79, 0.11, 0.11, 1.0 ]
                            } else {
                                [ 0.11, 0.79, 0.26, 1.0 ]
                            };

                            ui.text_colored(color, &format!("Defused in {:.3} by {}", defuse.time_remaining, defuse.player_name));
                        } else {
                            ui.text("Not defusing");
                        }
                    },
                    BombState::Defused => {
                        ui.text("Bomb has been defused");
                    },
                    BombState::Detonated => {
                        ui.text("Bomb has been detonated");
                    }
                }

                group.end();
            }
        }
    }

    fn render_settings(&self, ui: &imgui::Ui) {
        ui.window(obfstr!("Valthrun"))
            .size([600.0, 300.0], Condition::FirstUseEver)
            .build(|| {
                ui.text(obfstr!("Valthrun an open source CS2 external read only kernel cheat."));
                ui.separator();

                let mut settings = self.settings.borrow_mut();
                ui.checkbox(obfstr!("Player List"), &mut settings.player_list);
                ui.checkbox(obfstr!("Player Position Dots"), &mut settings.player_pos_dot);
                ui.checkbox(obfstr!("ESP Boxes"), &mut settings.esp_boxes);
                ui.checkbox(obfstr!("ESP Skeletons"), &mut settings.esp_skeleton);
                ui.checkbox(obfstr!("Bomb Timer"), &mut settings.bomb_timer);
            });
    }
}

fn show_critical_error(message: &str) {
    log::error!("{}", message);

    if !is_console_invoked() {
        overlay::show_error_message(obfstr!("Valthrun Controller"), message);
    }
}

fn main() {
    let args = match AppArgs::try_parse() {
        Ok(args) => args,
        Err(error) => {
            println!("{:#}", error);
            std::process::exit(1);
        }
    };

    env_logger::builder()
        .filter_level(if args.verbose { log::LevelFilter::Trace } else { log::LevelFilter::Info })
        .parse_default_env()
        .init();

    let command = args.command.as_ref().unwrap_or(&AppCommand::Overlay);
    let result = match command {
        AppCommand::DumpSchema(args) => main_schema_dump(args),
        AppCommand::Overlay => main_overlay()
    };
    
    if let Err(error) = result {
        show_critical_error(&format!("{:#}", error));
    }
}

#[derive(Debug, Parser)]
#[clap(name = "Valthrun", version)]
struct AppArgs {
    /// Enable verbose logging ($env:RUST_LOG="trace")
    #[clap(short, long)]
    verbose: bool,

    #[clap(subcommand)]
    command: Option<AppCommand>
}

#[derive(Debug, Subcommand)]
enum AppCommand {
    /// Start the overlay
    Overlay,

    /// Create a schema dump
    DumpSchema(SchemaDumpArgs)
}

#[derive(Debug, Args)]
struct SchemaDumpArgs {
    pub target_file: PathBuf,
}

fn is_console_invoked() -> bool {
    let console_count = unsafe { 
        let mut result = [0u32; 128];
        GetConsoleProcessList(&mut result)
    };

    console_count > 1
}

fn main_schema_dump(args: &SchemaDumpArgs) -> anyhow::Result<()> {
    log::info!("Dumping schema. Please wait...");

    let cs2 = CS2Handle::create()?;
    let schema = cs2::dump_schema(&cs2)?;

    let output = File::options()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&args.target_file)?;

    let mut output = BufWriter::new(output);
    serde_json::to_writer_pretty(&mut output, &schema)?;
    log::info!("Schema dumped to {}", args.target_file.to_string_lossy());
    Ok(())
}

fn main_overlay() -> anyhow::Result<()> {
    let settings = load_app_settings()?;

    let cs2 = Arc::new(CS2Handle::create()?);
    let cs2_offsets = Arc::new(
        CS2Offsets::resolve_offsets(&cs2)
            .with_context(|| obfstr!("failed to load CS2 offsets").to_string())?
    );

    let imgui_settings = settings.imgui.clone();
    let app = Application {
        cs2: cs2.clone(),
        cs2_entities: EntitySystem::new(cs2.clone(), cs2_offsets.clone()),
        cs2_offsets: cs2_offsets.clone(),
        cs2_globals: Globals::default(),

        settings_visible: false,

        players: Vec::with_capacity(16),
        model_cache: EntryCache::new({
            let cs2 = cs2.clone();
            move |model| {
                let model_name = cs2.read_string(Module::Absolute, &[*model as u64 + 0x08, 0], Some(32))?;
                log::debug!("{} {}. Caching.", obfstr!("Discovered new player model"), model_name);
    
                Ok(CS2Model::read(&cs2, *model as u64)?)
            }
        }),
        class_name_cache: EntryCache::new({
            let cs2 = cs2.clone();
            move |vtable: &u64| {
                let fn_get_class_schema = cs2.read::<u64>(Module::Absolute, &[
                    *vtable + 0x00, // First entry in V-Table is GetClassSchema
                ])?;

                let schema_offset = cs2.read::<i32>(Module::Absolute, &[
                    fn_get_class_schema + 0x03, // lea rcx, <class schema>
                ])? as u64;

                let class_schema = fn_get_class_schema
                    .wrapping_add(schema_offset)
                    .wrapping_add(0x07);

                if !cs2.module_address(Module::Client, class_schema).is_some() {
                    /* Class defined in other module. GetClassSchema function might be implemented diffrently. */
                    return Ok(None);
                }

                let class_name = cs2.read_string(Module::Absolute, &[
                    class_schema + 0x08,
                    0
                ], Some(32))?;
                Ok(Some(class_name))
            }
        }),

        view_controller: ViewController::new(cs2_offsets.clone()),
        bomb_state: BombState::Unset,

        last_total_read_calls: 0,
        frame_read_calls: 0,

        settings: RefCell::new(settings),
        settings_dirty: false,
    };

    let app = Rc::new(RefCell::new(app));
    
    let mut overlay = overlay::init(obfstr!("CS2 Overlay"), obfstr!("Counter-Strike 2"))?;
    if let Some(imgui_settings) = imgui_settings {
        overlay.imgui.load_ini_settings(&imgui_settings);
    }

    overlay.main_loop(
        {
            let app = app.clone();
            move |context| {
                let mut app = app.borrow_mut();
                if let Err(err) = app.pre_update(context) {
                    show_critical_error(&format!("{:#}", err));
                    false
                } else {
                    true    
                }            
            }
        },
        move |ui| {
            let mut app = app.borrow_mut();

            if let Err(err) = app.update(ui) {
                show_critical_error(&format!("{:#}", err));
                return false;
            }

            app.render(ui);
            true
        }
    )
}